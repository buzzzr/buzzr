//! A minimal ACP client: start a harness, `initialize`, open a session, read what it offers.
//!
//! The Agent Client Protocol is JSON-RPC 2.0, one object per line, over the harness's standard input and output. buzzr
//! needs only the opening of a session: `initialize`, then `session/new`, whose answer lists the harness's
//! `configOptions` — the models it can run and the thinking levels of the model it starts on. `session/set_config_option`
//! puts the session on a model, which is how the levels that come back are that model's. (The Buzz app's ACP client
//! reads the same fields, under `session/new` and `session/set_config_option`.)
//!
//! Nothing here is a long-lived session: this is a probe, so the process is stopped once the answer is read.

use crate::plugin::{Choice, HarnessOffer};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// How long to wait for one answer before giving up on the harness.
const TIMEOUT: Duration = Duration::from_secs(20);
/// The ACP protocol version buzzr speaks.
const PROTOCOL_VERSION: u32 = 1;
/// The model config, when a harness names it by id rather than by `category`.
const MODEL_IDS: &[&str] = &["model"];
/// The thinking-level config, likewise (`thought_level` is the category; harnesses spell the id differently).
const EFFORT_IDS: &[&str] = &["thought_level", "thinking_effort"];

/// Start `program`, ask it to open a session in `cwd`, and read what it offers. `missing` is the message when the
/// program cannot be started at all — it is not installed. Blocking; the process is killed before this returns.
pub fn probe(program: &str, cwd: &Path, model: Option<&str>, missing: &str) -> Result<HarnessOffer, String> {
    let mut child = Command::new(program)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => missing.to_owned(),
            _ => format!("could not start {program}: {e}"),
        })?;
    let stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    // Read on a thread: a harness that never answers would otherwise block a read forever and the timeout below could
    // never fire. Killing the child closes the pipe, which ends the thread.
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
            if tx.send(std::mem::take(&mut line)).is_err() {
                break;
            }
        }
    });
    let result = converse(stdin, rx, cwd, model);
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// Open the session and read what it offers: `initialize`, `session/new`, and — when a model is asked for — put the
/// session on it, so the levels that come back are that model's.
fn converse(stdin: ChildStdin, rx: Receiver<String>, cwd: &Path, model: Option<&str>) -> Result<HarnessOffer, String> {
    let mut acp = Acp { stdin, rx, next: 1 };
    let client = json!({ "name": "buzzr", "version": env!("CARGO_PKG_VERSION") });
    acp.call("initialize", json!({ "protocolVersion": PROTOCOL_VERSION, "clientCapabilities": {}, "clientInfo": client }))?;
    let opened = acp.call("session/new", json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }))?;
    let session = opened.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_owned();
    let (mut models, mut efforts) = groups(&opened);
    // A harness that will not take the model keeps the one it started on: the choice is still recorded, and the probe
    // does not fail for it.
    if let (Some(model), Some(config)) = (model, models.as_ref().map(|g| g.id.clone())) {
        let params = json!({ "sessionId": session, "configId": config, "value": model });
        if let Ok(set) = acp.call("session/set_config_option", params) {
            let (m, e) = groups(&set);
            models = m.or(models);
            efforts = e.or(efforts);
        }
    }
    Ok(offer(models, efforts))
}

/// One config the harness listed: its id (to set it), the value it is on, and every value it offers.
struct Group {
    id: String,
    current: String,
    values: Vec<Choice>,
}

/// The model and thinking-level configs out of a `session/new` or `session/set_config_option` answer. A harness may
/// name a config by `category` or only by `id`, so both are tried.
fn groups(value: &Value) -> (Option<Group>, Option<Group>) {
    let mut model = None;
    let mut effort = None;
    for item in value.get("configOptions").and_then(Value::as_array).into_iter().flatten() {
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let category = item.get("category").and_then(Value::as_str).unwrap_or_default();
        let group = Group {
            current: item.get("currentValue").and_then(Value::as_str).unwrap_or_default().to_owned(),
            values: choices(item.get("options")),
            id,
        };
        if (category == "model" || MODEL_IDS.contains(&group.id.as_str())) && model.is_none() {
            model = Some(group);
        } else if (category == "thought_level" || EFFORT_IDS.contains(&group.id.as_str())) && effort.is_none() {
            effort = Some(group);
        }
    }
    (model, effort)
}

