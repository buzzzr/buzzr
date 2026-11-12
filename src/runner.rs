//! The runner: the local agents, answering the mentions that name them.
//!
//! `agents.rs` makes an agent's identity and `harness/` can run one turn over ACP, but nothing was running an agent:
//! a message that mentioned one went unanswered. This is the host. It watches `agents.json`, and for every agent it
//! finds it starts a watcher that asks the relay for the messages naming the agent, runs the harness on each one, and
//! posts what the harness answered back to the channel the mention came from.
//!
//! **The person's relay, the agent's key.** An agent answers on the relay the person is connected to (`base_url`) and
//! signs with its own key, so the answer is the agent's on the wire: its own NIP-98 request, carrying the owner's
//! NIP-OA attestation for it (the same attestation its profile has), which is how the relay knows the agent is
//! acting for its owner.
//!
//! **A mention is a message that names the agent** — one of the message kinds with the agent in a `p` tag, in a
//! channel. A message the agent wrote itself is skipped: an answer names the person who asked, never the agent, so an
//! answer can never wake its own author. The watch begins when it begins: a mention that arrived while buzzr was not
//! running is not answered (there is no stored position yet).
//!
//! **One session, kept between mentions.** A turn is one `session/prompt` over a harness started in the agent's own
//! folder, so every mention is answered by an agent that has what it was told to do and remembers the conversation it
//! is in. The session is opened on the first mention and kept for the next; a harness that fails or stops is dropped,
//! and the mention after it opens a fresh one. Opening a session and running a turn both block on the harness, so
//! they run on the blocking pool, never on the event loop (principle 6 in `AGENTS.md`).
//!
//! **The answer is the harness's own text**, posted as the agent's message: a reply in the mention's thread when the
//! mention was one, and a top-level message in the channel when it was not, naming the person who asked so they are
//! told. The Buzz desktop hands its harness a tool to publish with; a harness here speaks plain ACP with no tool
//! server, so the text the turn streams back *is* the reply (`ARCHITECTURE.md`).

use crate::agents;
use crate::app::AppEvent;
use crate::config::Paths;
use crate::harness::{self, Harness};
use crate::model::{Ev, MESSAGE_KINDS};
use crate::relay::{self, Relay};
use nostr::Keys;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

/// How long between two asks for new mentions.
const POLL: Duration = Duration::from_secs(2);
/// How long between two reads of `agents.json`: short enough that an agent made in this run starts answering straight
/// away, long enough that it is not a file read per keystroke.
const SCAN: Duration = Duration::from_secs(2);
/// The most mentions one ask reads. The relay answers newest first, so a window busier than this is read down to its
/// newest limit and the older mentions in it are left behind when the cursor moves past them: a mention is one person
/// naming one agent, and a flood of them at one agent is bounded rather than worked through as a backlog.
const LIMIT: u32 = 50;

/// Run the local agents: one task that watches `agents.json` and starts a watcher per agent, and one watcher per
/// agent that answers its mentions. Every answer is signed with the agent's own key and goes to the relay the person
/// is on; each turn's outcome is reported to the app as [`AppEvent::AgentTurn`].
///
/// Only call this when buzzr may send (not read-only, not `BUZZR_SAFE_MODE`): an agent that answers a mention is
/// sending a message.
pub fn spawn(relay: Arc<Relay>, paths: Paths, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        // One watcher per agent, keyed by the agent's key: that is what makes this idempotent — an agent still in
        // the file keeps the watcher it has. A record that goes away (the file is edited by hand) loses its watcher,
        // and the session goes with it, which stops the harness.
        let mut running: HashMap<String, JoinHandle<()>> = HashMap::new();
        loop {
            let agents = agents::load_with_keys(&paths.agents());
            let wanted: Vec<String> = agents.iter().map(|(a, _)| a.pubkey.clone()).collect();
            running.retain(|pubkey, task| {
                if wanted.iter().any(|p| p == pubkey) {
                    return true;
                }
                task.abort();
                false
            });
            for (agent, keys) in agents {
                if running.contains_key(&agent.pubkey) {
                    continue;
                }
                let watcher = tokio::spawn(watch(relay.clone(), agent.clone(), keys, tx.clone()));
                running.insert(agent.pubkey.clone(), watcher);
            }
            tokio::time::sleep(SCAN).await;
        }
    });
}

