//! The `/agents` plugin: it lists the local agents and runs the flow that makes one.
//!
//! It is a **compiled-in** plugin — in buzzr's own tree, registered with `register_plugin!` — and it is the worked
//! example of the plugin API: it is written against exactly the same [`PluginInput`]/[`PluginOutput`] contract an
//! external plugin speaks over the wire. It draws nothing and it never touches the key, the relay or the record file:
//! the list comes to it as [`PluginInput::Agents`] (no key in it), the questions it asks the user go through
//! `PluginOutput::Prompt` (buzzr owns the composer), the harness is started by the host and reported back as
//! [`PluginInput::HarnessOffer`] (a plugin never starts a process), and making the agent is `PluginOutput::CreateAgent`
//! — the host makes the key, the folder and the record, and publishes the agent, then answers with
//! [`PluginInput::Created`].
//!
//! The flow: ask for a name and then what the agent should do, ask the *harness* what it offers (its models, and the
//! thinking levels of the model it starts on), let the user pick a model and a level from a panel each, and only then
//! ask for the agent. A harness that will not start or will not answer is an error and nothing is made — the same
//! invariant as a relay that refuses the agent.
//!
//! So the whole feature is expressed in the plugin vocabulary, and a change to how buzzr draws a panel, asks for a line
//! or checks a permission reaches this plugin and an external one alike.

use crate::contrib::{Contributions, Panel, PanelRow, Tone};
use crate::harness;
use crate::plugin::{command, AgentView, Declared, HarnessOffer, Plugin, PluginInput, PluginOutput, AGENTS_READ, AGENTS_WRITE};

/// The id and the command it answers to. A compiled-in plugin is named by its type; the id is what `/plugins`, the
/// permission checks and the command routing key on.
pub const ID: &str = "agents";

/// The new agent the flow is building, filled in a step at a time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Draft {
    name: String,
    instructions: String,
    /// The harness to make it on: an id from `harness.rs`.
    harness: String,
    /// The model the harness offered and the user picked; empty is the harness's own choice.
    model: String,
    /// The thinking level the user picked; empty is the harness's own choice.
    effort: String,
}

/// Where the "new agent" flow is: what it is waiting for. A name and what the agent should do come first, then the
/// harness is asked what it offers — the models, and a model's thinking levels — and each is picked from a panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// A name is being asked for.
    Name,
    /// What the agent should do is being asked for.
    Instructions,
    /// The harness is being asked what it offers (its models, and the levels of the model it starts on).
    Offer,
    /// The models are on a panel, and one is being picked.
    Model,
    /// The harness is being asked for the picked model's thinking levels.
    Levels,
    /// The levels are on a panel, and one is being picked.
    Effort,
    /// An existing agent's new name is being asked for (empty keeps it).
    EditName,
    /// An existing agent's new instructions are being asked for (empty keeps them, `-` clears them).
    EditInstructions,
}

/// The agent being edited: who, and the name chosen for it (once asked).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Edit {
    pubkey: String,
    name: String,
}

/// The plugin. It holds the last agent list it was handed, the step of the flow it is on and what that flow has so far.
#[derive(Default)]
pub struct Agents {
    agents: Vec<AgentView>,
    step: Option<Step>,
    draft: Draft,
    edit: Edit,
    /// The last offer a harness gave: the models, the one it starts on, and that model's levels.
    offer: HarnessOffer,
}

impl Agents {
    /// The panel `agents` opens: one pair of rows per agent, and a row at the end that starts a new one.
    fn panel(&self) -> PluginOutput {
        let mut rows: Vec<PanelRow> = Vec::new();
        if self.agents.is_empty() {
            rows.push(PanelRow { text: "No agents yet.".into(), tone: Tone::Bold, action: None });
            rows.push(PanelRow {
                text: "Enter “new agent” below, or agents create, makes one — its own identity and folder.".into(),
                tone: Tone::Dim,
                action: None,
            });
        }
        for a in &self.agents {
            // Enter on an agent's own row opens its menu (edit, delete), so the verbs are found where the agent is.
            rows.push(PanelRow { text: format!("{}  ·  {}", a.name, short(&a.npub)), tone: Tone::Accent, action: Some(format!("agent:{}", a.pubkey)) });
            rows.push(PanelRow { text: detail(a), tone: Tone::Dim, action: None });
        }
        rows.push(PanelRow { text: "new agent…".into(), tone: Tone::Ok, action: Some("new".into()) });
        // The title is just the name: the panel draws its own "Up/Down · Enter runs · Esc closes" after it, so anything
        // longer here collides with that line and clips.
        PluginOutput::Panel(Panel { id: ID.into(), title: "local agents".into(), rows })
    }

    /// Start the flow: ask for a name. The harness is the default one for now; the model and level are asked of the
    /// harness once it is known to answer.
    fn start(&mut self) -> Vec<PluginOutput> {
        self.step = Some(Step::Name);
        self.draft = Draft { harness: harness::DEFAULT.id.to_owned(), ..Draft::default() };
        self.offer = HarnessOffer::default();
        vec![self.prompt_name()]
    }

