//! The plugin API: one vocabulary for compiled-in and runtime-loaded plugins.
//!
//! buzzr has exactly two kinds of plugin, and nothing between them. A **compiled-in** plugin is a Rust type in this tree
//! (or in a crate outside it) that registers itself with [`register_plugin!`]; a **runtime-loaded** plugin is an
//! executable the host starts and speaks newline-delimited JSON with (see `PLUGIN.md`). Both are handed the same
//! [`PluginInput`], answer with the same [`PluginOutput`], declare the same [`Declared`] when they are ready, and are
//! held to the same permissions and the same screen rules — a compiled-in plugin gets no door an external one lacks, and
//! it never draws, never sees the key and never touches the relay.
//!
//! The two kinds differ only in *transport*: a compiled-in plugin is a value driven in this process, an external one is
//! a child process on the far side of the wire in `plugins::proto`. Everything the transport adds (the JSON codec, the
//! handshake, the heartbeat, the permission check) is described once, against this module.
//!
//! Nothing a plugin returns reaches the screen unchecked: the same cleaning and clipping that guards the wire guards a
//! compiled-in plugin too (see `contrib`), and the same application path handles both (see `App::plugin_event`).

use crate::contrib::{clean_block, clean_line, Contributions, Panel, Pane, PluginCommand, PluginEvent, Projection, Segment};

/// Re-exported so `register_plugin!` can name `inventory` through `$crate` when it is called from another crate.
pub use inventory;

/// Permission: the plugin may be handed the bounded [`Projection`] of what is on screen.
pub const READ_VISIBLE: &str = "read:visible";
/// Permission: the plugin's `Send` is honoured (only ever in the open channel, rate-limited).
pub const MESSAGES_WRITE: &str = "messages:write";
/// Permission: the plugin may be told which local agents exist (`AgentView`, never a key).
pub const AGENTS_READ: &str = "agents:read";
/// Permission: the plugin's `CreateAgent` is honoured (it makes a new local identity and publishes it).
pub const AGENTS_WRITE: &str = "agents:write";

/// The send limit both kinds share: at most this many sends from one plugin in a minute, and never two closer together
/// than `SEND_INTERVAL`. The host applies these to an external plugin; the app applies the same numbers to a compiled-in
/// one, so a plugin's transport never changes how fast it may talk.
pub const SENDS_PER_MINUTE: usize = 20;
pub const SEND_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Everything a plugin says about itself when it becomes ready: its display name, what it asks to be allowed to do, how
/// often it wants a heartbeat (0 = never), and what it puts on screen to begin with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declared {
    pub name: String,
    pub permissions: Vec<String>,
    pub tick_ms: u64,
    pub contributions: Contributions,
}

/// One local agent as a plugin may see it: the identity it speaks with and where it works. There is no key here — that
/// stays in the record file, which only the host reads (see `agents.rs`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct AgentView {
    pub name: String,
    /// The agent's public key, hex.
    pub pubkey: String,
    /// The same key as `npub1…`, for display.
    pub npub: String,
    /// What runs it: `claude-code-acp`.
    pub harness: String,
    /// The folder the harness works in.
    pub workspace: String,
    pub created_at: u64,
}

/// An agent the host made, as the answer to [`PluginOutput::CreateAgent`]: the identity it speaks with. The relay is
/// asked first, so there is no "made but not published" state — a relay that refuses the agent means it was not made,
/// and that comes back as the `Err` of [`PluginInput::Created`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CreatedAgent {
    pub name: String,
    pub pubkey: String,
    /// The same key as `npub1…`, for display.
    pub npub: String,
}

/// What buzzr hands a plugin: a fresh snapshot of what is on screen, a heartbeat, a command the user ran, the answer to
/// a panel row the user picked, the answer to a prompt the plugin asked for, the agent list it asked for, or the result
/// of a creation it asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginInput {
    /// What the open view looks like — only while the plugin holds [`READ_VISIBLE`].
    Snapshot(Projection),
    /// The heartbeat the plugin asked for with `tick_ms`.
    Tick,
    /// `/command args` from the input box.
    Command { command: String, args: String },
    /// Enter on a panel row that has an action.
    PanelAction { id: String, row: usize, value: Option<String> },
    /// The text the user typed into a prompt the plugin asked for (see [`PluginOutput::Prompt`]); `None` when the user
    /// cancelled it with Esc.
    Prompt { id: String, text: Option<String> },
    /// The local agents, as the answer to [`PluginOutput::ListAgents`].
    Agents { id: String, agents: Vec<AgentView> },
    /// The answer to [`PluginOutput::CreateAgent`]: the agent that was made, or why it was not.
    Created { id: String, result: Result<CreatedAgent, String> },
}

