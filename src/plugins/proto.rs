//! The wire between buzzr and a plugin process: one JSON object per line, both ways.
//!
//! Host to plugin: `hello`, `snapshot`, `invoke`, `panel_action`, `bye`.
//! Plugin to host: `ready`, `status`, `panel`, `flash`, `draft`, `send`, `log`.
//! Unknown message types and unknown fields are ignored (and logged), so a newer plugin can talk to an older buzzr; an unknown
//! `apiVersion` in the manifest is refused instead, because that is a contract change.

use crate::contrib::{clean_block, clean_line, Contributions, Panel, Projection, Segment, MAX_ROWS};
use serde::Serialize;
use serde_json::Value;

pub const PROTOCOL: u32 = 1;
pub const API_VERSION: u32 = 1;
/// A line longer than this is a protocol violation, not a message.
pub const MAX_LINE: usize = 64 * 1024;

/// Host to plugin. One of these is serialised per line.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPlugin<'a> {
    Hello { protocol: u32, #[serde(rename = "apiVersion")] api_version: u32, host: &'a str, plugin: &'a str },
    Snapshot { seq: u64, projection: &'a Projection },
    Invoke { id: u64, command: &'a str, args: &'a str },
    PanelAction { id: &'a str, row: usize, value: Option<&'a str> },
    Bye,
}

impl ToPlugin<'_> {
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("a message of plain data serialises")
    }
}

/// Plugin to host, after parsing and cleaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromPlugin {
    Ready { contributions: Contributions },
    /// Replaces the plugin's status item; empty segments clear it.
    Status { segments: Vec<Segment> },
    Panel(Panel),
    Flash { text: String },
    Draft { text: String },
    Send { channel: Option<String>, text: String },
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
            FromPlugin::Ready { contributions: contributions.clean() }
        }
        "status" => {
            // `{"type":"status","text":"2 open PRs"}` or `{"type":"status","segments":[...]}`; no text clears the item.
            let segments = match field("segments") {
                Some(v) => serde_json::from_value::<Vec<Segment>>(v.clone()).map_err(|e| format!("bad status segments: {e}"))?,
                None => field("text").and_then(Value::as_str).map(|t| vec![Segment::new(clean_line(t), Default::default())]).unwrap_or_default(),
            };
            FromPlugin::Status { segments: clean_segments(segments) }
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
        "log" => FromPlugin::Log {
            level: clean_line(field("level").and_then(Value::as_str).unwrap_or("info")),
            text: clean_line(field("text").and_then(Value::as_str).unwrap_or_default()),
        },
        _ => return Ok(None),
    };
    Ok(Some(msg))
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
        assert!(hello.contains(r#""apiVersion":1"#), "the wire name matches the manifest: {hello}");
        assert!(!hello.contains('\n'));
        let p = Projection { me: "me".into(), connection: "live".into(), view: "channel".into(), unread: 2, ..Projection::default() };
        let snap = ToPlugin::Snapshot { seq: 3, projection: &p }.line();
        assert!(snap.contains(r#""type":"snapshot""#) && snap.contains(r#""seq":3"#));
    }

    #[test]
    fn a_ready_cleans_what_it_brings() {
        let line = r#"{"type":"ready","contributions":{"commands":[{"id":"todo","title":"TODO \u001b[31m"}],"status":[{"text":"1 \u0007item","tone":"warn"}],"unknown_field":true}}"#;
        let Some(FromPlugin::Ready { contributions }) = parse(line).unwrap() else { panic!("not ready") };
        assert_eq!(contributions.status[0].text, "1 item");
        assert_eq!(contributions.status[0].tone, crate::contrib::Tone::Warn);
        assert_eq!(contributions.commands[0].title, "TODO [31m");
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
    fn contributions_use_the_same_camel_case_names_as_the_manifest() {
        let line = r#"{"type":"ready","contributions":{"messageMarks":[{"target":"m1","text":"PR"}],"channelMarks":[{"target":"c1","text":"3"}]}}"#;
        let Some(FromPlugin::Ready { contributions }) = parse(line).unwrap() else { panic!() };
        assert_eq!(contributions.message_marks[0].target, "m1");
        assert_eq!(contributions.channel_marks[0].text, "3");
        // the snake_case spelling is unknown, so it is ignored rather than half-understood
        let Some(FromPlugin::Ready { contributions }) = parse(r#"{"type":"ready","contributions":{"message_marks":[{"target":"m1","text":"PR"}]}}"#).unwrap()
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
}
