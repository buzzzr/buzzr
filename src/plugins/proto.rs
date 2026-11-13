//! The wire between buzzr and a plugin process: one JSON object per line, both ways.
//!
//! Host to plugin: `hello`, `snapshot`, `tick`, `invoke`, `panel_action`, `prompt_answer`, `agents`, `harness_offer`,
//! `created`, `deleted`, `bye`.
//! Plugin to host: `ready`, `status`, `pane`, `panel`, `flash`, `draft`, `send`, `prompt`, `list_agents`, `probe_harness`,
//! `create_agent`, `delete_agent`, `log`.
//! Unknown message types and unknown fields are ignored (and logged), so a newer plugin can talk to an older buzzr; a
//! plugin that cannot speak the `apiVersion` in `hello` refuses it and exits instead.

use crate::contrib::{clean_block, clean_line, Contributions, Pane, Panel, Projection, Segment, MAX_ROWS};
use crate::plugins::discover;
use serde::Serialize;
use serde_json::Value;

pub const PROTOCOL: u32 = 1;
/// The contract version, sent in `hello`. It lives in `discover` because that is the module the host reads it from.
pub const API_VERSION: u32 = discover::API_VERSION;
/// A line longer than this is a protocol violation, not a message.
pub const MAX_LINE: usize = 64 * 1024;

/// Host to plugin. One of these is serialised per line.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPlugin<'a> {
    Hello { protocol: u32, #[serde(rename = "apiVersion")] api_version: u32, host: &'a str, plugin: &'a str },
    Snapshot { seq: u64, projection: &'a Projection },
    /// The opt-in heartbeat the plugin asked for with `tickMs` in its `ready`: something time-based may need redrawing.
    Tick { seq: u64 },
    Invoke { id: u64, command: &'a str, args: &'a str },
    PanelAction { id: &'a str, row: usize, value: Option<&'a str> },
    /// The user answered (or, with no `text`, cancelled) a prompt the plugin asked for with `prompt`.
    PromptAnswer { id: &'a str, #[serde(skip_serializing_if = "Option::is_none")] text: Option<&'a str> },
    /// The local agents, as the answer to the plugin's `list_agents`.
    Agents { id: &'a str, agents: &'a [crate::plugin::AgentView] },
    /// The answer to the plugin's `probe_harness`: what the harness offers (`offer`), or why it could not be asked
    /// (`error`: it is not installed, or it did not answer ACP).
    HarnessOffer {
        id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        offer: Option<&'a crate::plugin::HarnessOffer>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a str>,
    },
    /// The answer to the plugin's `create_agent`: the agent that was made (`agent`), or why not (`error`).
    Created {
        id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<&'a crate::plugin::CreatedAgent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a str>,
    },
    /// The answer to the plugin's `delete_agent`: the name of the agent that was deleted (`name`), or why not (`error`).
    Deleted {
        id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a str>,
    },
    /// The answer to the plugin's `update_agent`: the name the agent has now (`name`), or why not (`error`).
    Updated {
        id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a str>,
    },
    Bye,
}

impl ToPlugin<'_> {
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("a message of plain data serialises")
    }
}

/// What a plugin declares about itself when it says `ready` — its display name, what it may do, and whether it wants a
/// heartbeat — all of it in the plugin's own code, where it belongs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declaration {
    /// The display name, cleaned; empty means "call it by its id".
    pub name: String,
    /// What the plugin asked to be allowed to do. Only names this build enforces survive.
    pub permissions: Vec<String>,
    /// Opt-in heartbeat in milliseconds; `0` (or absent) means never.
    pub tick_ms: u64,
}

