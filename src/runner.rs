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
//! **A thread mention comes with its thread.** A mention that is a reply is answered with the thread's earlier messages
//! in the prompt — the root and the replies before it, oldest first, the newest twelve — so the agent reads the
//! conversation it is joining instead of one message out of it. The Buzz app hands its agents the same window (its
//! harness prompts keep the newest twelve context messages); a harness here speaks plain ACP with no tool to read the
//! channel with, so the window travels as text in the prompt.
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
/// The most earlier messages of a thread shown with a mention, newest kept: the same window the Buzz app hands its agents.
const THREAD_CONTEXT_LIMIT: usize = 12;
/// The most characters of each earlier message shown.
const CONTEXT_CONTENT_LIMIT: usize = 4000;

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
    let mut cursor = crate::clock::now_secs();
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
    let prompt = mention_prompt(mention, &thread_context(relay, &channel, mention).await);
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
    let attestation = agents::attestation(relay.keys(), &agent.pubkey);
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
        && e.has_tag("p", pubkey)
}

/// The earlier messages of a mention's thread, oldest first, as they go into the prompt: empty for a mention that is not
/// a reply, and for a thread the relay will not answer (the mention is still answered, just without the context).
async fn thread_context(relay: &Relay, channel: &str, mention: &Ev) -> Vec<Ev> {
    let Some(root) = mention.root_hint() else {
        return Vec::new();
    };
    match relay.thread(channel, root, relay::THREAD_LIMIT).await {
        Ok(events) => earlier_in_thread(events, channel, mention),
        Err(_) => Vec::new(),
    }
}

/// The thread's earlier messages from the events the relay answered: the root and the replies that came before the
/// mention, in the channel, oldest first, the newest [`THREAD_CONTEXT_LIMIT`] kept. `relay::thread` also returns the
/// reactions, edits and deletions of the thread; those are not messages and are left out.
fn earlier_in_thread(events: Vec<Ev>, channel: &str, mention: &Ev) -> Vec<Ev> {
    let mut earlier: Vec<Ev> = events
        .into_iter()
        .filter(|e| {
            e.id != mention.id
                && MESSAGE_KINDS.contains(&e.kind)
                && e.channel() == Some(channel)
                && e.created_at <= mention.created_at
        })
        .collect();
    earlier.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    let skip = earlier.len().saturating_sub(THREAD_CONTEXT_LIMIT);
    earlier.split_off(skip)
}

/// The prompt one mention turns into: the earlier messages of its thread when it is a reply, then who is asking and what
/// they said. A turn here has no tool to publish with — a harness speaks plain ACP, with no tool server — so the harness
/// is told that the text it streams back is what goes to the channel, which is the one thing it cannot read anywhere
/// else.
fn mention_prompt(e: &Ev, earlier: &[Ev]) -> String {
    let who = relay::npub_of(&e.pubkey).unwrap_or_else(|| e.pubkey.clone());
    let scope = if e.root_hint().is_some() { "thread" } else { "channel" };
    let mut prompt = format!(
        "You were mentioned in a Buzz {scope}. Answer in your reply: the text you stream back is posted to the channel as your own message."
    );
    if !earlier.is_empty() {
        prompt.push_str("\n\nEarlier messages in this thread, oldest first, so you can read the conversation you are joining:");
        for message in earlier {
            let author = relay::npub_of(&message.pubkey).unwrap_or_else(|| message.pubkey.clone());
            prompt.push_str(&format!("\n\n{author} wrote:\n{}", clip(message.content.trim())));
        }
    }
    prompt.push_str(&format!("\n\n{who} wrote:\n{}", e.content.trim()));
    prompt
}

/// `text` cut to [`CONTEXT_CONTENT_LIMIT`] characters, at a character boundary: one long message in a thread must not
/// crowd out the rest of the window.
fn clip(text: &str) -> String {
    if text.chars().count() <= CONTEXT_CONTENT_LIMIT {
        return text.to_owned();
    }
    let cut: String = text.chars().take(CONTEXT_CONTENT_LIMIT).collect();
    format!("{cut}…")
}

