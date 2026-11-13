//! A minimal ACP client: start a harness, `initialize`, open a session, read what it offers, and run a turn.
//!
//! The Agent Client Protocol is JSON-RPC 2.0, one object per line, over the harness's standard input and output. buzzr
//! uses it for two things, and both begin the same way: `initialize`, then `session/new`, whose answer lists the
//! harness's `configOptions` — the models it can run and the thinking levels of the model it starts on.
//! `session/set_config_option` puts the session on a model, which is how the levels that come back are that model's.
//!
//! * [`probe`] reads that much and stops: it is what `/agents create` shows before it makes an agent, and the "does
//!   this harness work?" check. The process is killed as soon as the answer is read.
//! * [`Session`] keeps the process and runs one turn over it: `session/prompt` answers with the reply the harness
//!   streams as `agent_message_chunk` updates, and `session/request_permission` is answered as the harness asks (with
//!   the option that allows the call). That is how a local agent answers a mention (see `runner.rs`).
//!
//! (The Buzz app's ACP client reads the same fields, under `session/new`, `session/set_config_option`,
//! `session/prompt` and `session/update`.)

use crate::plugin::{Choice, HarnessOffer};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// How long to wait for one answer before giving up on the harness.
const TIMEOUT: Duration = Duration::from_secs(20);
/// How long a harness has to start and open a session. Generous: a harness that runs through `node` takes a while to
/// come up, and starting one is not the turn itself.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);
/// How long one turn may take. A turn is the model working, which can be minutes, so this is a ceiling on a harness
/// that has stopped answering rather than a deadline for the model.
const TURN_TIMEOUT: Duration = Duration::from_secs(600);
/// The ACP protocol version buzzr speaks.
const PROTOCOL_VERSION: u32 = 1;
/// The model config, when a harness names it by id rather than by `category`.
const MODEL_IDS: &[&str] = &["model"];
/// The thinking-level config, likewise (`thought_level` is the category; harnesses spell the id differently).
const EFFORT_IDS: &[&str] = &["thought_level", "thinking_effort"];

/// Start `program`, ask it to open a session in `cwd`, and read what it offers. `missing` is the message when the
/// program cannot be started at all — it is not installed. Blocking; the process is killed before this returns.
pub fn probe(program: &str, cwd: &Path, model: Option<&str>, missing: &str) -> Result<HarnessOffer, String> {
    let mut wire = Wire::spawn(program, cwd, missing)?;
    converse(&mut wire, cwd, model) // dropping the wire kills the harness
}

/// Open the session and read what it offers: `initialize`, `session/new`, and — when a model is asked for — put the
/// session on it, so the levels that come back are that model's.
fn converse(wire: &mut Wire, cwd: &Path, model: Option<&str>) -> Result<HarnessOffer, String> {
    let client = json!({ "name": "buzzr", "version": env!("CARGO_PKG_VERSION") });
    wire.call("initialize", json!({ "protocolVersion": PROTOCOL_VERSION, "clientCapabilities": {}, "clientInfo": client }), TIMEOUT)?;
    let opened = wire.call("session/new", json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }), TIMEOUT)?;
    let session = opened.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_owned();
    let (mut models, mut efforts) = groups(&opened);
    // A harness that will not take the model keeps the one it started on: the choice is still recorded, and the probe
    // does not fail for it.
    if let (Some(model), Some(config)) = (model, models.as_ref().map(|g| g.id.clone())) {
        let params = json!({ "sessionId": session, "configId": config, "value": model });
        if let Ok(set) = wire.call("session/set_config_option", params, TIMEOUT) {
            let (m, e) = groups(&set);
            models = m.or(models);
            efforts = e.or(efforts);
        }
    }
    Ok(offer(models, efforts))
}

/// A harness process and the wire to it: one JSON-RPC object per line, written to its standard input and read back
/// from its standard output on a thread of its own. Dropping it kills the process.
#[derive(Debug)]
struct Wire {
    child: std::process::Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next: u64,
}