/// Every choice in a `configOptions[].options` list, groups flattened (a model list may be grouped by family).
fn choices(options: Option<&Value>) -> Vec<Choice> {
    fn walk(value: &Value, out: &mut Vec<Choice>) {
        for item in value.as_array().into_iter().flatten() {
            if let Some(value) = item.get("value").and_then(Value::as_str) {
                let label = item.get("name").and_then(Value::as_str).unwrap_or(value);
                out.push(Choice { value: value.to_owned(), label: label.to_owned() });
            }
            if let Some(nested) = item.get("options") {
                walk(nested, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(value) = options {
        walk(value, &mut out);
    }
    out
}

fn offer(model: Option<Group>, effort: Option<Group>) -> HarnessOffer {
    HarnessOffer {
        models: model.as_ref().map(|g| g.values.clone()).unwrap_or_default(),
        model: model.as_ref().map(|g| g.current.clone()).unwrap_or_default(),
        efforts: effort.as_ref().map(|g| g.values.clone()).unwrap_or_default(),
        effort: effort.as_ref().map(|g| g.current.clone()).unwrap_or_default(),
    }
}

/// The wire: one request at a time, reading past notifications and other answers.
struct Acp {
    stdin: ChildStdin,
    rx: Receiver<String>,
    next: u64,
}

impl Acp {
    /// Send one request and read until its answer.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        writeln!(self.stdin, "{line}").and_then(|()| self.stdin.flush()).map_err(|e| format!("could not talk to the harness: {e}"))?;
        loop {
            let line = self.rx.recv_timeout(TIMEOUT).map_err(|_| format!("the harness did not answer {method}"))?;
            let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // a notification, or an answer to something else
            }
            if let Some(error) = value.get("error") {
                let message = error.get("message").and_then(Value::as_str).unwrap_or("no reason given");
                return Err(format!("the harness refused {method}: {message}"));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// The fake harness the tests drive: the shell script committed at `tests/fixtures/claude-code-acp`, copied into
    /// `dir` and made executable so it can be started by path. The end-to-end suite drives the same file, so there is
    /// one fake to keep in step with the client.
    fn fake_acp(dir: &Path) -> PathBuf {
        let path = dir.join("claude-code-acp");
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code-acp");
        std::fs::copy(fixture, &path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("buzzr-acp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn values(choices: &[Choice]) -> Vec<&str> {
        choices.iter().map(|c| c.value.as_str()).collect()
    }

    #[test]
    fn a_probe_reads_the_models_and_levels_a_harness_offers() {
        let dir = tempdir();
        let program = fake_acp(&dir);
        let offer = probe(program.to_str().unwrap(), &dir, None, "missing").unwrap();
        assert_eq!(values(&offer.models), vec!["sonnet", "opus"]);
        assert_eq!(offer.models[0].label, "Sonnet", "a choice's name is its label");
        assert_eq!(offer.model, "sonnet", "the model it starts on");
        assert_eq!(values(&offer.efforts), vec!["low", "medium", "high"]);
        assert_eq!(offer.efforts[1].label, "medium", "a choice with no name is labelled by its value");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn putting_the_session_on_a_model_reads_that_model_s_levels() {
        let dir = tempdir();
        let program = fake_acp(&dir);
        let offer = probe(program.to_str().unwrap(), &dir, Some("opus"), "missing").unwrap();
        assert_eq!(offer.model, "opus", "the chosen model comes back as the current one");
        assert_eq!(values(&offer.efforts), vec!["low", "high"], "and its levels are the ones offered");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_program_that_is_not_installed_is_the_missing_message() {
        let dir = tempdir();
        let err = probe("definitely-not-a-real-program-xyz", &dir, None, "claude-code-acp is not installed").unwrap_err();
        assert_eq!(err, "claude-code-acp is not installed");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn config_options_are_read_by_category_or_by_id_and_groups_are_flattened() {
        let value = json!({"configOptions":[
            {"id":"model","category":"model","currentValue":"a","options":[{"value":"a","name":"A"},{"options":[{"value":"b","name":"B"}]}]},
            {"id":"thinking_effort","currentValue":"low","options":[{"value":"low"}]}
        ]});
        let (model, effort) = groups(&value);
        let model = model.unwrap();
        assert_eq!(model.id, "model");
        assert_eq!(model.current, "a");
        assert_eq!(values(&model.values), vec!["a", "b"], "a grouped list is flattened");
        assert_eq!(effort.unwrap().id, "thinking_effort", "a level config with no category is found by its id");
    }

    #[test]
    fn an_answer_with_no_options_is_an_empty_offer() {
        let (model, effort) = groups(&json!({}));
        assert!(model.is_none() && effort.is_none());
        let offer = offer(None, None);
        assert_eq!(offer, HarnessOffer::default());
    }
}
