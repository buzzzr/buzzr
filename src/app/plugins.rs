//! The plugin side of `App`: contributions, plugin events and prompts, panels, and the projection plugins are shown.
//!
//! A second `impl App` block: the state lives on `App` in `app.rs`, and what is here is only the behaviour that reads and
//! changes it, so `app.rs` stays the one place to see what `App` holds. Private methods are `pub(super)` so `app.rs` and the
//! other feature modules can call them.

use super::*;

impl App {
    /// Take the compiled-in plugins this build registered and start them: apply what each declared when it was ready,
    /// and remember it for the permission checks and `/plugins`. The loop calls this once, before the first event.
    pub fn seed_plugins(&mut self, runtime: plugin::PluginRuntime) {
        self.internal = runtime;
        let declared = self.internal.ready();
        for (id, d) in declared {
            self.plugin_ready(&id, d);
        }
    }

    /// Apply one plugin's `ready`: its contributions join the registry, its permissions are remembered, and it shows in
    /// `/plugins` beside the external ones.
    pub(super) fn plugin_ready(&mut self, id: &str, mut declared: plugin::Declared) {
        let name = if declared.name.is_empty() { id.to_owned() } else { declared.name.clone() };
        // The same filter the wire applies to an external plugin's `ready`: a plugin only holds a permission this build
        // enforces, so both kinds are granted from the same list (`plugins::discover::KNOWN_PERMISSIONS`).
        declared.permissions.retain(|p| crate::plugins::discover::KNOWN_PERMISSIONS.contains(&p.as_str()));
        declared.permissions.sort();
        declared.permissions.dedup();
        self.plugins.set(id, &name, declared.contributions.clone());
        self.internal_declared.insert(id.to_owned(), declared);
        self.content_changed();
        self.refresh_plugin_panel();
    }

    /// Show the compiled-in plugins the open view — but only when it changed, the same "only what changed" rule the
    /// host follows for an external one. Called by the loop after every event.
    pub fn plugin_snapshot(&mut self, projection: Projection) -> Vec<Cmd> {
        if self.internal.is_empty() || self.pushed_projection.as_ref() == Some(&projection) {
            return Vec::new();
        }
        self.pushed_projection = Some(projection.clone());
        // A plugin sees the view only while it holds `read:visible` — the same gate the host applies to an external one.
        let ids: Vec<String> = self.internal.ids().iter().map(|s| (*s).to_owned()).collect();
        let mut outs = Vec::new();
        for id in ids {
            if !self.plugin_may(&id, plugin::READ_VISIBLE) {
                continue;
            }
            for out in self.internal.on_for(&id, PluginInput::Snapshot(projection.clone())) {
                outs.push((id.clone(), out));
            }
        }
        self.apply_tagged_outputs(outs)
    }

    /// Give a heartbeat to every compiled-in plugin whose own code asked for one and is due. Called on the tick.
    pub fn plugin_tick(&mut self) -> Vec<Cmd> {
        if self.internal.is_empty() {
            return Vec::new();
        }
        let now = Instant::now();
        let ids: Vec<String> = self.internal.ids().iter().map(|s| (*s).to_owned()).collect();
        let mut due = Vec::new();
        for id in ids {
            let Some(ms) = self.internal.tick_ms(&id) else { continue };
            let last = self.plugin_ticks.entry(id.clone()).or_insert(now);
            if now.duration_since(*last) >= Duration::from_millis(ms) {
                *last = now;
                due.push(id);
            }
        }
        let mut cmds = Vec::new();
        for id in due {
            let outs = self.internal.on_for(&id, PluginInput::Tick);
            cmds.extend(self.apply_outputs(&id, outs));
        }
        cmds
    }

    /// Hand one input to one plugin: in-process for a compiled-in plugin, or out to the host for an external one.
    pub(super) fn plugin_input(&mut self, plugin: &str, input: PluginInput) -> Vec<Cmd> {
        if self.internal.has(plugin) {
            let outs = self.internal.on_for(plugin, input);
            self.apply_outputs(plugin, outs)
        } else {
            match plugin_cmd(plugin, input) {
                Some(cmd) => vec![Cmd::Plugin(cmd)],
                None => Vec::new(),
            }
        }
    }