/// Plugin to host, after parsing and cleaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromPlugin {
    Ready { declaration: Declaration, contributions: Contributions },
    /// Replaces the plugin's status item; empty segments clear it.
    Status { segments: Vec<Segment> },
    /// Replaces the plugin's pane under the channel list; `None` clears it.
    Pane(Option<Pane>),
    Panel(Panel),
    Flash { text: String },
    Draft { text: String },
    Send { channel: Option<String>, text: String },
    /// Ask the user for a line (`multiline == false`) or a block of text: buzzr draws and owns the prompt, and the
    /// answer comes back as a `prompt_answer` with this `id`.
    Prompt { id: String, title: String, hint: String, multiline: bool },
    /// Ask for the local agents; the answer comes back as `agents` with this `id`.
    ListAgents { id: String },
    /// Ask buzzr to start a harness and read what it offers for a new agent. When `model` is given the session is put
    /// on that model first, so the levels that come back are that model's. The answer comes back as `harness_offer`
    /// with this `id`. The harness is buzzr's to run: a plugin never starts a process.
    ProbeHarness { id: String, harness: String, model: Option<String> },
    /// Ask buzzr to make and publish a local agent; the answer comes back as `created` with this `id`.
    CreateAgent { id: String, name: String, instructions: String, harness: String, model: String, effort: String },
    /// Ask buzzr to delete the local agent `pubkey`; the answer comes back as `deleted` with this `id`.
    DeleteAgent { id: String, pubkey: String },
    /// Ask buzzr to change the name and instructions of the local agent `pubkey`; the answer comes back as `updated`.
    UpdateAgent { id: String, pubkey: String, name: String, instructions: String },
    Log { level: String, text: String },
}