    /// `agents` (list them) and `agents create` (make one).
    fn on_command(&mut self, args: &str) -> Vec<PluginOutput> {
        match args.trim() {
            "" => vec![PluginOutput::ListAgents { id: "list".into() }],
            "create" | "new" => self.start(),
            _ => vec![PluginOutput::Flash("usage: agents (list them) or agents create (make one)".into())],
        }
    }

    /// The answer to a prompt the plugin asked for. `None` is the user pressing Esc.
    fn on_answer(&mut self, id: &str, text: Option<String>) -> Vec<PluginOutput> {
        if id.starts_with("edit-") {
            return self.on_edit_answer(id, text);
        }
        match (id, text) {
            ("name", None) | ("instructions", None) => {
                self.reset();
                vec![PluginOutput::Flash("new agent cancelled".into())]
            }
            ("name", Some(text)) => match crate::agents::clean_name(&text) {
                Ok(name) => {
                    self.draft.name = name.clone();
                    self.step = Some(Step::Instructions);
                    vec![PluginOutput::Prompt {
                        id: "instructions".into(),
                        title: "new agent · instructions".into(),
                        hint: format!("What should {name} do? Enter makes it, Esc cancels (Alt+Enter for a new line)"),
                        multiline: true,
                    }]
                }
                // A name that cannot be used is refused where it is typed, before the instructions are asked for.
                Err(e) => vec![PluginOutput::Flash(e.to_string()), self.prompt_name()],
            },
            ("instructions", Some(text)) => {
                if self.step != Some(Step::Instructions) {
                    return Vec::new();
                }
                self.draft.instructions = text;
                // Ask the harness what it offers before anything is made: a harness that will not answer is an error,
                // and then there is nothing to pick from and no agent (see `on_offer`).
                self.step = Some(Step::Offer);
                vec![PluginOutput::ProbeHarness { id: "offer".into(), harness: self.draft.harness.clone(), model: None }]
            }
            _ => Vec::new(),
        }
    }

    /// Start editing an agent: ask for its new name first. The hint shows what it is now.
    fn start_edit(&mut self, pubkey: &str) -> Vec<PluginOutput> {
        if !self.agents.iter().any(|a| a.pubkey == pubkey) {
            return vec![PluginOutput::Flash("that agent is not in the list any more".into())];
        }
        self.step = Some(Step::EditName);
        self.edit = Edit { pubkey: pubkey.to_owned(), name: String::new() };
        let name = self.name_of(pubkey);
        vec![PluginOutput::Prompt {
            id: "edit-name".into(),
            title: format!("edit {name} · name"),
            hint: format!("Name is {name}. Type a new one, or just Enter to keep it. Esc cancels"),
            multiline: false,
        }]
    }

    /// The answers of the edit flow: a name, then instructions, then the host is asked to change the agent.
    fn on_edit_answer(&mut self, id: &str, text: Option<String>) -> Vec<PluginOutput> {
        let Some(agent) = self.agents.iter().find(|a| a.pubkey == self.edit.pubkey).cloned() else {
            self.step = None;
            return vec![PluginOutput::Flash("that agent is not in the list any more".into())];
        };
        let Some(text) = text else {
            self.step = None;
            return vec![PluginOutput::Flash("edit cancelled".into())];
        };
        match (id, self.step) {
            ("edit-name", Some(Step::EditName)) => {
                let name = if text.trim().is_empty() { agent.name.clone() } else { text };
                match crate::agents::clean_name(&name) {
                    Ok(name) => {
                        self.edit.name = name;
                        self.step = Some(Step::EditInstructions);
                        let now = if agent.instructions.is_empty() { "none yet".to_owned() } else { short_text(&agent.instructions) };
                        vec![PluginOutput::Prompt {
                            id: "edit-instructions".into(),
                            title: format!("edit {} · instructions", self.edit.name),
                            hint: format!("Now: {now}. Enter alone keeps them, - clears them (Alt+Enter for a new line)"),
                            multiline: true,
                        }]
                    }
                    Err(e) => vec![PluginOutput::Flash(e.to_string()), self.start_edit(&agent.pubkey).remove(0)],
                }
            }
            ("edit-instructions", Some(Step::EditInstructions)) => {
                self.step = None;
                let instructions = match text.trim() {
                    "" => agent.instructions.clone(),
                    "-" => String::new(),
                    other => other.to_owned(),
                };
                vec![PluginOutput::UpdateAgent { id: "edit".into(), pubkey: agent.pubkey, name: std::mem::take(&mut self.edit.name), instructions }]
            }
            _ => Vec::new(),
        }
    }

    /// The answer to `UpdateAgent`: say what it is called now and re-open the list; a failure changed nothing.
    fn on_updated(&mut self, result: Result<String, String>) -> Vec<PluginOutput> {
        match result {
            Ok(name) => vec![PluginOutput::ListAgents { id: "refresh".into() }, PluginOutput::Flash(format!("{name} updated"))],
            Err(e) => vec![PluginOutput::Flash(format!("could not change the agent: {e}"))],
        }
    }