impl Wire {
    /// Start `program` in `cwd`. `missing` is the message when it cannot be started at all.
    fn spawn(program: &str, cwd: &Path, missing: &str) -> Result<Wire, String> {
        let mut cmd = Command::new(program);
        cmd.current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        // Through `spawn::`: the harness may be a program that was written to disk as we start it (see `crate::spawn`).
        let mut child = crate::spawn::start(|| cmd.spawn()).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => missing.to_owned(),
            _ => format!("could not start {program}: {e}"),
        })?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        // Read on a thread: a harness that never answers would otherwise block a read forever and the timeout could
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
        Ok(Wire { child, stdin, rx, next: 1 })
    }

    /// Write one request and return its id: the answer comes back from `read` carrying that id.
    fn send(&mut self, method: &str, params: Value) -> Result<u64, String> {
        let id = self.next;
        self.next += 1;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        writeln!(self.stdin, "{line}").and_then(|()| self.stdin.flush()).map_err(|e| format!("could not talk to the harness: {e}"))?;
        Ok(id)
    }

    /// Answer something the harness asked us (`session/request_permission`).
    fn reply(&mut self, id: &Value, result: Value) -> Result<(), String> {
        let line = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
        writeln!(self.stdin, "{line}").and_then(|()| self.stdin.flush()).map_err(|e| format!("could not answer the harness: {e}"))
    }

    /// The next line from the harness, parsed. `Ok(None)` is a line that is not JSON (a harness may print its own
    /// noise); an error is the harness going quiet or stopping.
    fn read(&mut self, timeout: Duration) -> Result<Option<Value>, String> {
        match self.rx.recv_timeout(timeout) {
            Ok(line) => Ok(serde_json::from_str(&line).ok()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!("the harness did not answer in {}s", timeout.as_secs())),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("the harness stopped".to_owned()),
        }
    }

    /// Send one request and read until its answer, dropping notifications and other answers on the way.
    fn call(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.send(method, params)?;
        loop {
            let Some(value) = self.read(timeout)? else { continue };
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

impl Drop for Wire {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A harness process with a session open, for running turns: one local agent, answering one message.
///
/// The harness is started once and kept for the session's life; dropping the session kills it. A turn is one
/// `session/prompt`, and its answer is the text the harness streams back as `agent_message_chunk` updates.
#[derive(Debug)]
pub struct Session {
    wire: Wire,
    /// The session's id, as `session/new` named it.
    id: String,
    /// The agent's instructions, to lead the first prompt with: a harness that does not advertise ACP's
    /// `systemPrompt` capability never sees them as a system prompt, and one turn is all the agent gets.
    preamble: String,
}

impl Session {
    /// Start `program` in `cwd` and open a session in it, on `model` and at thinking level `effort` when given (an
    /// empty one is the harness's own choice), with `instructions` as what the agent was told to do. `missing` is the
    /// message when the program cannot be started at all.
    pub fn open(program: &str, cwd: &Path, model: Option<&str>, effort: Option<&str>, instructions: &str, missing: &str) -> Result<Session, String> {
        let mut wire = Wire::spawn(program, cwd, missing)?;
        let client = json!({ "name": "buzzr", "version": env!("CARGO_PKG_VERSION") });
        let init = wire.call("initialize", json!({ "protocolVersion": PROTOCOL_VERSION, "clientCapabilities": {}, "clientInfo": client }), OPEN_TIMEOUT)?;
        // ACP's `systemPrompt` is only sent when the harness says it takes one; otherwise the instructions lead the
        // first prompt instead (the same fallback the Buzz app makes).
        let standard = advertises(&init, "systemPrompt");
        let instructions = instructions.trim();
        let mut params = json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] });
        if standard && !instructions.is_empty() {
            params["systemPrompt"] = json!({ "mode": "append", "content": [{ "type": "text", "text": instructions }] });
        }
        let opened = wire.call("session/new", params, OPEN_TIMEOUT)?;
        let id = opened.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_owned();
        if id.is_empty() {
            return Err("the harness opened no session".to_owned());
        }
        // Put the session on the model and thinking level the agent was made with, when it still lists them. A
        // harness that will not take one keeps its own choice: the turn runs anyway.
        let (models, mut efforts) = groups(&opened);
        if let (Some(model), Some(config)) = (model, models.as_ref().map(|g| g.id.clone())) {
            let params = json!({ "sessionId": id, "configId": config, "value": model });
            if let Ok(set) = wire.call("session/set_config_option", params, OPEN_TIMEOUT) {
                // The levels that come back are the model's, which is the one the agent was made on.
                let (_, e) = groups(&set);
                efforts = e.or(efforts);
            }
        }
        if let (Some(effort), Some(config)) = (effort, efforts.as_ref().map(|g| g.id.clone())) {
            let params = json!({ "sessionId": id, "configId": config, "value": effort });
            let _ = wire.call("session/set_config_option", params, OPEN_TIMEOUT);
        }
        let preamble = if standard { String::new() } else { instructions.to_owned() };
        Ok(Session { wire, id, preamble })
    }

    /// Run one turn: send `text` as the prompt and return the reply the harness streamed back. A harness that asks
    /// permission to use a tool is answered with the option that allows the call, so a turn never stalls on a prompt
    /// nobody is there to answer. `Err` is the harness refusing the prompt, going quiet, or stopping.
    pub fn prompt(&mut self, text: &str) -> Result<String, String> {
        let lead = std::mem::take(&mut self.preamble);
        let text = if lead.is_empty() { text.to_owned() } else { format!("{lead}\n\n{text}") };
        let id = self.wire.send("session/prompt", json!({ "sessionId": self.id, "prompt": [{ "type": "text", "text": text }] }))?;
        let mut written = String::new();
        loop {
            let Some(value) = self.wire.read(TURN_TIMEOUT)? else { continue };
            if let Some(method) = value.get("method").and_then(Value::as_str) {
                match value.get("id") {
                    // The harness asking us something (a tool call needs permission): answer it and keep reading.
                    Some(rid) if !rid.is_null() => {
                        let params = value.get("params").cloned().unwrap_or(Value::Null);
                        if method == "session/request_permission" {
                            let rid = rid.clone();
                            self.wire.reply(&rid, json!({ "outcome": allow(&params) }))?;
                        } else {
                            let rid = rid.clone();
                            self.wire.reply(&rid, json!({ "outcome": { "outcome": "cancelled" } }))?;
                        }
                    }
                    // A notification: the reply is streamed as `agent_message_chunk` updates.
                    _ => {
                        if method == "session/update" {
                            if let Some(part) = chunk(value.get("params").unwrap_or(&Value::Null)) {
                                written.push_str(part);
                            }
                        }
                    }
                }
                continue;
            }
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // an answer to something else
            }
            if let Some(error) = value.get("error") {
                // The prompt never reached the model, so the instructions still lead the next one.
                if !lead.is_empty() && self.preamble.is_empty() {
                    self.preamble = lead;
                }
                let message = error.get("message").and_then(Value::as_str).unwrap_or("no reason given");
                return Err(format!("the harness refused the prompt: {message}"));
            }
            return Ok(written);
        }
    }
}

