//! The `/agents` plugin: it lists the local agents and runs the two-step flow that makes one.
//!
//! It is a **compiled-in** plugin — in buzzr's own tree, registered with `register_plugin!` — and it is the worked
//! example of the plugin API: it is written against exactly the same [`PluginInput`]/[`PluginOutput`] contract an
//! external plugin speaks over the wire. It draws nothing and it never touches the key, the relay or the record file:
//! the list comes to it as [`PluginInput::Agents`] (no key in it), the question it asks the user goes through
//! `PluginOutput::Prompt` (buzzr owns the composer), and making the agent is `PluginOutput::CreateAgent` — the host
//! makes the key, the folder and the record, and publishes the agent, then answers with [`PluginInput::Created`].
//!
//! So the whole feature is expressed in the plugin vocabulary, and a change to how buzzr draws a panel, asks for a line
//! or checks a permission reaches this plugin and an external one alike.

use crate::contrib::{Contributions, Panel, PanelRow, Tone};
use crate::plugin::{command, AgentView, Declared, Plugin, PluginInput, PluginOutput, AGENTS_READ, AGENTS_WRITE};

/// The id and the command it answers to. A compiled-in plugin is named by its type; the id is what `/plugins`, the
/// permission checks and the command routing key on.
pub const ID: &str = "agents";

/// The step of the "new agent" flow the plugin is on: a name is asked for first, then what the agent should do.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Flow {
    Name,
    Instructions { name: String },
}

/// The plugin. It holds the last agent list it was handed and the step of the flow it is on, if any.
#[derive(Default)]
pub struct Agents {
    agents: Vec<AgentView>,
    flow: Option<Flow>,
}

impl Agents {
    /// The panel `/agents` opens: one pair of rows per agent, and a row at the end that starts a new one.
    fn panel(&self) -> PluginOutput {
        let mut rows: Vec<PanelRow> = Vec::new();
        if self.agents.is_empty() {
            rows.push(PanelRow { text: "No agents yet.".into(), tone: Tone::Bold, action: None });
            rows.push(PanelRow {
                text: "Enter “new agent” below, or /agents create, makes one — its own identity and folder.".into(),
                tone: Tone::Dim,
                action: None,
            });
        }
        for a in &self.agents {
            rows.push(PanelRow { text: format!("{}  ·  {}", a.name, short(&a.npub)), tone: Tone::Accent, action: None });
            rows.push(PanelRow { text: format!("   {}  ·  {}", a.harness, a.workspace), tone: Tone::Dim, action: None });
        }
        rows.push(PanelRow { text: "new agent…".into(), tone: Tone::Ok, action: Some("new".into()) });
        // The title is just the name: the panel draws its own "Up/Down · Enter runs · Esc closes" after it, so anything
        // longer here collides with that line and clips.
        PluginOutput::Panel(Panel { id: ID.into(), title: "local agents".into(), rows })
    }

    /// Start the flow: ask for a name.
    fn start(&mut self) -> Vec<PluginOutput> {
        self.flow = Some(Flow::Name);
        vec![PluginOutput::Prompt {
            id: "name".into(),
            title: "new agent · name".into(),
            hint: "New agent: type a name, then Enter. Esc cancels".into(),
            multiline: false,
        }]
    }

    /// `/agents` (list them) and `/agents create` (make one).
    fn on_command(&mut self, args: &str) -> Vec<PluginOutput> {
        match args.trim() {
            "" => vec![PluginOutput::ListAgents { id: "list".into() }],
            "create" | "new" => self.start(),
            _ => vec![PluginOutput::Flash("usage: /agents (list them) or /agents create (make one)".into())],
        }
    }

    /// The answer to a prompt the plugin asked for. `None` is the user pressing Esc.
    fn on_answer(&mut self, id: &str, text: Option<String>) -> Vec<PluginOutput> {
        match (id, text) {
            ("name", None) | ("instructions", None) => {
                self.flow = None;
                vec![PluginOutput::Flash("new agent cancelled".into())]
            }
            ("name", Some(text)) => match crate::agents::clean_name(&text) {
                Ok(name) => {
                    self.flow = Some(Flow::Instructions { name: name.clone() });
                    vec![PluginOutput::Prompt {
                        id: "instructions".into(),
                        title: "new agent · instructions".into(),
                        hint: format!("What should {name} do? Enter makes it, Esc cancels (Alt+Enter for a new line)"),
                        multiline: true,
                    }]
                }
                // A name that cannot be used is refused where it is typed, before the instructions are asked for.
                Err(e) => vec![PluginOutput::Flash(e), self.prompt_name()],
            },
            ("instructions", Some(text)) => match self.flow.take() {
                Some(Flow::Instructions { name }) => vec![PluginOutput::CreateAgent { id: "create".into(), name, instructions: text }],
                _ => Vec::new(),
            },
            _ => Vec::new(),
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
                commands: vec![command(ID, "your local agents; /agents create makes one", "/agents [create]")],
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
            PluginInput::PanelAction { value: Some(v), .. } if v == "new" => self.start(),
            _ => Vec::new(),
        }
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
            harness: "claude-code-acp".into(),
            workspace: "/home/me/.config/buzzr/agents/scout".into(),
            created_at: 0,
        }
    }

    fn declares(p: &mut Agents) -> Declared {
        p.ready()
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
        assert_eq!(panel.rows.last().and_then(|r| r.action.clone()).as_deref(), Some("new"), "and a row makes a new one");
    }

    #[test]
    fn the_flow_asks_for_a_name_then_the_instructions_then_makes_the_agent() {
        let mut p = Agents::default();
        // `/agents create` asks for a name
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
        // the instructions make the agent
        assert_eq!(
            p.on(PluginInput::Prompt { id: "instructions".into(), text: Some("watch the repo".into()) }),
            vec![PluginOutput::CreateAgent { id: "create".into(), name: "Scout".into(), instructions: "watch the repo".into() }]
        );
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

    #[test]
    fn an_unknown_command_or_input_is_ignored() {
        let mut p = Agents::default();
        assert!(matches!(p.on(PluginInput::Command { command: "other".into(), args: String::new() })[..], []));
        assert!(matches!(p.on(PluginInput::Tick)[..], []));
        assert!(matches!(p.on(PluginInput::Prompt { id: "nope".into(), text: Some("x".into()) })[..], []));
    }
}