    fn prompt_name(&self) -> PluginOutput {
        PluginOutput::Prompt {
            id: "name".into(),
            title: "new agent · name".into(),
            hint: "New agent: type a name, then Enter. Esc cancels".into(),
            multiline: false,
        }
    }

    /// The answer to `ProbeHarness`: what a harness offers, or why it could not be asked. A harness that will not
    /// start, or will not answer ACP, is an error — nothing is created, and the flow ends saying why (the same
    /// invariant as a relay that refuses the agent).
    fn on_offer(&mut self, id: &str, result: Result<HarnessOffer, String>) -> Vec<PluginOutput> {
        let expected = match id {
            "offer" => Step::Offer,
            "levels" => Step::Levels,
            _ => return Vec::new(),
        };
        if self.step != Some(expected) {
            return Vec::new(); // a stale answer: the flow moved on, or was cancelled
        }
        match result {
            Err(e) => {
                let what = self.draft.harness.clone();
                self.reset();
                vec![PluginOutput::Flash(format!("could not start the {what} harness: {e} · no agent was made"))]
            }
            Ok(offer) => {
                self.offer = offer;
                // The first look also brings the models; with none to choose from, the levels are next (or, with none
                // of those either, the agent is made on the harness's own choice).
                if id == "offer" && !self.offer.models.is_empty() {
                    self.step = Some(Step::Model);
                    vec![self.model_panel()]
                } else {
                    self.levels()
                }
            }
        }
    }

    /// The next step after the models are known: the thinking levels of the model in hand, or the agent itself.
    fn levels(&mut self) -> Vec<PluginOutput> {
        if self.offer.efforts.is_empty() {
            return self.create();
        }
        self.step = Some(Step::Effort);
        vec![self.effort_panel()]
    }

    /// A panel row the user pressed Enter on: a model, a level, the "new agent…" row (which starts the flow), an agent (which opens its menu), or a
    /// "delete <name>…" row (which asks to confirm the deletion first).
    fn on_panel(&mut self, value: &str) -> Vec<PluginOutput> {
        if value == "new" {
            return self.start();
        }
        // Backing out of a deletion is a plain refresh, so the list reflects whatever is really there.
        if value == "keep" {
            return vec![PluginOutput::ListAgents { id: "refresh".into() }];
        }
        if let Some(pubkey) = value.strip_prefix("agent:") {
            return vec![self.agent_menu(pubkey)];
        }
        if let Some(pubkey) = value.strip_prefix("edit:") {
            return self.start_edit(pubkey);
        }
        if let Some(pubkey) = value.strip_prefix("delete:") {
            return vec![self.delete_panel(pubkey)];
        }
        if let Some(pubkey) = value.strip_prefix("confirm:") {
            // The host owns the relay and the record file, so the plugin only asks; the answer comes back as
            // `PluginInput::Deleted`.
            return vec![PluginOutput::DeleteAgent { id: "delete".into(), pubkey: pubkey.to_owned() }];
        }
        if let Some(picked) = value.strip_prefix("model:") {
            if self.step != Some(Step::Model) {
                return Vec::new();
            }
            self.draft.model = picked.to_owned();
            // The offer already holds this model's levels when it is the one the session started on; otherwise the
            // harness is asked again, so the levels offered are that model's.
            if self.draft.model == self.offer.model {
                return self.levels();
            }
            self.step = Some(Step::Levels);
            return vec![PluginOutput::ProbeHarness {
                id: "levels".into(),
                harness: self.draft.harness.clone(),
                model: Some(self.draft.model.clone()),
            }];
        }
        if let Some(picked) = value.strip_prefix("effort:") {
            if self.step != Some(Step::Effort) {
                return Vec::new();
            }
            self.draft.effort = picked.to_owned();
            return self.create();
        }
        Vec::new()
    }

    /// Make the agent: the picks are the flow's, and the host owns the key, the folder and the relay.
    fn create(&mut self) -> Vec<PluginOutput> {
        let draft = std::mem::take(&mut self.draft);
        self.step = None;
        vec![PluginOutput::CreateAgent {
            id: "create".into(),
            name: draft.name,
            instructions: draft.instructions,
            harness: draft.harness,
            model: draft.model,
            effort: draft.effort,
        }]
    }

    fn reset(&mut self) {
        self.step = None;
        self.draft = Draft::default();
        self.offer = HarnessOffer::default();
    }

    /// The model panel: one row per model the harness listed, the one it starts on marked. A row's action carries the
    /// value to set, so the answer needs no other state.
    fn model_panel(&self) -> PluginOutput {
        let rows = self.offer.models.iter().map(|c| choice_row("model", c, &self.offer.model)).collect();
        PluginOutput::Panel(Panel { id: ID.into(), title: "new agent · model".into(), rows })
    }