/// Whether `initialize` advertised `sessionCapabilities[name]` — an object, as ACP has it.
fn advertises(init: &Value, name: &str) -> bool {
    init.get("agentCapabilities")
        .and_then(|c| c.get("sessionCapabilities"))
        .and_then(|c| c.get(name))
        .map(Value::is_object)
        .unwrap_or(false)
}

/// The text in a `session/update`, when it is an `agent_message_chunk` carrying some.
fn chunk(params: &Value) -> Option<&str> {
    let update = params.get("update")?;
    if update.get("sessionUpdate")?.as_str()? != "agent_message_chunk" {
        return None;
    }
    update.get("content")?.get("text")?.as_str()
}

/// The outcome for a `session/request_permission`: the option that allows the call, preferring one that keeps
/// allowing, and `cancelled` when the harness offered no such option.
fn allow(params: &Value) -> Value {
    let options = params.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
    let option = |kind: &str| {
        options
            .iter()
            .find(|o| o.get("kind").and_then(Value::as_str) == Some(kind))
            .and_then(|o| o.get("optionId"))
            .and_then(Value::as_str)
    };
    match option("allow_always").or_else(|| option("allow_once")) {
        Some(id) => json!({ "outcome": "selected", "optionId": id }),
        None => json!({ "outcome": "cancelled" }),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// The fake harness the tests drive: the shell script committed at `tests/fixtures/claude-agent-acp`, copied into
    /// `dir` under `name` and made executable so it can be started by path. The end-to-end suite drives the same file,
    /// so there is one fake to keep in step with the client. The name matters: one ending in `-sysprompt` makes the
    /// fake advertise ACP's `systemPrompt` capability.
    fn copy_acp(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-agent-acp");
        std::fs::copy(fixture, &path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn fake_acp(dir: &Path) -> PathBuf {
        copy_acp(dir, "claude-agent-acp")
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
        let err = probe("definitely-not-a-real-program-xyz", &dir, None, "claude-agent-acp is not installed").unwrap_err();
        assert_eq!(err, "claude-agent-acp is not installed");
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

    #[test]
    fn a_turn_is_the_reply_the_harness_streams_and_the_instructions_lead_it() {
        let dir = tempdir();
        let program = fake_acp(&dir);
        // The fake takes no system prompt, so the instructions lead the first prompt — the reply quotes them back.
        let mut session = Session::open(program.to_str().unwrap(), &dir, None, None, "Be a helper.", "missing").unwrap();
        let reply = session.prompt("hi there").unwrap();
        assert!(reply.contains("heard:"), "the chunks are collected in order: {reply:?}");
        assert!(reply.contains("Be a helper."), "the instructions led the prompt: {reply:?}");
        assert!(reply.contains("hi there"), "and the message followed them: {reply:?}");
        assert!(reply.contains("permission: yes"), "the harness's permission request was answered with the allowing option: {reply:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_instructions_lead_only_the_first_prompt() {
        let dir = tempdir();
        let program = fake_acp(&dir);
        let mut session = Session::open(program.to_str().unwrap(), &dir, None, None, "Be a helper.", "missing").unwrap();
        assert!(session.prompt("first").unwrap().contains("Be a helper."));
        let second = session.prompt("second").unwrap();
        assert!(second.contains("second"), "{second:?}");
        assert!(!second.contains("Be a helper."), "the instructions are not repeated: {second:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_harness_that_takes_a_system_prompt_is_sent_the_instructions_that_way() {
        let dir = tempdir();
        let program = copy_acp(&dir, "claude-agent-acp-sysprompt");
        let mut session = Session::open(program.to_str().unwrap(), &dir, None, None, "Be a helper.", "missing").unwrap();
        let reply = session.prompt("hi").unwrap();
        assert!(reply.contains("[system: Be a helper.]"), "the instructions went in `session/new`: {reply:?}");
        assert!(!reply.contains("Be a helper.\n\n"), "and did not lead the prompt as well: {reply:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_session_on_a_model_and_level_the_agent_was_made_with_still_runs() {
        let dir = tempdir();
        let program = fake_acp(&dir);
        let mut session = Session::open(program.to_str().unwrap(), &dir, Some("opus"), Some("high"), "", "missing").unwrap();
        assert!(session.prompt("hi").unwrap().contains("hi"), "a session put on a model and level still runs a turn");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_session_for_a_program_that_is_not_installed_is_the_missing_message() {
        let dir = tempdir();
        let err = Session::open("definitely-not-a-real-program-xyz", &dir, None, None, "", "claude-agent-acp is not installed").unwrap_err();
        assert_eq!(err, "claude-agent-acp is not installed");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_permission_outcome_allows_the_call_and_prefers_the_one_that_keeps_allowing() {
        let params = json!({"options":[{"optionId":"once","kind":"allow_once"},{"optionId":"always","kind":"allow_always"}]});
        assert_eq!(allow(&params), json!({"outcome":"selected","optionId":"always"}));
        let only_once = json!({"options":[{"optionId":"once","kind":"allow_once"},{"optionId":"no","kind":"reject_once"}]});
        assert_eq!(allow(&only_once), json!({"outcome":"selected","optionId":"once"}));
        assert_eq!(allow(&json!({"options":[]})), json!({"outcome":"cancelled"}), "no allowing option is no permission");
        assert_eq!(allow(&json!({})), json!({"outcome":"cancelled"}));
    }

    #[test]
    fn a_session_capability_is_read_from_the_initialize_answer() {
        assert!(advertises(&json!({"agentCapabilities":{"sessionCapabilities":{"systemPrompt":{}}}}), "systemPrompt"));
        assert!(!advertises(&json!({"agentCapabilities":{}}), "systemPrompt"), "an absent capability is not advertised");
        assert!(!advertises(&json!({"agentCapabilities":{"sessionCapabilities":{"systemPrompt":null}}}), "systemPrompt"), "nor is a null one");
        assert!(!advertises(&json!({}), "systemPrompt"));
    }

    #[test]
    fn only_a_message_chunk_carries_reply_text() {
        let params = |update: Value| json!({ "update": update });
        assert_eq!(
            chunk(&params(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}))),
            Some("hi"),
            "a message chunk's text is the reply"
        );
        assert_eq!(chunk(&params(json!({"sessionUpdate":"agent_thought_chunk","content":{"text":"hm"}}))), None, "thinking is not the reply");
        assert_eq!(chunk(&params(json!({"sessionUpdate":"tool_call","title":"read"}))), None);
        assert_eq!(chunk(&json!({})), None);
    }
}