/// What a plugin asks buzzr to do — the same surface an external plugin has on the wire.
///
/// This is the whole vocabulary a plugin may speak, so it is deliberately wider than any one plugin uses: a compiled-in
/// plugin that never sends, or never sets a status, leaves those variants unconstructed here while the wire still offers
/// them to an external one. `allow(dead_code)` is for exactly that — the variants are the API, not dead weight.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginOutput {
    /// Replace this plugin's status item (left of the built-in status text).
    Status(Vec<Segment>),
    /// Replace (or clear, with `None`) the plugin's pane under the channel list.
    Pane(Option<Pane>),
    /// Replace the plugin's panel (a selectable list), opening it.
    Panel(Panel),
    Flash(String),
    Draft(String),
    Send { channel: Option<String>, text: String },
    Log { level: String, text: String },
    /// Ask the user for one line (`multiline == false`) or a block of text. buzzr draws and owns the prompt; the answer
    /// comes back as [`PluginInput::Prompt`] with this `id`. `id` is the plugin's own label, not a path.
    Prompt { id: String, title: String, hint: String, multiline: bool },
    /// Ask for the local agents. The answer comes back as [`PluginInput::Agents`] with this `id`.
    ListAgents { id: String },
    /// Make a local agent and publish it. The host owns the key, the folder and the relay, so the plugin asks; the
    /// answer comes back as [`PluginInput::Created`] with this `id`.
    CreateAgent { id: String, name: String, instructions: String },
}

impl PluginOutput {
    /// Turn one output into the `PluginEvent` the app applies, cleaning it on the way. This is the single door from a
    /// plugin's words to the screen, taken by both kinds, so an external plugin's wire text and a compiled-in plugin's
    /// value are held to exactly the same rules.
    pub fn into_event(self, id: &str) -> PluginEvent {
        match self {
            PluginOutput::Status(segments) => PluginEvent::Status { id: id.to_owned(), segments: clean_segments(segments) },
            PluginOutput::Pane(pane) => {
                let pane = pane.map(Pane::clean).filter(|p| !p.is_empty());
                PluginEvent::Pane { id: id.to_owned(), pane }
            }
            PluginOutput::Panel(panel) => PluginEvent::Panel { plugin: id.to_owned(), panel: clean_panel(panel) },
            PluginOutput::Flash(text) => PluginEvent::Flash { text: clean_line(&text) },
            PluginOutput::Draft(text) => PluginEvent::Draft { plugin: id.to_owned(), text: clean_block(&text) },
            PluginOutput::Send { channel, text } => PluginEvent::Send {
                plugin: id.to_owned(),
                channel: channel.map(|c| clean_line(&c)),
                text: clean_block(&text),
            },
            PluginOutput::Log { level, text } => PluginEvent::Log { id: id.to_owned(), level: clean_line(&level), text: clean_line(&text) },
            PluginOutput::Prompt { id: prompt, title, hint, multiline } => PluginEvent::Prompt {
                plugin: id.to_owned(),
                id: clean_line(&prompt),
                title: clean_line(&title),
                hint: clean_line(&hint),
                multiline,
            },
            PluginOutput::ListAgents { id: prompt } => PluginEvent::ListAgents { plugin: id.to_owned(), id: clean_line(&prompt) },
            PluginOutput::CreateAgent { id: prompt, name, instructions } => PluginEvent::CreateAgent {
                plugin: id.to_owned(),
                id: clean_line(&prompt),
                name: clean_line(&name),
                instructions: clean_block(&instructions),
            },
        }
    }
}

fn clean_segments(segments: Vec<Segment>) -> Vec<Segment> {
    segments.into_iter().take(32).map(Segment::clean).filter(|s| !s.text.is_empty()).collect()
}

fn clean_panel(mut panel: Panel) -> Panel {
    panel.title = clean_line(&panel.title);
    panel.rows.truncate(crate::contrib::MAX_ROWS);
    for row in &mut panel.rows {
        row.text = clean_line(&row.text);
        row.action = row.action.as_deref().map(|a| clean_line(a).chars().take(256).collect::<String>()).filter(|a| !a.is_empty());
    }
    panel.rows.retain(|r| !r.text.trim().is_empty());
    panel
}