    /// The thinking-level panel, for the model in hand.
    fn effort_panel(&self) -> PluginOutput {
        let rows = self.offer.efforts.iter().map(|c| choice_row("effort", c, &self.offer.effort)).collect();
        PluginOutput::Panel(Panel { id: ID.into(), title: "new agent · level".into(), rows })
    }

    /// The menu an agent's row opens: what can be done to it. `back` is the same refresh as backing out of a deletion.
    fn agent_menu(&self, pubkey: &str) -> PluginOutput {
        let name = self.name_of(pubkey);
        let rows = vec![
            PanelRow { text: "edit name and instructions…".into(), tone: Tone::Accent, action: Some(format!("edit:{pubkey}")) },
            PanelRow { text: "delete…".into(), tone: Tone::Error, action: Some(format!("delete:{pubkey}")) },
            PanelRow { text: "back".into(), tone: Tone::Dim, action: Some("keep".into()) },
        ];
        PluginOutput::Panel(Panel { id: ID.into(), title: name, rows })
    }

    /// The confirmation a "delete <name>…" row opens: deleting an agent is not undone here, so it takes a second,
    /// deliberate Enter. Esc closes the panel without deleting anything.
    fn delete_panel(&self, pubkey: &str) -> PluginOutput {
        let name = self.name_of(pubkey);
        let rows = vec![
            PanelRow { text: format!("Delete {name}: take it off the relay and delete its key here"), tone: Tone::Error, action: Some(format!("confirm:{pubkey}")) },
            PanelRow { text: "keep it".into(), tone: Tone::Dim, action: Some("keep".into()) },
        ];
        PluginOutput::Panel(Panel { id: ID.into(), title: format!("delete {name}"), rows })
    }

    /// The name of an agent in the list in hand, for the words on a confirmation and on a result. A pubkey the list no
    /// longer holds is named by its own short form, so a late answer still reads.
    fn name_of(&self, pubkey: &str) -> String {
        self.agents.iter().find(|a| a.pubkey == pubkey).map(|a| a.name.clone()).unwrap_or_else(|| short(pubkey))
    }

    /// The answer to `CreateAgent`: say plainly that it was made and published, and re-open the list on it. A relay
    /// that refused it (or any other failure) made no agent at all, so the error is all there is to say.
    fn on_created(&mut self, result: Result<crate::plugin::CreatedAgent, String>) -> Vec<PluginOutput> {
        match result {
            Ok(a) => {
                // The list first, the flash last: a panel opening sets its own hint, so saying what happened has to be
                // the final word or the "created" message is lost behind it.
                vec![
                    PluginOutput::ListAgents { id: "refresh".into() },
                    PluginOutput::Flash(format!("{} created ({}) · published to the relay · /agents lists it", a.name, short(&a.npub))),
                ]
            }
            // Nothing was made: no key, no folder, no record. Say why and leave the list as it was.
            Err(e) => vec![PluginOutput::Flash(format!("could not create the agent: {e}"))],
        }
    }

    /// The answer to `DeleteAgent`: say plainly that it is gone, and re-open the list without it. A relay that refused
    /// to take it off (or any other failure) left it in place, so the error is all there is to say.
    fn on_deleted(&mut self, result: Result<String, String>) -> Vec<PluginOutput> {
        match result {
            Ok(name) => vec![
                // The list first, the flash last: a panel opening sets its own hint, so saying what happened has to be
                // the final word or the "deleted" message is lost behind it.
                PluginOutput::ListAgents { id: "refresh".into() },
                PluginOutput::Flash(format!("{name} deleted · taken off the relay and its key removed")),
            ],
            // It is still there — listed and running — so the list is left as it was.
            Err(e) => vec![PluginOutput::Flash(format!("could not delete the agent: {e}"))],
        }
    }
}

/// One row of a choice panel: the label, marked when it is the value in force, and an action naming the setting and the
/// value to set (`model:opus`, `effort:high`).
fn choice_row(kind: &str, choice: &crate::plugin::Choice, current: &str) -> PanelRow {
    let text = if choice.value == current && !current.is_empty() { format!("{}  (current)", choice.label) } else { choice.label.clone() };
    PanelRow { text, tone: Tone::Default, action: Some(format!("{kind}:{}", choice.value)) }
}

/// The second line of an agent's panel row: what runs it, on what model and level, and where it works.
fn detail(a: &AgentView) -> String {
    let mut what = a.harness.clone();
    for pick in [&a.model, &a.effort].into_iter().flatten() {
        what.push_str(" · ");
        what.push_str(pick);
    }
    format!("   {what}  ·  {}", a.workspace)
}

impl Plugin for Agents {
    fn id(&self) -> &str {
        ID
    }

    fn ready(&self) -> Declared {
        Declared {
            name: "Agents".into(),
            permissions: vec![AGENTS_READ.into(), AGENTS_WRITE.into()],
            tick_ms: 0,
            contributions: Contributions {
                commands: vec![command(ID, "your local agents: list, edit, delete, create", "[create]")],
                ..Contributions::default()
            },
        }
    }