/// `Ok(None)` is a message type this buzzr does not know: log it and carry on. `Err` is malformed JSON or a broken message.
pub fn parse(line: &str) -> Result<Option<FromPlugin>, String> {
    if line.len() > MAX_LINE {
        return Err(format!("line of {} bytes is over the {} byte limit", line.len(), MAX_LINE));
    }
    let value: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    let Some(kind) = value.get("type").and_then(Value::as_str) else {
        return Err("no \"type\" field".into());
    };
    let field = |name: &str| value.get(name);
    let obj = |v: &Value, what: &str| serde_json::from_value::<Contributions>(v.clone()).map_err(|e| format!("bad {what}: {e}"));
    let msg = match kind {
        "ready" => {
            let contributions = match field("contributions") {
                Some(v) => obj(v, "contributions")?,
                None => Contributions::default(),
            };
            FromPlugin::Ready { declaration: declaration(&value)?, contributions: contributions.clean() }
        }
        "status" => {
            // `{"type":"status","text":"2 open PRs"}` or `{"type":"status","segments":[...]}`; no text clears the item.
            let segments = match field("segments") {
                Some(v) => serde_json::from_value::<Vec<Segment>>(v.clone()).map_err(|e| format!("bad status segments: {e}"))?,
                None => field("text").and_then(Value::as_str).map(|t| vec![Segment::new(clean_line(t), Default::default())]).unwrap_or_default(),
            };
            FromPlugin::Status { segments: clean_segments(segments) }
        }
        "pane" => {
            // `{"type":"pane","title":"…","rows":[…]}` replaces it; `{"type":"pane"}` (or only empty rows) clears it.
            let pane: Pane = serde_json::from_value(value.clone()).map_err(|e| format!("bad pane: {e}"))?;
            let pane = pane.clean();
            FromPlugin::Pane(if pane.is_empty() { None } else { Some(pane) })
        }
        "panel" => {
            let mut panel: Panel = serde_json::from_value(value.clone()).map_err(|e| format!("bad panel: {e}"))?;
            panel.title = clean_line(&panel.title);
            panel.rows.truncate(MAX_ROWS);
            for row in &mut panel.rows {
                row.text = clean_line(&row.text);
                row.action = row.action.as_deref().map(|a| clean_line(a).chars().take(256).collect::<String>()).filter(|a| !a.is_empty());
            }
            panel.rows.retain(|r| !r.text.trim().is_empty());
            if panel.id.is_empty() {
                return Err("a panel needs an id".into());
            }
            FromPlugin::Panel(panel)
        }
        "flash" => FromPlugin::Flash { text: clean_line(field("text").and_then(Value::as_str).unwrap_or_default()) },
        "draft" => FromPlugin::Draft { text: clean_block(field("text").and_then(Value::as_str).unwrap_or_default()) },
        "send" => FromPlugin::Send {
            channel: field("channel").and_then(Value::as_str).map(|c| clean_line(c)),
            text: clean_block(field("text").and_then(Value::as_str).unwrap_or_default()),
        },
        "prompt" => {
            let id = clean_line(field("id").and_then(Value::as_str).unwrap_or_default());
            if id.is_empty() {
                return Err("a prompt needs an id".into());
            }
            FromPlugin::Prompt {
                id,
                title: clean_line(field("title").and_then(Value::as_str).unwrap_or_default()),
                hint: clean_line(field("hint").and_then(Value::as_str).unwrap_or_default()),
                multiline: field("multiline").and_then(Value::as_bool).unwrap_or(false),
            }
        }
        "list_agents" => {
            let id = clean_line(field("id").and_then(Value::as_str).unwrap_or_default());
            if id.is_empty() {
                return Err("a list_agents needs an id".into());
            }
            FromPlugin::ListAgents { id }
        }
        "probe_harness" => {
            let id = clean_line(field("id").and_then(Value::as_str).unwrap_or_default());
            if id.is_empty() {
                return Err("a probe_harness needs an id".into());
            }
            FromPlugin::ProbeHarness {
                id,
                harness: clean_line(field("harness").and_then(Value::as_str).unwrap_or_default()),
                model: field("model").and_then(Value::as_str).map(clean_line).filter(|m| !m.is_empty()),
            }
        }
        "create_agent" => {
            let id = clean_line(field("id").and_then(Value::as_str).unwrap_or_default());
            if id.is_empty() {
                return Err("a create_agent needs an id".into());
            }
            FromPlugin::CreateAgent {
                id,
                name: clean_line(field("name").and_then(Value::as_str).unwrap_or_default()),
                instructions: clean_block(field("instructions").and_then(Value::as_str).unwrap_or_default()),
                harness: clean_line(field("harness").and_then(Value::as_str).unwrap_or_default()),
                model: clean_line(field("model").and_then(Value::as_str).unwrap_or_default()),
                effort: clean_line(field("effort").and_then(Value::as_str).unwrap_or_default()),
            }
        }
        "log" => FromPlugin::Log {
            level: clean_line(field("level").and_then(Value::as_str).unwrap_or("info")),
            text: clean_line(field("text").and_then(Value::as_str).unwrap_or_default()),
        },
        "delete_agent" => {
            let id = clean_line(field("id").and_then(Value::as_str).unwrap_or_default());
            if id.is_empty() {
                return Err("a delete_agent needs an id".into());
            }
            FromPlugin::DeleteAgent {
                id,
                pubkey: clean_line(field("pubkey").and_then(Value::as_str).unwrap_or_default()),
            }
        }
        "update_agent" => {
            let id = clean_line(field("id").and_then(Value::as_str).unwrap_or_default());
            if id.is_empty() {
                return Err("an update_agent needs an id".into());
            }
            FromPlugin::UpdateAgent {
                id,
                pubkey: clean_line(field("pubkey").and_then(Value::as_str).unwrap_or_default()),
                name: clean_line(field("name").and_then(Value::as_str).unwrap_or_default()),
                instructions: clean_block(field("instructions").and_then(Value::as_str).unwrap_or_default()),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(msg))
}

/// Read the fields a plugin declares about itself out of a `ready` message. What it asks to do is filtered against what
/// this build enforces, so a plugin can never claim more than buzzr grants.
fn declaration(value: &Value) -> Result<Declaration, String> {
    let name = value.get("name").and_then(Value::as_str).map(clean_line).unwrap_or_default().trim().to_string();
    let mut permissions: Vec<String> = match value.get("permissions") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("bad permissions: {e}"))?,
        None => Vec::new(),
    };
    permissions.retain(|p| discover::KNOWN_PERMISSIONS.contains(&p.as_str()));
    permissions.sort();
    permissions.dedup();
    let asked = value.get("tickMs").and_then(Value::as_u64).unwrap_or(0);
    let tick_ms = if asked == 0 { 0 } else { asked.clamp(discover::MIN_TICK_MS, discover::MAX_TICK_MS) };
    Ok(Declaration { name, permissions, tick_ms })
}