/// The single contract a plugin implements, whatever kind it is.
///
/// `ready` is called once, when the plugin starts, and `on` for every input after that. Implementations are the plugin's
/// own business; everything that reaches the screen or the relay goes back through [`PluginOutput`].
pub trait Plugin: Send {
    /// Stable id: the folder name for an external plugin, the type's own name for a compiled-in one.
    fn id(&self) -> &str;
    /// What the plugin declares when it starts. Called once.
    fn ready(&self) -> Declared;
    /// Handle one input. Returned outputs are delivered in order.
    fn on(&mut self, input: PluginInput) -> Vec<PluginOutput>;
}

/// A compiled-in plugin's registration: how to build one. Registered by [`register_plugin!`], collected by `inventory`
/// across every crate linked into buzzr.
pub struct Registration {
    pub make: fn() -> Box<dyn Plugin>,
}

inventory::collect!(Registration);

/// Every compiled-in plugin, in registration order.
pub fn builtins() -> Vec<Box<dyn Plugin>> {
    inventory::iter::<Registration>.into_iter().map(|r| (r.make)()).collect()
}

/// A monomorphised `fn() -> Box<dyn Plugin>` for [`register_plugin!`]: a function item coerces to a plain function
/// pointer, which is what `inventory::submit!` needs in its const context (a closure would not).
pub fn boxed<T: Plugin + Default + 'static>() -> Box<dyn Plugin> {
    Box::new(T::default())
}

/// Register a compiled-in plugin. Call it once, at module scope, next to the type it names:
///
/// ```
/// # use buzzr::plugin::{Declared, Plugin, PluginInput, PluginOutput};
/// #[derive(Default)]
/// struct Clock;
/// impl Plugin for Clock {
///     fn id(&self) -> &str { "clock" }
///     fn ready(&self) -> Declared { Declared::default() }
///     fn on(&mut self, _input: PluginInput) -> Vec<PluginOutput> { Vec::new() }
/// }
/// buzzr::register_plugin!(Clock);
/// ```
///
/// The type must be `Default` — the registry builds one when buzzr starts — and hold no state that has to come from
/// outside: a plugin keeps what it needs in itself, across the inputs it is handed.
///
/// Within buzzr, `register_plugin!(Clock)` is enough. A crate outside the tree calls it the same way after depending on
/// the `buzzr` library; `inventory` collects submissions from every crate linked into the build, so no list in buzzr
/// has to name it. (See `src/lib.rs`.)
#[macro_export]
macro_rules! register_plugin {
    ($ty:ty) => {
        $crate::plugin::inventory::submit! {
            $crate::plugin::Registration { make: $crate::plugin::boxed::<$ty> }
        }
    };
}

/// The command a plugin contributes, as the palette shows it. A convenience for `Declared { contributions, .. }`.
pub fn command(id: &str, title: &str, usage: &str) -> PluginCommand {
    PluginCommand { id: id.to_owned(), title: title.to_owned(), usage: usage.to_owned() }
}

/// The compiled-in provider: the plugins `inventory` collected from this build (and any crate linked into it), held in
/// registration order. It is the counterpart of `plugins::Host` — that one supervises child processes, this one holds
/// values — and both are driven through the same [`PluginInput`]/[`PluginOutput`] contract.
#[derive(Default)]
pub struct PluginRuntime {
    plugins: Vec<Box<dyn Plugin>>,
    /// What each plugin declared, read once (the trait promises `ready` is called a single time).
    declared: Vec<(String, Declared)>,
}

impl PluginRuntime {
    /// Every plugin this build registered.
    pub fn new() -> PluginRuntime {
        Self::from_plugins(builtins())
    }