    fn on(&mut self, input: PluginInput) -> Vec<PluginOutput> {
        match input {
            PluginInput::Command { command, args } if command == ID => self.on_command(&args),
            PluginInput::Prompt { id, text } => self.on_answer(&id, text),
            PluginInput::Agents { agents, .. } => {
                self.agents = agents;
                vec![self.panel()]
            }
            PluginInput::Created { result, .. } => self.on_created(result),
            PluginInput::Deleted { result, .. } => self.on_deleted(result),
            PluginInput::Updated { result, .. } => self.on_updated(result),
            PluginInput::HarnessOffer { id, result } => self.on_offer(&id, result),
            PluginInput::PanelAction { value: Some(v), .. } => self.on_panel(&v),
            _ => Vec::new(),
        }
    }
}

/// The first line of some text, cut to fit a hint.
fn short_text(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > 60 || text.lines().count() > 1 {
        format!("{}…", line.chars().take(60).collect::<String>())
    } else {
        line.to_owned()
    }
}

/// `npub1abcd…wxyz`, so a flash stays one line.
fn short(npub: &str) -> String {
    if npub.chars().count() > 16 {
        format!("{}…{}", npub.chars().take(12).collect::<String>(), npub.chars().skip(npub.chars().count() - 4).collect::<String>())
    } else {
        npub.to_owned()
    }
}

crate::register_plugin!(Agents);

#[cfg(test)]
mod tests {
    use super::*;

    fn view(name: &str) -> AgentView {
        AgentView {
            name: name.into(),
            pubkey: "00".repeat(32),
            npub: "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqs2d".into(),
            harness: "claude-agent-acp".into(),
            model: Some("opus".into()),
            effort: Some("high".into()),
            workspace: "/home/me/.config/buzzr/agents/scout".into(),
            instructions: "watch the repo".into(),
            created_at: 0,
        }
    }

    fn declares(p: &mut Agents) -> Declared {
        p.ready()
    }

    /// What a harness answered: its models, the one it starts on, and that model's levels.
    fn offer(models: &[(&str, &str)], model: &str, efforts: &[&str], effort: &str) -> HarnessOffer {
        let choice = |(v, l): &(&str, &str)| crate::plugin::Choice { value: (*v).into(), label: (*l).into() };
        HarnessOffer {
            models: models.iter().map(choice).collect(),
            model: model.into(),
            efforts: efforts.iter().map(|e| crate::plugin::Choice { value: (*e).into(), label: (*e).into() }).collect(),
            effort: effort.into(),
        }
    }

    /// Run the flow up to the point where the harness is asked what it offers.
    fn up_to_the_probe(p: &mut Agents) {
        p.on(PluginInput::Command { command: ID.into(), args: "create".into() });
        p.on(PluginInput::Prompt { id: "name".into(), text: Some("Scout".into()) });
        p.on(PluginInput::Prompt { id: "instructions".into(), text: Some("watch the repo".into()) });
    }

    #[test]
    fn the_plugin_declares_its_command_and_the_agents_permissions() {
        let d = declares(&mut Agents::default());
        assert_eq!(d.name, "Agents");
        assert_eq!(d.permissions, vec![AGENTS_READ.to_string(), AGENTS_WRITE.to_string()]);
        assert_eq!(d.contributions.commands[0].id, ID);
        assert_eq!(d.tick_ms, 0);
    }

    #[test]
    fn listing_asks_for_the_agents_and_then_draws_a_panel() {
        let mut p = Agents::default();
        assert_eq!(p.on(PluginInput::Command { command: ID.into(), args: String::new() }), vec![PluginOutput::ListAgents { id: "list".into() }]);
        let outs = p.on(PluginInput::Agents { id: "list".into(), agents: vec![view("Scout")] });
        let PluginOutput::Panel(panel) = &outs[0] else { panic!("not a panel") };
        assert_eq!(panel.id, ID);
        assert!(panel.rows.iter().any(|r| r.text.contains("Scout")), "the agent is listed");
        // the second line names what runs it and on what, and where it works
        assert!(panel.rows.iter().any(|r| r.text.contains("claude-agent-acp · opus · high")), "{:?}", panel.rows);
        assert_eq!(panel.rows.last().and_then(|r| r.action.clone()).as_deref(), Some("new"), "and a row makes a new one");
    }