/// Tell the app what a turn came to: the status line says the agent answered, or why it could not.
fn report(tx: &UnboundedSender<AppEvent>, agent: &str, result: Result<(), String>) {
    let _ = tx.send(AppEvent::AgentTurn { agent: agent.to_owned(), result });
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
        let prompt = mention_prompt(&message("m1", 1000, &pubkey, "  @Scout what is new?  ", &[&["h", "c1"], &["p", &pubkey]]), &[]);
        assert!(prompt.contains(&npub), "the person who asked is named: {prompt:?}");
        assert!(prompt.contains("@Scout what is new?"), "and so is what they said, trimmed: {prompt:?}");
        assert!(prompt.contains("posted to the channel as your own message"), "the turn says what happens to its answer: {prompt:?}");
        assert!(!prompt.contains("\"  @Scout"), "the message is trimmed: {prompt:?}");
        assert!(prompt.contains("Buzz channel"), "a top-level mention is a channel mention: {prompt:?}");
        assert!(!prompt.contains("Earlier messages in this thread"), "and has no thread to be shown: {prompt:?}");
    }

    #[test]
    fn a_thread_mention_is_answered_with_the_threads_earlier_messages_oldest_first() {
        let me = &"aa".repeat(32);
        let other = &"bb".repeat(32);
        let root = "cc".repeat(32);
        let root_msg = message("r", 1000, other, "we are planning the release", &[&["h", "c1"]]);
        let reply = message("p", 1500, other, "the date is settled", &[&["h", "c1"], &["e", &root, "", "reply"]]);
        let mention = message("m", 2000, other, "@Scout what did we decide?", &[&["h", "c1"], &["e", &root, "", "reply"], &["p", me]]);
        let earlier = earlier_in_thread(vec![root_msg, reply, mention.clone()], "c1", &mention);
        let ids: Vec<&str> = earlier.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["r", "p"], "the thread's earlier messages, oldest first, the mention left out");
        let prompt = mention_prompt(&mention, &earlier);
        assert!(prompt.contains("Buzz thread"), "a reply is answered as a thread mention: {prompt:?}");
        assert!(prompt.contains("Earlier messages in this thread"), "the context is framed: {prompt:?}");
        let (first, second) = (prompt.find("we are planning the release"), prompt.find("the date is settled"));
        assert!(first.is_some() && second.is_some(), "both earlier messages are shown: {prompt:?}");
        assert!(first < second, "oldest first, the way the conversation happened: {prompt:?}");
        assert!(prompt.contains("@Scout what did we decide?"), "the mention itself is still the ask: {prompt:?}");
    }

    #[test]
    fn a_thread_context_is_only_the_messages_of_that_thread_before_the_mention() {
        let me = &"aa".repeat(32);
        let other = &"bb".repeat(32);
        let root = "cc".repeat(32);
        let mention = message("m", 2000, other, "@Scout?", &[&["h", "c1"], &["e", &root, "", "reply"], &["p", me]]);
        let in_thread = message("a", 1000, other, "in the thread", &[&["h", "c1"], &["e", &root, "", "reply"]]);
        let later = message("b", 3000, other, "after the mention", &[&["h", "c1"], &["e", &root, "", "reply"]]);
        let elsewhere = message("c", 1100, other, "another channel", &[&["h", "c2"], &["e", &root, "", "reply"]]);
        let mut reaction = message("d", 1200, other, "👍", &[&["h", "c1"], &["e", &root, "", "reply"]]);
        reaction.kind = 7;
        let mut edit = message("e", 1300, other, "fixed", &[&["h", "c1"], &["e", &root, "", "reply"]]);
        edit.kind = 40003;
        let mut deletion = message("f", 1400, other, "", &[&["h", "c1"], &["e", &root, "", "reply"]]);
        deletion.kind = 5;
        let earlier = earlier_in_thread(vec![in_thread, later, elsewhere, reaction, edit, deletion, mention.clone()], "c1", &mention);
        let ids: Vec<&str> = earlier.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["a"], "only the messages of this thread that came before the mention: {ids:?}");
    }

    #[test]
    fn only_the_newest_earlier_messages_are_kept() {
        let other = &"bb".repeat(32);
        let root = "cc".repeat(32);
        let mut events: Vec<Ev> = (1..=15)
            .map(|n| message(&format!("m{n}"), 1000 + n, other, "something", &[&["h", "c1"], &["e", &root, "", "reply"]]))
            .collect();
        let mention = message("ask", 9000, other, "@Scout?", &[&["h", "c1"], &["e", &root, "", "reply"]]);
        events.push(mention.clone());
        let earlier = earlier_in_thread(events, "c1", &mention);
        let ids: Vec<&str> = earlier.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids.len(), THREAD_CONTEXT_LIMIT, "the window is bounded: {ids:?}");
        assert_eq!(ids.first(), Some(&"m4"), "the oldest ones past the window are dropped: {ids:?}");
        assert_eq!(ids.last(), Some(&"m15"), "the newest one is kept: {ids:?}");
    }

    #[test]
    fn one_long_earlier_message_is_cut_to_the_context_limit() {
        let long = "x".repeat(CONTEXT_CONTENT_LIMIT + 500);
        let cut = clip(&long);
        assert_eq!(cut.chars().count(), CONTEXT_CONTENT_LIMIT + 1, "the message is cut to the limit, with the marker");
        assert!(cut.ends_with('…'), "and says it was cut: {cut:?}");
        assert_eq!(clip("short"), "short", "a message under the limit is untouched");
    }
}