    /// A runtime over an explicit set — tests build one without touching the global registry.
    pub fn from_plugins(plugins: Vec<Box<dyn Plugin>>) -> PluginRuntime {
        let declared = plugins.iter().map(|p| (p.id().to_owned(), p.ready())).collect();
        PluginRuntime { plugins, declared }
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// The id of every plugin held, in order. A duplicate id is a programming error the registry cannot see; keep ids
    /// unique.
    pub fn ids(&self) -> Vec<&str> {
        self.plugins.iter().map(|p| p.id()).collect()
    }

    /// Whether a compiled-in plugin with this id is held.
    pub fn has(&self, id: &str) -> bool {
        self.plugins.iter().any(|p| p.id() == id)
    }

    /// What each plugin declared when it started, paired with its id. Read once, at startup.
    pub fn ready(&self) -> Vec<(String, Declared)> {
        self.declared.clone()
    }

    /// Hand one input to one plugin, by id. Unknown ids are ignored (a plugin that is not registered cannot speak).
    pub fn on_for(&mut self, id: &str, input: PluginInput) -> Vec<PluginOutput> {
        match self.plugins.iter_mut().find(|p| p.id() == id) {
            Some(plugin) => plugin.on(input),
            None => Vec::new(),
        }
    }

    /// The heartbeat interval one plugin asked for, or `None` when it is not registered or wants no heartbeat.
    pub fn tick_ms(&self, id: &str) -> Option<u64> {
        self.declared.iter().find(|(p, _)| p == id).map(|(_, d)| d.tick_ms).filter(|ms| *ms > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contrib::{PanelRow, Projection, Tone};

    struct Greeter;
    impl Plugin for Greeter {
        fn id(&self) -> &str {
            "greeter"
        }
        fn ready(&self) -> Declared {
            Declared { name: "Greeter".into(), contributions: Contributions { commands: vec![command("hi", "Say hi", "/hi")], ..Contributions::default() }, ..Declared::default() }
        }
        fn on(&mut self, input: PluginInput) -> Vec<PluginOutput> {
            match input {
                PluginInput::Command { .. } => vec![PluginOutput::Flash("hi\u{1b}[31m".into())],
                PluginInput::Prompt { id, text } => vec![PluginOutput::Log { level: "info".into(), text: format!("{id}={:?}", text) }],
                _ => Vec::new(),
            }
        }
    }

    #[test]
    fn a_runtime_built_from_an_explicit_set_answers_by_id() {
        let mut rt = PluginRuntime::from_plugins(vec![Box::new(Greeter)]);
        assert!(rt.has("greeter") && !rt.has("nope"));
        assert_eq!(rt.ids(), vec!["greeter"]);
        assert_eq!(rt.ready()[0].1.name, "Greeter");
        assert_eq!(rt.tick_ms("greeter"), None, "greeter asked for no heartbeat");
        // an input by id reaches that plugin; a command is what greeter answers
        let outs = rt.on_for("greeter", PluginInput::Command { command: "hi".into(), args: String::new() });
        assert_eq!(outs, vec![PluginOutput::Flash("hi\u{1b}[31m".into())]);
        assert!(rt.on_for("greeter", PluginInput::Tick).is_empty(), "greeter has nothing to say to a tick");
        assert!(rt.on_for("nobody", PluginInput::Tick).is_empty(), "an unknown id is ignored");
    }

    #[test]
    fn an_output_is_cleaned_when_it_becomes_an_event() {
        // The door from a plugin's words to the app: control characters never survive, and the id is stamped on.
        let ev = PluginOutput::Flash("a\u{1b}[31mb".into()).into_event("greeter");
        assert_eq!(ev, PluginEvent::Flash { text: "a[31mb".into() });

        let ev = PluginOutput::Panel(Panel {
            id: "agents".into(),
            title: "Agents\u{7}".into(),
            rows: vec![PanelRow { text: "one\u{1b}[2J".into(), tone: Tone::Default, action: Some("open:1".into()) }, PanelRow { text: "  ".into(), ..PanelRow::default() }],
        })
        .into_event("greeter");
        let PluginEvent::Panel { plugin, panel } = ev else { panic!("not a panel") };
        assert_eq!(plugin, "greeter");
        assert_eq!(panel.title, "Agents");
        assert_eq!(panel.rows.len(), 1, "a blank row is dropped");
        assert_eq!(panel.rows[0].text, "one[2J");

        let ev = PluginOutput::CreateAgent { id: "new".into(), name: "Scout".into(), instructions: "a\nb".into() }.into_event("agents");
        assert_eq!(
            ev,
            PluginEvent::CreateAgent { plugin: "agents".into(), id: "new".into(), name: "Scout".into(), instructions: "a\nb".into() }
        );
    }

    #[test]
    fn a_prompt_and_a_list_ask_carry_their_id() {
        let ev = PluginOutput::Prompt { id: "name".into(), title: "new agent".into(), hint: "a name".into(), multiline: false }.into_event("agents");
        assert_eq!(ev, PluginEvent::Prompt { plugin: "agents".into(), id: "name".into(), title: "new agent".into(), hint: "a name".into(), multiline: false });
        let ev = PluginOutput::ListAgents { id: "list".into() }.into_event("agents");
        assert_eq!(ev, PluginEvent::ListAgents { plugin: "agents".into(), id: "list".into() });
    }

    #[test]
    fn the_projection_is_the_only_thing_a_plugin_sees_of_the_view() {
        // A compile-time guard on the shape: a plugin is handed a bounded snapshot and nothing else.
        let p = Projection { me: "me".into(), connection: "live".into(), view: "channel".into(), ..Projection::default() };
        let input = PluginInput::Snapshot(p.clone());
        assert_eq!(input, PluginInput::Snapshot(p));
    }
}