    #[test]
    fn the_flow_asks_for_a_name_then_the_instructions_then_the_harness_and_makes_the_agent() {
        let mut p = Agents::default();
        // `agents create` asks for a name
        let outs = p.on(PluginInput::Command { command: ID.into(), args: "create".into() });
        assert!(matches!(outs[0], PluginOutput::Prompt { ref id, multiline: false, .. } if id == "name"));
        // a name that cannot be used is refused, and the name is asked for again
        let outs = p.on(PluginInput::Prompt { id: "name".into(), text: Some("   ".into()) });
        assert!(matches!(outs[0], PluginOutput::Flash(_)));
        assert!(matches!(outs[1], PluginOutput::Prompt { ref id, .. } if id == "name"));
        // a good name moves to the instructions
        let outs = p.on(PluginInput::Prompt { id: "name".into(), text: Some("  Scout ".into()) });
        let PluginOutput::Prompt { id, hint, multiline, .. } = &outs[0] else { panic!("not a prompt") };
        assert_eq!(id, "instructions");
        assert!(hint.contains("Scout"), "the question names the agent: {hint}");
        assert!(*multiline, "the instructions may be a block of text");
        // the instructions are not the last step any more: the harness is asked what it offers first
        assert_eq!(
            p.on(PluginInput::Prompt { id: "instructions".into(), text: Some("watch the repo".into()) }),
            vec![PluginOutput::ProbeHarness { id: "offer".into(), harness: harness::DEFAULT.id.into(), model: None }]
        );
        // the answer brings the models, which come up on a panel with the one in force marked
        let outs = p.on(PluginInput::HarnessOffer {
            id: "offer".into(),
            result: Ok(offer(&[("sonnet", "Sonnet"), ("opus", "Opus")], "sonnet", &["low", "high"], "low")),
        });
        let PluginOutput::Panel(panel) = &outs[0] else { panic!("not a panel") };
        assert_eq!(panel.title, "new agent · model");
        assert!(panel.rows[0].text.contains("Sonnet") && panel.rows[0].text.contains("current"), "the model in force is marked: {:?}", panel.rows[0]);
        assert_eq!(panel.rows[1].action.as_deref(), Some("model:opus"));
        // picking the model in force needs no second look: its levels are already in hand
        let outs = p.on(PluginInput::PanelAction { id: ID.into(), row: 0, value: Some("model:sonnet".into()) });
        let PluginOutput::Panel(panel) = &outs[0] else { panic!("not a panel") };
        assert_eq!(panel.title, "new agent · level");
        assert!(panel.rows[0].text.contains("current"), "{:?}", panel.rows[0]);
        assert_eq!(panel.rows[1].action.as_deref(), Some("effort:high"));
        // picking a level makes the agent: the name, the instructions, the harness and both picks
        assert_eq!(
            p.on(PluginInput::PanelAction { id: ID.into(), row: 1, value: Some("effort:high".into()) }),
            vec![PluginOutput::CreateAgent {
                id: "create".into(),
                name: "Scout".into(),
                instructions: "watch the repo".into(),
                harness: harness::DEFAULT.id.into(),
                model: "sonnet".into(),
                effort: "high".into(),
            }]
        );
    }

    #[test]
    fn picking_another_model_asks_the_harness_for_that_model_s_levels() {
        let mut p = Agents::default();
        up_to_the_probe(&mut p);
        p.on(PluginInput::HarnessOffer { id: "offer".into(), result: Ok(offer(&[("sonnet", "Sonnet"), ("opus", "Opus")], "sonnet", &["low", "high"], "low")) });
        // a model that is not the one the session started on: the harness is asked for *its* levels
        assert_eq!(
            p.on(PluginInput::PanelAction { id: ID.into(), row: 1, value: Some("model:opus".into()) }),
            vec![PluginOutput::ProbeHarness { id: "levels".into(), harness: harness::DEFAULT.id.into(), model: Some("opus".into()) }]
        );
        let outs = p.on(PluginInput::HarnessOffer {
            id: "levels".into(),
            result: Ok(offer(&[], "opus", &["low", "medium", "high"], "medium")),
        });
        let PluginOutput::Panel(panel) = &outs[0] else { panic!("not a panel") };
        assert_eq!(panel.rows.len(), 3, "the levels are the picked model's");
        assert!(panel.rows[1].text.contains("current"), "{:?}", panel.rows[1]);
        // and the pick that follows carries the model that was chosen
        let outs = p.on(PluginInput::PanelAction { id: ID.into(), row: 2, value: Some("effort:high".into()) });
        let PluginOutput::CreateAgent { model, effort, .. } = &outs[0] else { panic!("not a create") };
        assert_eq!((model.as_str(), effort.as_str()), ("opus", "high"));
    }

    #[test]
    fn a_harness_offering_no_model_and_no_levels_makes_the_agent_on_its_own_choice() {
        let mut p = Agents::default();
        up_to_the_probe(&mut p);
        assert_eq!(
            p.on(PluginInput::HarnessOffer { id: "offer".into(), result: Ok(HarnessOffer::default()) }),
            vec![PluginOutput::CreateAgent {
                id: "create".into(),
                name: "Scout".into(),
                instructions: "watch the repo".into(),
                harness: harness::DEFAULT.id.into(),
                model: String::new(),
                effort: String::new(),
            }]
        );
    }