/// One agent, for as long as buzzr runs: ask the relay for the mentions naming it, and answer them one at a time, in
/// the order they arrived.
async fn watch(relay: Arc<Relay>, agent: agents::Agent, keys: Keys, tx: UnboundedSender<AppEvent>) {
    let own = match own_relay(&relay, &agent, keys) {
        Ok(own) => own,
        Err(e) => {
            report(&tx, &agent.name, Err(e));
            return;
        }
    };
    let Some(harness) = harness::get(&agent.harness) else {
        report(&tx, &agent.name, Err(format!("{} is not a harness buzzr knows", agent.harness)));
        return;
    };
    // Where this watch starts. Mentions older than this are left alone: they are the ones that arrived while nobody
    // was running the agent, and answering a backlog of them is not what a mention asks for.
    let mut cursor = now();
    // The ids already answered, with the time they carried. The relay's `since` includes its own second, so the
    // events at the cursor come back on the next ask and must not be answered twice.
    let mut answered: HashMap<String, u64> = HashMap::new();
    // The agent's session, kept between mentions so it stays in the conversation. `None` until the first mention,
    // and again after a harness that could not answer, so the next mention starts a fresh one.
    let mut session: Option<harness::acp::Session> = None;
    loop {
        // A relay that does not answer (offline, restarting) is not worth a status line: the next ask tries again.
        if let Ok(events) = own.query(&[mentions_filter(&agent.pubkey, cursor)]).await {
            let mut fresh: Vec<Ev> = events.into_iter().filter(|e| answerable(e, &agent.pubkey) && !answered.contains_key(&e.id)).collect();
            // Oldest first: a conversation is answered in the order it happened, whatever order the relay answered in.
            fresh.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
            for mention in fresh {
                answered.insert(mention.id.clone(), mention.created_at);
                cursor = cursor.max(mention.created_at);
                let result = answer(&own, harness, &agent, &mut session, &mention).await;
                report(&tx, &agent.name, result);
            }
            // An event at the cursor's own second can still come back, so only the older ones are forgotten.
            answered.retain(|_, at| *at >= cursor);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Run one turn for one mention and post the answer as the agent: the harness in the agent's own folder, the session
/// kept between mentions, and the channel the mention was in.
async fn answer(relay: &Relay, harness: &'static Harness, agent: &agents::Agent, session: &mut Option<harness::acp::Session>, mention: &Ev) -> Result<(), String> {
    let Some(channel) = mention.channel().map(str::to_owned) else {
        return Err("a mention that names no channel cannot be answered".to_owned());
    };
    let prompt = mention_prompt(mention);
    // The harness is started and driven here, on the blocking pool: both block on the process, and the loop must not
    // (principle 6 in `AGENTS.md`). The session is moved across and comes back; a session that could not answer an
    // error is dropped rather than kept, so the next mention opens a fresh harness.
    let live = session.take();
    let (cwd, model, effort, instructions) =
        (PathBuf::from(&agent.workspace), agent.model.clone(), agent.effort.clone(), agent.instructions.clone());
    let (back, result) = tokio::task::spawn_blocking(move || {
        let mut session = match live {
            Some(session) => session,
            None => match harness::session(harness, &cwd, model.as_deref(), effort.as_deref(), &instructions) {
                Ok(session) => session,
                Err(e) => return (None, Err(e)),
            },
        };
        let result = session.prompt(&prompt);
        (Some(session), result)
    })
    .await
    .map_err(|e| format!("the turn did not run: {e}"))?;
    *session = if result.is_ok() { back } else { None };
    let text = result?;
    let text = text.trim();
    if text.is_empty() {
        return Err("the harness answered nothing".to_owned());
    }
    // The answer is the agent's own message: the channel the mention was in, in the mention's thread when the mention
    // was a reply, naming the person who asked so they are told about it.
    let builder = relay::message_builder(&channel, text, mention.root_hint(), std::slice::from_ref(&mention.pubkey))
        .map_err(|e| format!("{e:#}"))?;
    relay.submit(builder).await.map(|_| ()).map_err(|e| format!("the relay refused the answer: {e:#}"))
}

/// The agent's own relay: the person's relay, the agent's key, and the owner's NIP-OA attestation for this agent as
/// the `x-auth-tag` its events carry (and on them) — the same attestation the agent's profile goes out with, which is
/// what tells the relay the agent is acting for its owner.
fn own_relay(relay: &Relay, agent: &agents::Agent, keys: Keys) -> Result<Relay, String> {
    let attestation = agents::attestation(relay.keys(), &agent.pubkey)?;
    Relay::new(relay.base_url(), keys, Some(attestation)).map_err(|e| format!("{e:#}"))
}

/// The relay query that finds the messages naming `pubkey`: the message kinds, carrying the agent in a `p` tag, from
/// `since` on, newest first as the relay answers.
fn mentions_filter(pubkey: &str, since: u64) -> Value {
    json!({ "kinds": MESSAGE_KINDS, "#p": [pubkey], "since": since, "limit": LIMIT })
}

/// Whether a fetched event is a mention this agent answers: a message that names the agent, in a channel, that the
/// agent did not write itself.
fn answerable(e: &Ev, pubkey: &str) -> bool {
    MESSAGE_KINDS.contains(&e.kind)
        && e.pubkey != pubkey
        && e.channel().is_some()
        && e.tags.iter().any(|t| t.first().map(String::as_str) == Some("p") && t.get(1).map(String::as_str) == Some(pubkey))
}

/// The prompt one mention turns into: who is asking, and what they said. A turn here has no tool to publish with — a
/// harness speaks plain ACP, with no tool server — so the harness is told that the text it streams back is what goes
/// to the channel, which is the one thing it cannot read anywhere else.
fn mention_prompt(e: &Ev) -> String {
    let who = relay::npub_of(&e.pubkey).unwrap_or_else(|| e.pubkey.clone());
    format!(
        "You were mentioned in a Buzz channel. Answer in your reply: the text you stream back is posted to the channel as your own message.\n\n{who} wrote:\n{}",
        e.content.trim()
    )
}

/// Tell the app what a turn came to: the status line says the agent answered, or why it could not.
fn report(tx: &UnboundedSender<AppEvent>, agent: &str, result: Result<(), String>) {
    let _ = tx.send(AppEvent::AgentTurn { agent: agent.to_owned(), result });
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::ev;

    fn message(id: &str, at: u64, who: &str, content: &str, tags: &[&[&str]]) -> Ev {
        let mut e = ev(id, at, content, tags);
        e.pubkey = who.to_owned();
        e
    }

    #[test]
    fn only_a_message_that_names_the_agent_and_is_not_the_agents_own_is_answered() {
        let me = &"aa".repeat(32);
        let other = &"bb".repeat(32);
        let naming = |who: &str| message("m1", 1000, who, "@Scout what is new?", &[&["h", "c1"], &["p", me]]);
        assert!(answerable(&naming(other), me), "a message naming the agent is a mention");
        assert!(!answerable(&naming(me), me), "the agent's own message is never answered, so an answer cannot wake itself");
        let no_tag = message("m2", 1000, other, "hello", &[&["h", "c1"]]);
        assert!(!answerable(&no_tag, me), "a message that does not name the agent is not a mention");
        let no_channel = message("m3", 1000, other, "hi", &[&["p", me]]);
        assert!(!answerable(&no_channel, me), "a mention with no channel has nowhere to answer");
        let mut reaction = message("m4", 1000, other, "", &[&["h", "c1"], &["p", me]]);
        reaction.kind = 7;
        assert!(!answerable(&reaction, me), "only a message is answered, not a reaction that happens to name it");
    }

    #[test]
    fn the_query_asks_for_the_message_kinds_naming_the_agent_from_the_cursor() {
        let pubkey = "aa".repeat(32);
        let filter = mentions_filter(&pubkey, 1700);
        assert_eq!(filter["#p"], json!([pubkey]), "the mention is the `p` tag");
        assert_eq!(filter["since"], 1700);
        assert_eq!(filter["limit"], LIMIT);
        let kinds: Vec<u64> = serde_json::from_value(filter["kinds"].clone()).unwrap();
        assert_eq!(kinds, MESSAGE_KINDS.to_vec(), "the kinds the client reads as messages");
    }

    #[test]
    fn a_turn_is_told_who_asked_and_that_its_answer_is_the_reply() {
        let who = Keys::generate();
        let pubkey = who.public_key().to_hex();
        let npub = relay::npub_of(&pubkey).unwrap();
        let prompt = mention_prompt(&message("m1", 1000, &pubkey, "  @Scout what is new?  ", &[&["h", "c1"], &["p", &pubkey]]));
        assert!(prompt.contains(&npub), "the person who asked is named: {prompt:?}");
        assert!(prompt.contains("@Scout what is new?"), "and so is what they said, trimmed: {prompt:?}");
        assert!(prompt.contains("posted to the channel as your own message"), "the turn says what happens to its answer: {prompt:?}");
        assert!(!prompt.contains("\"  @Scout"), "the message is trimmed: {prompt:?}");
    }
}