    /// Apply one plugin's outputs, in order, through the same door an external plugin's events come through
    /// (`plugin_event`), so a compiled-in plugin is held to exactly the same rules.
    pub(super) fn apply_outputs(&mut self, plugin: &str, outputs: Vec<PluginOutput>) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        for out in outputs {
            cmds.extend(self.plugin_event(out.into_event(plugin)));
        }
        cmds
    }

    pub(super) fn apply_tagged_outputs(&mut self, outputs: Vec<(String, PluginOutput)>) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        for (id, out) in outputs {
            cmds.extend(self.plugin_event(out.into_event(&id)));
        }
        cmds
    }

    /// Whether a plugin was granted a permission. A compiled-in plugin's declaration is kept here; an external one's
    /// comes back with its `Ready`, in `plugin_meta`.
    pub(super) fn plugin_may(&self, plugin: &str, permission: &str) -> bool {
        if let Some(d) = self.internal_declared.get(plugin) {
            return d.permissions.iter().any(|p| p == permission);
        }
        self.plugin_meta.iter().find(|m| m.id == plugin).map(|m| m.permissions.iter().any(|p| p == permission)).unwrap_or(false)
    }

    /// The per-plugin send limit both kinds share (the same numbers the host applies to an external one).
    pub(super) fn allow_send(&mut self, plugin: &str) -> bool {
        let now = Instant::now();
        let times = self.plugin_sends.entry(plugin.to_owned()).or_default();
        times.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        if times.len() >= plugin::SENDS_PER_MINUTE || times.last().map(|t| now.duration_since(*t) < plugin::SEND_INTERVAL).unwrap_or(false) {
            return false;
        }
        times.push(now);
        true
    }

    // ---- plugin prompts (a plugin asks buzzr to ask the user) --------------------------------------------------------

    /// Put a prompt a plugin asked for in the composer. The box takes over from whatever was typed in it, the way an
    /// edit does, so a half-written message is stored and comes back when the prompt ends.
    pub(super) fn begin_prompt(&mut self, plugin: String, id: String, title: String, hint: String, multiline: bool) -> Vec<Cmd> {
        let cmds = self.stash_draft();
        self.editor.clear();
        // The prompt lives in the composer, so whatever was covering it gets out of the way — the same way starting a
        // flow from a panel row closes the panel. Without this a prompt a plugin asked for from a panel would be drawn
        // under the panel, and the user could not see what they are answering.
        self.overlay = Overlay::None;
        self.panel = None;
        self.prompt = Some(PromptState { plugin, id, title, hint: hint.clone(), multiline });
        self.set_focus(Focus::Input);
        self.flash(hint);
        cmds
    }

    /// A key while a plugin's prompt is up: Esc cancels it, Enter accepts it, and everything else edits the box.
    pub(super) fn key_prompt(&mut self, k: KeyEvent) -> Vec<Cmd> {
        self.set_focus(Focus::Input);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Esc => self.finish_prompt(None),
            // A newline inside a block-of-text prompt is Shift+Enter or Alt+Enter, as anywhere in the composer; Enter
            // accepts.
            KeyCode::Enter if !shift && !alt => self.finish_prompt(Some(self.editor.text())),
            // Up and Down move the cursor inside the box here, rather than leaving for the message list.
            KeyCode::Up => {
                self.editor.up(self.input_width());
                Vec::new()
            }
            KeyCode::Down => {
                self.editor.down(self.input_width());
                Vec::new()
            }
            _ => self.key_input(k, ctrl, alt, shift),
        }
    }

    /// Answer (or cancel, with `None`) the prompt that is up, and give the composer back to what it was writing.
    pub(super) fn finish_prompt(&mut self, text: Option<String>) -> Vec<Cmd> {
        let Some(prompt) = self.prompt.take() else { return Vec::new() };
        self.editor.clear();
        let mut cmds = self.plugin_input(&prompt.plugin, PluginInput::Prompt { id: prompt.id, text });
        cmds.extend(self.switch_draft()); // the box belonged to the prompt; it goes back to its draft
        cmds
    }


    /// A plugin's display name for a message, falling back to its id when it is not registered.
    pub(super) fn plugin_name(&self, id: &str) -> String {
        self.plugins.name_of(id).unwrap_or(id).to_owned()
    }

    /// The local agents as a plugin may see them: the words and the public identity, never a key (see `plugin::AgentView`).
    pub(super) fn agent_views(&self) -> Vec<plugin::AgentView> {
        self.agents
            .iter()
            .map(|a| plugin::AgentView {
                name: a.name.clone(),
                pubkey: a.pubkey.clone(),
                npub: relay::npub_of(&a.pubkey).unwrap_or_else(|| a.pubkey.clone()),
                harness: a.harness.clone(),
                model: a.model.clone(),
                effort: a.effort.clone(),
                workspace: a.workspace.clone(),
                instructions: a.instructions.clone(),
                created_at: a.created_at,
            })
            .collect()
    }

    pub(super) fn plugin_event(&mut self, ev: PluginEvent) -> Vec<Cmd> {
        match ev {
            PluginEvent::Ready { id, name, contributions } => {
                self.plugins.set(&id, &name, contributions);
                self.content_changed();
                self.refresh_plugin_panel();
                Vec::new()
            }
            PluginEvent::Meta(meta) => {
                self.plugin_meta = meta;
                self.refresh_plugin_panel();
                Vec::new()
            }
            PluginEvent::Status { id, segments } => {
                // A status for a plugin that is not registered (gone, or not ready) is dropped: the plugin list says why.
                self.plugins.set_status(&id, segments);
                Vec::new()
            }
            PluginEvent::Pane { id, pane } => {
                // Same rule as status: a pane for a plugin that is not registered is dropped, and the next draw shows it.
                self.plugins.set_pane(&id, pane);
                Vec::new()
            }
            PluginEvent::Panel { plugin, panel } => {
                self.open_panel(plugin, panel);
                Vec::new()
            }
            PluginEvent::Flash { text } => {
                self.flash(text);
                Vec::new()
            }
            PluginEvent::Draft { plugin, text } => {
                let name = self.plugin_name(&plugin);
                if self.read_only {
                    self.flash(format!("read-only mode: {name} cannot put a draft in the input"));
                } else if !self.editor.is_empty() || self.editing.is_some() {
                    self.flash(format!("{name} sent a draft, but you are already writing; it was dropped"));
                } else {
                    self.editor.insert_str(&text);
                    self.set_focus(Focus::Input);
                    self.flash(format!("{name} put a draft in the input: read it, then Enter sends it"));
                }
                Vec::new()
            }
            PluginEvent::Send { plugin, channel, text } => {
                let name = self.plugin_name(&plugin);
                if self.read_only {
                    self.flash("read-only mode: nothing is sent");
                    return Vec::new();
                }
                // An external plugin's send was already permission-checked and rate-limited by the host; a compiled-in
                // plugin gets the same gate here, so the transport never changes what a plugin may do.
                if self.internal.has(&plugin) {
                    if !self.plugin_may(&plugin, plugin::MESSAGES_WRITE) {
                        self.flash(format!("{name} needs the messages:write permission to send"));
                        return Vec::new();
                    }
                    if !self.allow_send(&plugin) {
                        self.flash(format!("{name} is sending too fast"));
                        return Vec::new();
                    }
                }
                let Some(open) = self.channel().map(|c| c.id.clone()) else {
                    self.flash(format!("{name} wanted to send, but no channel is open"));
                    return Vec::new();
                };
                if channel.as_deref().map(|c| c != open).unwrap_or(false) {
                    self.flash(format!("{name} wanted to send in another channel; plugins may only write in the open one"));
                    return Vec::new();
                }
                self.flash(format!("a message from {name}"));
                self.send(text, None)
            }
            PluginEvent::Prompt { plugin, id, title, hint, multiline } => {
                // A prompt is the host's to draw and own, but only one at a time: a second one while a prompt is up is
                // refused rather than clobbering what the user is already answering.
                if self.prompt.is_some() {
                    self.flash(format!("{} asked for a prompt, but one is already up", self.plugin_name(&plugin)));
                    return Vec::new();
                }
                self.begin_prompt(plugin, id, title, hint, multiline)
            }
            PluginEvent::ListAgents { plugin, id } => {
                if !self.plugin_may(&plugin, plugin::AGENTS_READ) {
                    self.flash(format!("{} is not allowed to read the local agents", self.plugin_name(&plugin)));
                    return Vec::new();
                }
                let agents = self.agent_views();
                self.plugin_input(&plugin, PluginInput::Agents { id, agents })
            }
            PluginEvent::ProbeHarness { plugin, id, harness, model } => {
                // The harness is buzzr's to start (principle 8: no plugin code in the core — and no process either), so
                // the ask comes here and the answer goes back as `AppEvent::HarnessOffer`. Refusing it in read-only mode
                // is the first half of "an agent is not created"; the second is the same guard on `CreateAgent`.
                if self.read_only {
                    self.flash("read-only mode: agents are not created");
                    return Vec::new();
                }
                if !self.plugin_may(&plugin, plugin::AGENTS_WRITE) {
                    self.flash(format!("{} is not allowed to create agents", self.plugin_name(&plugin)));
                    return Vec::new();
                }
                // One probe at a time: the flow asks for one look at a time, so a second while one is in flight is
                // refused rather than letting its answer clobber the first.
                if self.pending_probe.is_some() {
                    self.flash(format!("{} asked for a harness check, but one is already running", self.plugin_name(&plugin)));
                    return Vec::new();
                }
                self.pending_probe = Some((plugin, id));
                vec![Cmd::ProbeHarness { harness, model }]
            }
            PluginEvent::CreateAgent { plugin, id, name, instructions, harness, model, effort } => {
                if self.read_only {
                    self.flash("read-only mode: nothing is written");
                    return Vec::new();
                }
                if !self.plugin_may(&plugin, plugin::AGENTS_WRITE) {
                    self.flash(format!("{} is not allowed to create agents", self.plugin_name(&plugin)));
                    return Vec::new();
                }
                // The result comes back as `AppEvent::AgentCreated`; the plugin that asked is answered with it.
                self.pending_create = Some((plugin, id));
                vec![Cmd::CreateAgent { name, instructions, harness, model, effort }]
            }
            PluginEvent::DeleteAgent { plugin, id, pubkey } => {
                if self.read_only {
                    self.flash("read-only mode: nothing is written");
                    return Vec::new();
                }
                if !self.plugin_may(&plugin, plugin::AGENTS_WRITE) {
                    self.flash(format!("{} is not allowed to delete agents", self.plugin_name(&plugin)));
                    return Vec::new();
                }
                // One deletion at a time, like a creation: a second while one is in flight is refused rather than
                // letting its answer clobber the first.
                if self.pending_delete.is_some() {
                    self.flash(format!("{} asked to delete an agent, but one is already being deleted", self.plugin_name(&plugin)));
                    return Vec::new();
                }
                // The result comes back as `AppEvent::AgentDeleted`; the plugin that asked is answered with it.
                self.pending_delete = Some((plugin, id));
                vec![Cmd::DeleteAgent { pubkey }]
            }
            PluginEvent::UpdateAgent { plugin, id, pubkey, name, instructions } => {
                if self.read_only {
                    self.flash("read-only mode: nothing is written");
                    return Vec::new();
                }
                if !self.plugin_may(&plugin, plugin::AGENTS_WRITE) {
                    self.flash(format!("{} is not allowed to change agents", self.plugin_name(&plugin)));
                    return Vec::new();
                }
                if self.pending_update.is_some() {
                    self.flash(format!("{} asked to change an agent, but one is already being changed", self.plugin_name(&plugin)));
                    return Vec::new();
                }
                // The result comes back as `AppEvent::AgentUpdated`; the plugin that asked is answered with it.
                self.pending_update = Some((plugin, id));
                vec![Cmd::UpdateAgent { pubkey, name, instructions }]
            }
            PluginEvent::Gone { id, why } => {
                self.plugins.remove(&id);
                self.content_changed();
                self.refresh_plugin_panel();
                self.flash(format!("plugin {id} stopped: {why}"));
                Vec::new()
            }
            PluginEvent::Log { id, level: _, text } => {
                self.flash(format!("plugin {id}: {text}"));
                Vec::new()
            }
        }
    }

    /// The `plugins` overlay: what is on disk, what is running, what it may do, and the commands it added.
    pub(super) fn show_plugins(&mut self) {
        let mut rows = Vec::new();
        let builtin: Vec<String> = self.internal.ids().iter().map(|s| (*s).to_owned()).collect();
        if builtin.is_empty() && self.plugin_meta.is_empty() {
            rows.push(PanelRow { text: "No plugins found. Put a folder with an executable entry file in the plugins directory.".into(), tone: contrib::Tone::Dim, action: None });
        }
        if !builtin.is_empty() {
            rows.push(PanelRow { text: "Compiled in".into(), tone: contrib::Tone::Bold, action: None });
            for id in &builtin {
                let (perms, commands) = match self.internal_declared.get(id) {
                    Some(d) => (
                        if d.permissions.is_empty() { String::new() } else { format!(" · {}", d.permissions.join(", ")) },
                        d.contributions.commands.len(),
                    ),
                    None => (String::new(), 0),
                };
                rows.push(PanelRow {
                    text: format!("{:<18} {:<9} {} command(s){perms}", id, "built in", commands),
                    tone: contrib::Tone::Ok,
                    action: None,
                });
            }
            if !self.plugin_meta.is_empty() {
                rows.push(PanelRow { text: String::new(), tone: contrib::Tone::Default, action: None });
                rows.push(PanelRow { text: "Loaded from disk".into(), tone: contrib::Tone::Bold, action: None });
            }
        }
        for m in &self.plugin_meta {
            let tone = match m.state {
                PluginState::Running => contrib::Tone::Ok,
                PluginState::Failed => contrib::Tone::Error,
                PluginState::Disabled | PluginState::Discovered => contrib::Tone::Dim,
                PluginState::Starting => contrib::Tone::Warn,
            };
            let perms = if m.permissions.is_empty() { String::new() } else { format!(" · {}", m.permissions.join(", ")) };
            let detail = if m.detail.is_empty() { String::new() } else { format!(" · {}", m.detail) };
            rows.push(PanelRow {
                text: format!("{:<18} {:<9} {} command(s){perms}{detail}", m.id, m.state.label(), m.commands),
                tone,
                action: None,
            });
        }
        let commands = self.plugins.commands();
        if !commands.is_empty() {
            rows.push(PanelRow { text: String::new(), tone: contrib::Tone::Default, action: None });
            rows.push(PanelRow { text: "Commands plugins added".into(), tone: contrib::Tone::Bold, action: None });
            for (plugin, c) in commands {
                let usage = if c.usage.is_empty() { String::new() } else { format!("   {}", c.usage) };
                rows.push(PanelRow {
                    text: format!("{:<14} {}{usage}   ({plugin})", c.id, if c.title == c.id { "" } else { &c.title }),
                    tone: contrib::Tone::Default,
                    action: None,
                });
            }
        }
        rows.push(PanelRow { text: String::new(), tone: contrib::Tone::Default, action: None });
        rows.push(PanelRow {
            text: "Put a folder with an executable entry file (its shebang picks the language) in the plugins directory; the `plugins` command shows what loaded.".into(),
            tone: contrib::Tone::Dim,
            action: None,
        });
        self.panel = Some(PanelState { plugin: String::new(), id: "plugins".into(), title: "Plugins".into(), rows, cursor: 0 });
        self.overlay = Overlay::Panel;
    }

    /// Keep the `plugins` overlay current while it is open.
    pub(super) fn refresh_plugin_panel(&mut self) {
        let managed = self.panel.as_ref().map(|p| p.plugin.is_empty()).unwrap_or(false);
        if self.overlay != Overlay::Panel || !managed {
            return;
        }
        let cursor = self.panel.as_ref().map(|p| p.cursor).unwrap_or(0);
        self.show_plugins();
        if let Some(p) = self.panel.as_mut() {
            p.cursor = cursor.min(p.rows.len().saturating_sub(1));
        }
    }

    pub(super) fn open_panel(&mut self, plugin: String, panel: contrib::Panel) {
        let title = if panel.title.is_empty() { plugin.clone() } else { panel.title };
        self.panel = Some(PanelState { plugin, id: panel.id, title, rows: panel.rows, cursor: 0 });
        self.overlay = Overlay::Panel;
        self.flash("panel: Up/Down to move, Enter runs the row, Esc closes");
    }

    pub(super) fn key_panel(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let n = self.panel.as_ref().map(|p| p.rows.len()).unwrap_or(0);
        match k.code {
            KeyCode::Esc => {
                self.overlay = Overlay::None;
                self.panel = None;
            }
            KeyCode::Up => self.panel_move(-1),
            KeyCode::Down => self.panel_move(1),
            KeyCode::PageUp => self.panel_move(-8),
            KeyCode::PageDown => self.panel_move(8),
            KeyCode::Home => {
                if let Some(p) = self.panel.as_mut() {
                    p.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Some(p) = self.panel.as_mut() {
                    p.cursor = n.saturating_sub(1);
                }
            }
            KeyCode::Enter => {
                let Some(p) = self.panel.as_ref() else { return Vec::new() };
                if p.plugin.is_empty() {
                    return Vec::new(); // the built-in list has nothing to run
                }
                let Some(action) = p.rows.get(p.cursor).and_then(|r| r.action.clone()) else { return Vec::new() };
                let (plugin, id, row) = (p.plugin.clone(), p.id.clone(), p.cursor);
                return self.plugin_input(&plugin, PluginInput::PanelAction { id, row, value: Some(action) });
            }
            _ => {}
        }
        Vec::new()
    }

    pub(super) fn panel_move(&mut self, delta: isize) {
        let Some(p) = self.panel.as_mut() else { return };
        let n = p.rows.len();
        p.cursor = if delta < 0 { p.cursor.saturating_sub((-delta) as usize) } else { (p.cursor + delta as usize).min(n.saturating_sub(1)) };
    }

    /// What a plugin is told: the open view, the rows on screen, and nothing else (no key, no relay, no other channel).
    pub fn projection(&self) -> Projection {
        let connection = match &self.conn {
            Conn::Live => "live".to_owned(),
            Conn::Connecting => "connecting".to_owned(),
            Conn::Down(why) => format!("offline: {why}"),
        };
        let mut messages: Vec<contrib::MessageView> = Vec::new();
        if let (Some(store), Some((_, layout))) = (self.store(), self.layout.as_ref()) {
            let mut seen = HashSet::new();
            for line in self.state.scroll..self.state.scroll + self.view_h.max(1) {
                let Some((i, _)) = layout.at_line(line) else { continue };
                let id = &layout.items[i].id;
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Some(m) = store.msgs.get(id) else { continue };
                let (text, _) = store.content_of(m);
                let (replies, _) = store.replies_of(m);
                messages.push(contrib::MessageView {
                    id: m.id.clone(),
                    author: self.name_of(&m.pubkey),
                    pubkey: m.pubkey.clone(),
                    at: m.created_at,
                    text: contrib::clean_block(text),
                    replies,
                    selected: self.state.selected.as_deref() == Some(m.id.as_str()),
                });
            }
        }
        Projection {
            me: self.me.clone(),
            connection,
            channel: self.channel().map(|c| contrib::ChannelView { id: c.id.clone(), label: c.label.clone() }),
            view: if self.in_thread() { "thread" } else { "channel" }.to_owned(),
            selected: self.state.selected.clone(),
            messages,
            unread: self.total_unread(),
        }
    }
}