    #[test]
    fn a_harness_that_will_not_answer_ends_the_flow_and_makes_nothing() {
        let mut p = Agents::default();
        up_to_the_probe(&mut p);
        let outs = p.on(PluginInput::HarnessOffer {
            id: "offer".into(),
            result: Err("claude-agent-acp is not installed, or is not on PATH".into()),
        });
        assert_eq!(outs.len(), 1, "nothing was made: only the reason is said");
        assert!(
            matches!(outs[0], PluginOutput::Flash(ref t) if t.contains("is not installed") && t.contains("no agent was made")),
            "{outs:?}"
        );
        // the flow is over: a late answer changes nothing
        assert!(p.on(PluginInput::HarnessOffer { id: "offer".into(), result: Ok(HarnessOffer::default()) }).is_empty());
    }

    #[test]
    fn esc_cancels_the_flow_and_the_panel_row_starts_one() {
        let mut p = Agents::default();
        let _ = p.on(PluginInput::Command { command: ID.into(), args: "create".into() });
        let outs = p.on(PluginInput::Prompt { id: "name".into(), text: None });
        assert!(matches!(outs[0], PluginOutput::Flash(ref t) if t.contains("cancelled")));
        let outs = p.on(PluginInput::PanelAction { id: ID.into(), row: 1, value: Some("new".into()) });
        assert!(matches!(outs[0], PluginOutput::Prompt { ref id, .. } if id == "name"));
    }

    #[test]
    fn a_created_agent_is_reported_and_the_list_is_refreshed() {
        let mut p = Agents::default();
        let outs = p.on(PluginInput::Created {
            id: "create".into(),
            result: Ok(crate::plugin::CreatedAgent {
                name: "Scout".into(),
                pubkey: "00".repeat(32),
                npub: "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqs2d".into(),
            }),
        });
        // the list is refreshed first, then the result is said — so the flash is the last word after the panel opens
        assert!(matches!(&outs[0], PluginOutput::ListAgents { id } if id == "refresh"));
        assert!(matches!(outs[1], PluginOutput::Flash(ref t) if t.contains("published to the relay")));

        // a creation that failed (a relay that refused it, a name that is taken) made no agent: only the reason is said
        let outs = p.on(PluginInput::Created { id: "create".into(), result: Err("the relay returned 500".into()) });
        assert_eq!(outs.len(), 1, "nothing was made, so there is no list to refresh");
        assert!(matches!(outs[0], PluginOutput::Flash(ref t) if t.contains("could not create") && t.contains("relay returned 500")));
    }

    /// An agent's own row opens a menu with edit, delete and back; the list itself carries no verbs.
    #[test]
    fn an_agents_row_opens_a_menu_of_what_can_be_done_to_it() {
        let mut p = Agents::default();
        let a = view("Scout");
        let outs = p.on(PluginInput::Agents { id: "list".into(), agents: vec![a.clone()] });
        let PluginOutput::Panel(list) = &outs[0] else { panic!("not a panel") };
        assert!(list.rows.iter().all(|r| !r.text.contains("edit") && !r.text.contains("delete")), "{:?}", list.rows);
        let agent = format!("agent:{}", a.pubkey);
        assert!(list.rows.iter().any(|r| r.action.as_deref() == Some(agent.as_str())), "{:?}", list.rows);
        let outs = p.on(PluginInput::PanelAction { id: ID.into(), row: 0, value: Some(agent) });
        let PluginOutput::Panel(menu) = &outs[0] else { panic!("not a panel") };
        assert_eq!(menu.title, "Scout");
        let actions: Vec<_> = menu.rows.iter().map(|r| r.action.clone().unwrap_or_default()).collect();
        assert_eq!(actions, vec![format!("edit:{}", a.pubkey), format!("delete:{}", a.pubkey), "keep".to_owned()]);
    }

    #[test]
    fn an_agent_can_be_deleted_from_the_panel_after_a_confirmation() {
        let mut p = Agents::default();
        let a = view("Scout");
        let _ = p.on(PluginInput::Agents { id: "list".into(), agents: vec![a.clone()] });
        // the agent's menu offers to delete it
        let delete = format!("delete:{}", a.pubkey);
        let outs = p.on(PluginInput::PanelAction { id: ID.into(), row: 2, value: Some(delete.clone()) });
        let PluginOutput::Panel(panel) = &outs[0] else { panic!("not a panel") };
        assert_eq!(panel.title, "delete Scout");
        assert!(panel.rows[0].text.contains("Delete Scout"), "{:?}", panel.rows);
        assert_eq!(panel.rows[0].action.as_deref(), Some(format!("confirm:{}", a.pubkey).as_str()));
        // backing out refreshes the list and deletes nothing
        assert_eq!(
            p.on(PluginInput::PanelAction { id: ID.into(), row: 1, value: Some("keep".into()) }),
            vec![PluginOutput::ListAgents { id: "refresh".into() }]
        );
        // the confirmation is what asks the host to delete it
        assert_eq!(
            p.on(PluginInput::PanelAction { id: ID.into(), row: 0, value: Some(format!("confirm:{}", a.pubkey)) }),
            vec![PluginOutput::DeleteAgent { id: "delete".into(), pubkey: a.pubkey.clone() }]
        );
        // the answer says it is gone, and the list is refreshed first so the flash is the last word
        let outs = p.on(PluginInput::Deleted { id: "delete".into(), result: Ok("Scout".into()) });
        assert!(matches!(&outs[0], PluginOutput::ListAgents { id } if id == "refresh"));
        assert!(matches!(outs[1], PluginOutput::Flash(ref t) if t.contains("Scout deleted")));
        // a relay that refused to take it off left it in place: only the reason is said
        let outs = p.on(PluginInput::Deleted { id: "delete".into(), result: Err("the relay returned 500".into()) });
        assert_eq!(outs.len(), 1, "nothing was deleted, so there is no list to refresh");
        assert!(matches!(outs[0], PluginOutput::Flash(ref t) if t.contains("could not delete") && t.contains("relay returned 500")));
    }