fn clean_segments(segments: Vec<Segment>) -> Vec<Segment> {
    segments.into_iter().take(32).map(Segment::clean).filter(|s| !s.text.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_speaks_one_json_object_per_line() {
        let hello = ToPlugin::Hello { protocol: PROTOCOL, api_version: API_VERSION, host: "buzzr/0.1.0", plugin: "todo" }.line();
        assert!(hello.starts_with(r#"{"type":"hello""#), "{hello}");
        assert!(hello.contains(r#""apiVersion":2"#), "the wire name matches the contract version: {hello}");
        assert!(!hello.contains('\n'));
        let p = Projection { me: "me".into(), connection: "live".into(), view: "channel".into(), unread: 2, ..Projection::default() };
        let snap = ToPlugin::Snapshot { seq: 3, projection: &p }.line();
        assert!(snap.contains(r#""type":"snapshot""#) && snap.contains(r#""seq":3"#));
    }

    #[test]
    fn a_pane_replaces_itself_and_an_empty_one_clears_it() {
        let line = r#"{"type":"pane","title":"Clock","rows":[{"text":"12:00","tone":"accent"}]}"#;
        let Some(FromPlugin::Pane(Some(pane))) = parse(line).unwrap() else { panic!("not a pane") };
        assert_eq!(pane.title, "Clock");
        assert_eq!(pane.rows[0].text, "12:00");
        assert_eq!(pane.rows[0].tone, crate::contrib::Tone::Accent);
        // an empty pane is the same as none: it takes the space back
        assert!(matches!(parse(r#"{"type":"pane"}"#).unwrap(), Some(FromPlugin::Pane(None))));
        assert!(matches!(parse(r#"{"type":"pane","title":"","rows":[]}"#).unwrap(), Some(FromPlugin::Pane(None))));
        // a row is a single line, and its control characters never survive
        let Some(FromPlugin::Pane(Some(pane))) = parse("{\"type\":\"pane\",\"rows\":[{\"text\":\"a\\nb\\u001b[31m\"}]}").unwrap() else { panic!() };
        assert_eq!(pane.rows[0].text, "a b[31m");
    }

    #[test]
    fn the_heartbeat_is_one_line_of_json() {
        assert_eq!(ToPlugin::Tick { seq: 7 }.line(), r#"{"type":"tick","seq":7}"#);
    }

    #[test]
    fn a_ready_cleans_what_it_brings() {
        let line = r#"{"type":"ready","contributions":{"commands":[{"id":"todo","title":"TODO \u001b[31m"}],"status":[{"text":"1 \u0007item","tone":"warn"}],"unknown_field":true}}"#;
        let Some(FromPlugin::Ready { contributions, .. }) = parse(line).unwrap() else { panic!("not ready") };
        assert_eq!(contributions.status[0].text, "1 item");
        assert_eq!(contributions.status[0].tone, crate::contrib::Tone::Warn);
        assert_eq!(contributions.commands[0].title, "TODO [31m");
    }

    #[test]
    fn a_ready_declares_the_name_permissions_and_heartbeat_in_code() {
        // the name, permissions and heartbeat all arrive in `ready`, except that a plugin can only ask for what this build enforces
        let line = r#"{"type":"ready","name":"  My Plugin \u001b[31m ","permissions":["messages:write","root","read:visible"],"tickMs":1000}"#;
        let Some(FromPlugin::Ready { declaration, .. }) = parse(line).unwrap() else { panic!("not ready") };
        assert_eq!(declaration.name, "My Plugin [31m");
        assert_eq!(declaration.permissions, vec!["messages:write", "read:visible"], "root is not implemented, so it is dropped");
        assert_eq!(declaration.tick_ms, 1000);

        // absent fields are the defaults, and a heartbeat is clamped to what buzzr will honour
        let Some(FromPlugin::Ready { declaration, .. }) = parse(r#"{"type":"ready"}"#).unwrap() else { panic!() };
        assert_eq!(declaration, Declaration::default());
        let Some(FromPlugin::Ready { declaration, .. }) = parse(r#"{"type":"ready","tickMs":1}"#).unwrap() else { panic!() };
        assert_eq!(declaration.tick_ms, discover::MIN_TICK_MS);
        let Some(FromPlugin::Ready { declaration, .. }) = parse(r#"{"type":"ready","tickMs":999999}"#).unwrap() else { panic!() };
        assert_eq!(declaration.tick_ms, discover::MAX_TICK_MS);
        assert!(parse(r#"{"type":"ready","permissions":"read:visible"}"#).is_err(), "permissions is a list");
    }

    #[test]
    fn status_can_be_text_or_segments_and_empty_clears_it() {
        let Some(FromPlugin::Status { segments }) = parse(r#"{"type":"status","text":"2 open"}"#).unwrap() else { panic!() };
        assert_eq!(segments[0].text, "2 open");
        let Some(FromPlugin::Status { segments }) = parse(r#"{"type":"status"}"#).unwrap() else { panic!() };
        assert!(segments.is_empty());
        let Some(FromPlugin::Status { segments }) = parse(r#"{"type":"status","segments":[{"text":"a","tone":"ok"},{"text":""}]}"#).unwrap()
        else {
            panic!()
        };
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].tone, crate::contrib::Tone::Ok);
    }

    #[test]
    fn panel_rows_are_cleaned_and_actions_are_capped() {
        let line = format!(
            r#"{{"type":"panel","id":"p","title":"T","rows":[{{"text":"one\u001b[2J","action":"open:{}"}},{{"text":"  "}}]}}"#,
            "x".repeat(400)
        );
        let Some(FromPlugin::Panel(panel)) = parse(&line).unwrap() else { panic!() };
        assert_eq!(panel.rows.len(), 1);
        assert_eq!(panel.rows[0].text, "one[2J");
        assert_eq!(panel.rows[0].action.as_deref().map(str::len), Some(256));
    }

    #[test]
    fn contributions_use_the_same_camel_case_names_as_the_rest_of_the_wire() {
        let line = r#"{"type":"ready","contributions":{"messageMarks":[{"target":"m1","text":"PR"}],"channelMarks":[{"target":"c1","text":"3"}]}}"#;
        let Some(FromPlugin::Ready { contributions, .. }) = parse(line).unwrap() else { panic!() };
        assert_eq!(contributions.message_marks[0].target, "m1");
        assert_eq!(contributions.channel_marks[0].text, "3");
        // the snake_case spelling is unknown, so it is ignored rather than half-understood
        let Some(FromPlugin::Ready { contributions, .. }) = parse(r#"{"type":"ready","contributions":{"message_marks":[{"target":"m1","text":"PR"}]}}"#).unwrap()
        else {
            panic!()
        };
        assert!(contributions.message_marks.is_empty(), "the wire uses camelCase");
    }

    #[test]
    fn garbage_is_an_error_and_unknown_types_are_not() {
        assert!(parse("not json").is_err());
        assert!(parse(r#"{"type":"from-the-future","x":1}"#).unwrap().is_none());
        assert!(parse(r#"{"no":"type"}"#).is_err());
        assert!(parse(r#"{"type":"panel","title":"no id"}"#).is_err());
        assert!(parse(&format!(r#"{{"type":"status","text":"{}"}}"#, "x".repeat(MAX_LINE))).is_err());
    }

    #[test]
    fn a_panel_without_rows_is_allowed() {
        let Some(FromPlugin::Panel(p)) = parse(r#"{"type":"panel","id":"empty","title":"Nothing yet"}"#).unwrap() else { panic!() };
        assert!(p.rows.is_empty());
        assert_eq!(p.title, "Nothing yet");
    }

    #[test]
    fn log_and_flash_text_is_stripped_too() {
        let Some(FromPlugin::Flash { text }) = parse(r#"{"type":"flash","text":"hi\u001b]52;c;QQ==\u0007"}"#).unwrap() else { panic!() };
        assert_eq!(text, "hi]52;c;QQ==");
    }

    #[test]
    fn a_probe_and_a_create_carry_the_harness_and_the_picks() {
        let Some(FromPlugin::ProbeHarness { id, harness, model }) =
            parse(r#"{"type":"probe_harness","id":"offer","harness":"claude-agent-acp"}"#).unwrap()
        else {
            panic!("not a probe")
        };
        assert_eq!(id, "offer");
        assert_eq!(harness, "claude-agent-acp");
        assert_eq!(model, None, "no model is the first look");
        // a model is asked for on the second look, and control characters never survive
        let Some(FromPlugin::ProbeHarness { model, .. }) =
            parse("{\"type\":\"probe_harness\",\"id\":\"levels\",\"harness\":\"claude-agent-acp\",\"model\":\"opus\\u001b[31m\"}").unwrap()
        else {
            panic!("not a probe")
        };
        assert_eq!(model.as_deref(), Some("opus[31m"));
        let Some(FromPlugin::ProbeHarness { model, .. }) = parse(r#"{"type":"probe_harness","id":"x","model":""}"#).unwrap() else { panic!() };
        assert_eq!(model, None, "an empty model is no model at all");
        assert!(parse(r#"{"type":"probe_harness","harness":"x"}"#).is_err(), "a probe needs an id");

        let Some(FromPlugin::CreateAgent { harness, model, effort, .. }) =
            parse(r#"{"type":"create_agent","id":"new","name":"Scout","instructions":"watch","harness":"claude-agent-acp","model":"opus","effort":"high"}"#)
                .unwrap()
        else {
            panic!("not a create")
        };
        assert_eq!((harness.as_str(), model.as_str(), effort.as_str()), ("claude-agent-acp", "opus", "high"));
        // the picks are optional: a create that names none is still a create
        let Some(FromPlugin::CreateAgent { harness, model, effort, .. }) = parse(r#"{"type":"create_agent","id":"new"}"#).unwrap() else {
            panic!()
        };
        assert!(harness.is_empty() && model.is_empty() && effort.is_empty());
    }

    #[test]
    fn a_harness_offer_is_the_offer_or_the_reason() {
        let offer = crate::plugin::HarnessOffer {
            models: vec![crate::plugin::Choice { value: "opus".into(), label: "Opus".into() }],
            model: "opus".into(),
            efforts: vec![crate::plugin::Choice { value: "high".into(), label: "high".into() }],
            effort: "high".into(),
        };
        let line = ToPlugin::HarnessOffer { id: "offer", offer: Some(&offer), error: None }.line();
        assert!(line.contains(r#""type":"harness_offer""#) && line.contains(r#""offer":{"models""#), "{line}");
        let line = ToPlugin::HarnessOffer { id: "offer", offer: None, error: Some("claude-agent-acp is not installed") }.line();
        assert!(line.contains(r#""error":"claude-agent-acp is not installed""#) && !line.contains(r#""offer":"#), "{line}");
    }

    #[test]
    fn an_update_agent_carries_the_new_name_and_instructions_and_its_answer_the_name() {
        let Some(FromPlugin::UpdateAgent { id, pubkey, name, instructions }) =
            parse(r#"{"type":"update_agent","id":"edit","pubkey":"ab","name":"Scout","instructions":"watch"}"#).unwrap()
        else {
            panic!("not an update")
        };
        assert_eq!((id.as_str(), pubkey.as_str(), name.as_str(), instructions.as_str()), ("edit", "ab", "Scout", "watch"));
        assert!(parse(r#"{"type":"update_agent","pubkey":"ab"}"#).is_err(), "an update needs an id");
        let line = ToPlugin::Updated { id: "edit", name: Some("Scout"), error: None }.line();
        assert!(line.contains(r#""type":"updated""#) && line.contains(r#""name":"Scout""#), "{line}");
    }

    #[test]
    fn a_delete_agent_carries_the_pubkey_and_its_answer_the_name() {
        let Some(FromPlugin::DeleteAgent { id, pubkey }) =
            parse("{\"type\":\"delete_agent\",\"id\":\"delete\",\"pubkey\":\"00ff\\u001b[31m\"}").unwrap()
        else {
            panic!("not a delete")
        };
        assert_eq!(id, "delete");
        assert_eq!(pubkey, "00ff[31m", "control characters never survive");
        // a delete with no id cannot be answered, so it is refused
        assert!(parse(r#"{"type":"delete_agent","pubkey":"ab"}"#).is_err(), "a delete needs an id");
        // the answer carries the name of the agent that was deleted, or why it was not
        let line = ToPlugin::Deleted { id: "delete", name: Some("Scout"), error: None }.line();
        assert!(line.contains(r#""type":"deleted""#) && line.contains(r#""name":"Scout""#), "{line}");
        let line = ToPlugin::Deleted { id: "delete", name: None, error: Some("the relay returned 500") }.line();
        assert!(line.contains(r#""error":"the relay returned 500""#) && !line.contains(r#""name":"#), "{line}");
    }
}