    #[test]
    fn an_agent_can_be_edited_from_the_panel() {
        let mut p = Agents::default();
        let a = view("Scout");
        let outs = p.on(PluginInput::Agents { id: "list".into(), agents: vec![a.clone()] });
        let PluginOutput::Panel(_) = &outs[0] else { panic!("not a panel") };
        let outs = p.on(PluginInput::PanelAction { id: ID.into(), row: 2, value: Some(format!("edit:{}", a.pubkey)) });
        assert!(matches!(&outs[0], PluginOutput::Prompt { id, hint, .. } if id == "edit-name" && hint.contains("Scout")), "{outs:?}");
        // a bad name is refused and asked again
        let outs = p.on(PluginInput::Prompt { id: "edit-name".into(), text: Some("\u{1b}x".into()) });
        assert!(matches!(outs[0], PluginOutput::Flash(_)) && matches!(&outs[1], PluginOutput::Prompt { id, .. } if id == "edit-name"), "{outs:?}");
        let outs = p.on(PluginInput::Prompt { id: "edit-name".into(), text: Some(" Pathfinder ".into()) });
        assert!(matches!(&outs[0], PluginOutput::Prompt { id, multiline: true, hint, .. } if id == "edit-instructions" && hint.contains("watch the repo")), "{outs:?}");
        assert_eq!(
            p.on(PluginInput::Prompt { id: "edit-instructions".into(), text: Some("patrol".into()) }),
            vec![PluginOutput::UpdateAgent { id: "edit".into(), pubkey: a.pubkey.clone(), name: "Pathfinder".into(), instructions: "patrol".into() }]
        );
        // Enter alone keeps both
        p.on(PluginInput::PanelAction { id: ID.into(), row: 2, value: Some(format!("edit:{}", a.pubkey)) });
        p.on(PluginInput::Prompt { id: "edit-name".into(), text: Some(String::new()) });
        assert_eq!(
            p.on(PluginInput::Prompt { id: "edit-instructions".into(), text: Some(String::new()) }),
            vec![PluginOutput::UpdateAgent { id: "edit".into(), pubkey: a.pubkey.clone(), name: "Scout".into(), instructions: "watch the repo".into() }]
        );
        // `-` clears the instructions; Esc cancels
        p.on(PluginInput::PanelAction { id: ID.into(), row: 2, value: Some(format!("edit:{}", a.pubkey)) });
        p.on(PluginInput::Prompt { id: "edit-name".into(), text: Some(String::new()) });
        let outs = p.on(PluginInput::Prompt { id: "edit-instructions".into(), text: Some("-".into()) });
        assert!(matches!(&outs[0], PluginOutput::UpdateAgent { instructions, .. } if instructions.is_empty()), "{outs:?}");
        p.on(PluginInput::PanelAction { id: ID.into(), row: 2, value: Some(format!("edit:{}", a.pubkey)) });
        let outs = p.on(PluginInput::Prompt { id: "edit-name".into(), text: None });
        assert!(matches!(outs[0], PluginOutput::Flash(ref t) if t.contains("cancelled")));
        // the answers
        let outs = p.on(PluginInput::Updated { id: "edit".into(), result: Ok("Pathfinder".into()) });
        assert!(matches!(&outs[0], PluginOutput::ListAgents { id } if id == "refresh") && matches!(&outs[1], PluginOutput::Flash(t) if t.contains("Pathfinder updated")));
        let outs = p.on(PluginInput::Updated { id: "edit".into(), result: Err("the relay returned 500".into()) });
        assert!(matches!(&outs[..], [PluginOutput::Flash(t)] if t.contains("could not change")));
    }

    #[test]
    fn an_unknown_command_or_input_is_ignored() {
        let mut p = Agents::default();
        assert!(matches!(p.on(PluginInput::Command { command: "other".into(), args: String::new() })[..], []));
        assert!(matches!(p.on(PluginInput::Tick)[..], []));
        assert!(matches!(p.on(PluginInput::Prompt { id: "nope".into(), text: Some("x".into()) })[..], []));
        // a panel row that names no choice we know does nothing, whatever step the flow is on
        assert!(matches!(p.on(PluginInput::PanelAction { id: ID.into(), row: 0, value: Some("nope".into()) })[..], []));
    }
}
