//! The `--demo` world: a small community that is always there, and the answers a relay would give, so the client can
//! be tried without a connection.
//!
//! A demo run never builds a [`crate::relay::Relay`]: the loop hands every command to [`Demo::answer`], which turns it
//! into the same `AppEvent`s a relay's answers arrive as, and what the reader writes comes back as live events.
//! Nothing here touches the network or the disk, so a demo leaves the machine exactly as it found it.

use crate::app::{AppEvent, Cmd};
use crate::clock::now_secs;
use crate::config::ReadState;
use crate::live::Live;
use crate::model::{Channel, Ev, Section, Store, KIND_DELETION, KIND_DELETE_MESSAGE, KIND_EDIT, KIND_REACTION, MESSAGE_KINDS};
use crate::relay::{Profile, Window};
use std::collections::HashMap;
use tokio::sync::mpsc::UnboundedSender;

/// The line a demo greets the reader with: what it is, and the two things it never does.
pub const HINT: &str = "demo: a built-in world — nothing is sent, nothing is saved";

/// What a demo says when a command needs a relay. The agents are the only feature left that has nowhere else to go.
const NO_RELAY: &str = "the demo has no relay, so this goes nowhere";

/// The seed of the demo identity's key (see [`key`]).
const ME: u8 = 1;

/// The name the demo identity shows up under: the reader's own, so the messages they can edit and delete are the ones
/// they wrote.
const ME_NAME: &str = "you";

/// A person of the demo cast: the key the app sees, the name it shows, and the line on their profile card.
struct Person {
    key: String,
    name: &'static str,
    about: &'static str,
}

/// The public key of a fixed secret, so the cast is the same on every run. An npub, a profile card and a name all keep
/// working, and a test does not depend on a key that changes underneath it.
fn key(seed: u8) -> String {
    nostr::Keys::parse(&format!("{seed:02x}").repeat(32)).expect("a fixed demo key").public_key().to_hex()
}

/// One channel of the world.
fn chan(id: &str, name: &str, about: &str, section: Section, private: bool) -> Channel {
    Channel { id: id.into(), name: name.into(), about: about.into(), created_at: 0, section, label: name.into(), is_private: private }
}

/// Writes the world's events, with one counter for their ids, so the demo is the same on every run.
struct Pen<'a> {
    events: &'a mut HashMap<String, Vec<Ev>>,
    seq: u64,
}

impl Pen<'_> {
    fn id(&mut self) -> String {
        self.seq += 1;
        format!("demo{:04}", self.seq)
    }

    fn put(&mut self, channel: &str, e: Ev) -> String {
        let id = e.id.clone();
        self.events.entry(channel.to_owned()).or_default().push(e);
        id
    }

    /// A message on a channel's timeline.
    fn say(&mut self, channel: &str, from: &str, at: u64, text: &str) -> String {
        let id = self.id();
        let tags = vec![vec!["h".to_owned(), channel.to_owned()]];
        self.put(channel, Ev { id, pubkey: from.into(), created_at: at, kind: 9, content: text.into(), tags })
    }

    /// A reply. `root` is the message the thread starts from, which a direct reply also names as its parent.
    fn reply(&mut self, channel: &str, from: &str, at: u64, text: &str, root: &str) -> String {
        let id = self.id();
        let link = |marker: &str| vec!["e".to_owned(), root.to_owned(), String::new(), marker.to_owned()];
        let tags = vec![vec!["h".to_owned(), channel.to_owned()], link("root"), link("reply")];
        self.put(channel, Ev { id, pubkey: from.into(), created_at: at, kind: 9, content: text.into(), tags })
    }

    /// A reaction to `target`.
    fn react(&mut self, channel: &str, from: &str, at: u64, target: &str, emoji: &str) {
        let id = self.id();
        let tags = vec![vec!["h".to_owned(), channel.to_owned()], vec!["e".to_owned(), target.to_owned()]];
        self.put(channel, Ev { id, pubkey: from.into(), created_at: at, kind: KIND_REACTION, content: emoji.into(), tags });
    }
}

/// The world a `--demo` run lives in, and the relay it answers for.
pub struct Demo {
    me: String,
    people: Vec<Person>,
    channels: Vec<Channel>,
    /// Every event of a channel, oldest first: what the window, the thread and the search are answered from.
    events: HashMap<String, Vec<Ev>>,
    /// Who is in each channel, by public key (`kind:39002`).
    roster: HashMap<String, Vec<String>>,
    /// Where the reader left each channel last time, so the first screen has something unread to show.
    read: HashMap<String, u64>,
    /// A counter for the ids of what the reader writes, carrying on from the seeded events.
    seq: u64,
}

impl Demo {
    /// The world a demo run starts in: a few channels, a DM, and a cast the screen can put names to.
    pub fn new() -> Demo {
        let me = key(ME);
        let people: Vec<Person> = [
            (2u8, "Ada Lin", "terminal UI, and a picker for reactions"),
            (3, "Mara Lindqvist", "design — the small details are the work"),
            (4, "Tomas Reyes", "releases, packaging, and the tag that ships"),
            (5, "Priya Nair", "reads the fine print so the rest of us do not have to"),
            (6, "Jonas Weber", "mostly lurking"),
            (7, "Sofia Marchetti", "docs and screenshots"),
        ]
        .into_iter()
        .map(|(seed, name, about)| Person { key: key(seed), name, about })
        .collect();
        let who = |name: &str| people.iter().find(|p| p.name == name).expect("a person of the demo cast").key.clone();
        let (ada, mara, tomas, priya) = (who("Ada Lin"), who("Mara Lindqvist"), who("Tomas Reyes"), who("Priya Nair"));

        let channels = vec![
            chan("buzzr", "buzzr", "the terminal client, and what it should do", Section::Channels, false),
            chan("design", "design", "how it looks and feels", Section::Channels, false),
            chan("random", "random", "anything at all", Section::Channels, false),
            chan("release", "release", "shipping", Section::Channels, true),
            chan("dm-ada", "DM", "", Section::Dms, true),
        ];
        let mut roster: HashMap<String, Vec<String>> = HashMap::new();
        roster.insert("buzzr".into(), vec![me.clone(), ada.clone(), mara.clone(), tomas.clone(), priya.clone()]);
        roster.insert("design".into(), vec![me.clone(), ada.clone(), mara.clone()]);
        // An open channel we are not in yet: the composer is the Join button until it is joined.
        roster.insert("random".into(), vec![ada.clone(), tomas.clone(), priya.clone()]);
        roster.insert("release".into(), vec![me.clone(), tomas.clone(), ada.clone()]);
        roster.insert("dm-ada".into(), vec![me.clone(), ada.clone()]);

        let now = now_secs();
        let ago = |mins: u64| now.saturating_sub(mins * 60);
        let mut events: HashMap<String, Vec<Ev>> = HashMap::new();
        let seq = {
            let mut pen = Pen { events: &mut events, seq: 0 };
            let footer = pen.say("buzzr", &ada, ago(190), "the footer clips from the right, so the always-on keys survive a narrow terminal");
            pen.reply("buzzr", &mara, ago(186), "and the per-focus hints go first — that reads better than I expected", &footer);
            pen.reply("buzzr", &ada, ago(184), "that was the idea: the group is the last thing to go", &footer);
            let marks = pen.say("buzzr", &tomas, ago(150), "should a thread mark unread per thread, or per channel?");
            pen.react("buzzr", &mara, ago(148), &marks, "👍");
            pen.react("buzzr", &ada, ago(147), &marks, "🎉");
            pen.say("buzzr", &priya, ago(120), "per thread — a reply is not a timeline row, so a channel badge would never clear");
            pen.say("buzzr", &me, ago(60), "agreed, I'll write it up");

            pen.say("design", &mara, ago(300), "the new-messages line should sit above the first unread message, not above the last read one");
            pen.say("design", &me, ago(290), "that is what it does now: the position is taken when the channel is opened");
            let mock = pen.say("design", &ada, ago(240), "mock for the reaction picker: type to search, Enter to react");
            pen.reply("design", &mara, ago(200), "it should open on the message you have selected, not the newest one", &mock);
            let chars = pen.say("design", &tomas, ago(30), "one character per channel: # open, * private, @ dm");
            pen.react("design", &priya, ago(28), &chars, "❤️");

            pen.say("release", &tomas, ago(1000), "tagging is what publishes a release, not merging the version bump");
            pen.say("release", &ada, ago(980), "right — the workflow watches the tag");
            pen.say("release", &me, ago(900), "noted in the release notes");

            pen.say("random", &tomas, ago(800), "coffee or tea?");
            pen.say("random", &ada, ago(790), "tea, obviously");

            pen.say("dm-ada", &ada, ago(45), "want me to take the picker?");
            pen.say("dm-ada", &me, ago(40), "yes please — I'm on the thread marks");
            pen.say("dm-ada", &ada, ago(35), "👍");
            pen.seq
        };
        // Two channels were left part-read, so the first screen has a badge on them and the "new messages" line has
        // something to mark. The channel the demo opens on (the first it lists) has no position and starts clean.
        let read = HashMap::from([("design".to_owned(), ago(205)), ("random".to_owned(), ago(795))]);
        Demo { me, people, channels, events, roster, read, seq }
    }

    /// The identity a demo run wears.
    pub fn me(&self) -> &str {
        &self.me
    }

    /// Where the reader "left" each channel last time: the demo opens with a badge or two, the way a real session does.
    pub fn read_state(&self) -> ReadState {
        ReadState { channels: self.read.clone(), threads: HashMap::new(), marks: HashMap::new() }
    }

    /// Answer one command the way a relay would: the same events come back, so the screen cannot tell the difference.
    pub fn answer(&mut self, cmd: Cmd, tx: &UnboundedSender<AppEvent>) {
        match cmd {
            Cmd::LoadChannels => send(tx, AppEvent::Channels(Ok(self.channels.clone()))),
            Cmd::LoadRoster { channels } => {
                let roster = channels.iter().map(|id| (id.clone(), self.roster.get(id).cloned().unwrap_or_default())).collect();
                send(tx, AppEvent::Roster(Ok(roster)));
            }
            Cmd::LoadMembers { channel } => {
                let members = self.roster.get(&channel).cloned().unwrap_or_default().into_iter().map(|key| (key, "member".to_owned())).collect();
                send(tx, AppEvent::Members { channel, result: Ok(members) });
            }
            Cmd::LoadNames(pubkeys) => {
                let names = pubkeys.iter().filter_map(|key| self.name_of(key).map(|n| (key.clone(), n.to_owned()))).collect();
                send(tx, AppEvent::Names(Ok(names)));
            }
            Cmd::LoadWindow { channel, cursor } => {
                // One page is the whole channel: the demo has nothing older behind it.
                let events = self.events.get(&channel).cloned().unwrap_or_default();
                send(tx, AppEvent::Window { channel, cursor, result: Ok(Window { events, has_more: false, next_cursor: None }) });
            }
            Cmd::LoadThread { channel, root } => {
                let events = self.thread(&channel, &root);
                send(tx, AppEvent::Thread { channel, root, result: Ok(events) });
            }
            Cmd::LoadUnread { channel, since } => {
                let count = self.unread_in(&channel, since);
                send(tx, AppEvent::Unread { channel, count });
            }
            Cmd::LoadProfile(pubkey) => {
                let profile = self.person(&pubkey).map(|p| Profile { name: p.name.to_owned(), about: p.about.to_owned() }).unwrap_or_default();
                send(tx, AppEvent::Profile { pubkey, result: Ok(profile) });
            }
            Cmd::FindPeople { query } => {
                let hits = self.people_matching(&query);
                send(tx, AppEvent::People { query, result: Ok(hits) });
            }
            Cmd::Search { query, seq } => {
                let hits = self.search(&query);
                send(tx, AppEvent::Search { seq, result: Ok(hits) });
            }
            Cmd::Send { channel, text, root, mentions } => {
                let _ = mentions; // the demo has nobody to notify: a mention is read on screen, not routed
                let me = self.me.clone();
                let mut tags = vec![vec!["h".to_owned(), channel.clone()]];
                if let Some(root) = &root {
                    for marker in ["root", "reply"] {
                        tags.push(vec!["e".to_owned(), root.clone(), String::new(), marker.to_owned()]);
                    }
                }
                let id = self.emit(&channel, 9, &me, &text, tags, tx);
                send(tx, AppEvent::Sent(Ok(id)));
            }
            Cmd::Edit { channel, id, text } => {
                let me = self.me.clone();
                let tags = vec![vec!["h".to_owned(), channel.clone()], vec!["e".to_owned(), id]];
                let sent = self.emit(&channel, KIND_EDIT, &me, &text, tags, tx);
                send(tx, AppEvent::Sent(Ok(sent)));
            }
            Cmd::Delete { channel, id } => {
                let me = self.me.clone();
                let tags = vec![vec!["h".to_owned(), channel.clone()], vec!["e".to_owned(), id]];
                let sent = self.emit(&channel, KIND_DELETE_MESSAGE, &me, "", tags, tx);
                send(tx, AppEvent::Sent(Ok(sent)));
            }
            Cmd::React { target, emoji, remove } => {
                let channel = self.channel_of(&target).unwrap_or_default();
                let me = self.me.clone();
                let mut sent = String::new();
                // Undo the reaction this one replaces first, as the app asks: a change never leaves two emoji from one person.
                if let Some(reaction) = remove {
                    let tags = vec![vec!["h".to_owned(), channel.clone()], vec!["e".to_owned(), reaction]];
                    sent = self.emit(&channel, KIND_DELETION, &me, "", tags, tx);
                }
                if !emoji.is_empty() {
                    let tags = vec![vec!["h".to_owned(), channel.clone()], vec!["e".to_owned(), target]];
                    sent = self.emit(&channel, KIND_REACTION, &me, &emoji, tags, tx);
                }
                send(tx, AppEvent::Sent(Ok(sent)));
            }
            Cmd::CreateChannel { name, private, about } => {
                let id = self.new_channel(&name, private, about.as_deref().unwrap_or(""));
                send(tx, AppEvent::Created(Ok(id)));
            }
            Cmd::JoinChannel { channel } => {
                let me = self.me.clone();
                let members = self.roster.entry(channel.clone()).or_default();
                if !members.contains(&me) {
                    members.push(me);
                }
                send(tx, AppEvent::Joined { channel, result: Ok(String::new()) });
            }
            Cmd::LeaveChannel { channel } => {
                let me = self.me.clone();
                if let Some(members) = self.roster.get_mut(&channel) {
                    members.retain(|key| *key != me);
                }
                send(tx, AppEvent::Left { channel, result: Ok(String::new()) });
            }
            Cmd::OpenDm { peer } => {
                self.new_dm(&peer);
                send(tx, AppEvent::DmOpened(Ok(())));
            }
            Cmd::Invite { channel, pubkey, dm } => {
                let _ = dm; // the demo has one kind of membership: the new member simply joins the channel's list
                let members = self.roster.entry(channel.clone()).or_default();
                if !members.contains(&pubkey) {
                    members.push(pubkey.clone());
                }
                send(tx, AppEvent::Invited { channel, pubkey, result: Ok(()) });
            }
            // What only a relay can do. A demo has none, so these say so rather than pretending.
            Cmd::CreateAgent { .. } => send(tx, AppEvent::AgentCreated(Err(NO_RELAY.to_owned()))),
            Cmd::DeleteAgent { .. } => send(tx, AppEvent::AgentDeleted(Err(NO_RELAY.to_owned()))),
            Cmd::UpdateAgent { .. } => send(tx, AppEvent::AgentUpdated(Err(NO_RELAY.to_owned()))),
            Cmd::ProbeHarness { .. } => send(tx, AppEvent::HarnessOffer(Err(NO_RELAY.to_owned()))),
            // Nothing of the demo's is worth saving and nothing it is told is worth telling on: the subscription, the
            // typing indicator, the files the loop would write, the plugin runtime and opening a link all do nothing.
            Cmd::Subscribe(_) | Cmd::Typing { .. } | Cmd::SaveRead(_) | Cmd::SaveArchived(_) | Cmd::SaveDrafts(_) | Cmd::Plugin(_) | Cmd::OpenUrl(_) => {}
        }
    }

    /// Add an event to the world and show it at once: the screen sees what it would see if a relay had sent it back.
    fn emit(&mut self, channel: &str, kind: u64, from: &str, content: &str, tags: Vec<Vec<String>>, tx: &UnboundedSender<AppEvent>) -> String {
        self.seq += 1;
        let id = format!("demo{:04}", self.seq);
        let e = Ev { id: id.clone(), pubkey: from.into(), created_at: now_secs(), kind, content: content.into(), tags };
        self.events.entry(channel.to_owned()).or_default().push(e.clone());
        send(tx, AppEvent::Live(Live::Event(e)));
        id
    }

    /// A new channel of the world, with the demo identity in it, and the id it was given.
    fn new_channel(&mut self, name: &str, private: bool, about: &str) -> String {
        self.seq += 1;
        let id = format!("demo-chan-{}", self.seq);
        let section = Section::Channels;
        self.channels.push(Channel { id: id.clone(), name: name.to_owned(), about: about.to_owned(), created_at: now_secs(), section, label: name.to_owned(), is_private: private });
        self.roster.insert(id.clone(), vec![self.me.clone()]);
        self.events.insert(id.clone(), Vec::new());
        id
    }

    /// The DM with `peer`, made if it is not there yet: a channel between the two of you, the way a relay opens one.
    fn new_dm(&mut self, peer: &str) {
        if self.dm_with(peer).is_some() {
            return;
        }
        self.seq += 1;
        let id = format!("demo-dm-{}", self.seq);
        let about = String::new();
        self.channels.push(Channel { id: id.clone(), name: "DM".into(), about, created_at: now_secs(), section: Section::Dms, label: "DM".into(), is_private: true });
        self.roster.insert(id.clone(), vec![self.me.clone(), peer.to_owned()]);
        self.events.insert(id, Vec::new());
    }

    /// The DM that has `peer` as its only other member, if the demo has one.
    fn dm_with(&self, peer: &str) -> Option<&str> {
        self.channels.iter().filter(|c| c.is_dm()).find_map(|c| {
            let others: Vec<&str> = self.roster.get(&c.id)?.iter().map(String::as_str).filter(|k| *k != self.me.as_str()).collect();
            (others == [peer]).then_some(c.id.as_str())
        })
    }

    /// The channel an event id lives in: a reaction removal names the reaction, which is in the same channel as what it
    /// was put on.
    fn channel_of(&self, id: &str) -> Option<String> {
        self.events.iter().find(|(_, events)| events.iter().any(|e| e.id == id)).map(|(channel, _)| channel.clone())
    }

    /// The root of a thread and every message under it, as the thread view would load them.
    fn thread(&self, channel: &str, root: &str) -> Vec<Ev> {
        let mut store = Store::default();
        for e in self.events.get(channel).map(Vec::as_slice).unwrap_or(&[]) {
            store.apply(e);
        }
        store.thread(root).into_iter().cloned().collect()
    }

    /// How many messages a channel has that the reader has not seen: the badge a stored position asks for.
    fn unread_in(&self, channel: &str, since: u64) -> u32 {
        self.events
            .get(channel)
            .map(|events| {
                events
                    .iter()
                    .filter(|e| MESSAGE_KINDS.contains(&e.kind) && e.pubkey != self.me && e.created_at >= since && e.is_top_level())
                    .count() as u32
            })
            .unwrap_or(0)
    }

    /// Messages whose text contains `q`, newest first — the order the relay's full-text search comes back in.
    fn search(&self, q: &str) -> Vec<Ev> {
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<Ev> = self.events.values().flatten().filter(|e| MESSAGE_KINDS.contains(&e.kind) && e.content.to_lowercase().contains(&q)).cloned().collect();
        hits.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
        hits.truncate(25);
        hits
    }

    /// The cast whose name contains `q`, exact names first.
    fn people_matching(&self, q: &str) -> Vec<(String, String)> {
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(String, String)> = self.people.iter().filter(|p| p.name.to_lowercase().contains(&q)).map(|p| (p.key.clone(), p.name.to_owned())).collect();
        hits.sort_by_key(|(_, name)| (!name.to_lowercase().starts_with(&q), name.to_lowercase()));
        hits
    }

    fn person(&self, key: &str) -> Option<&Person> {
        self.people.iter().find(|p| p.key == key)
    }

    /// The name a key shows up under, or `None` for a key the demo has never met.
    fn name_of(&self, key: &str) -> Option<&str> {
        if key == self.me {
            return Some(ME_NAME);
        }
        self.person(key).map(|p| p.name)
    }
}

fn send(tx: &UnboundedSender<AppEvent>, event: AppEvent) {
    let _ = tx.send(event); // a closed receiver means the app is gone: there is nobody left to answer
}

impl Default for Demo {
    fn default() -> Demo {
        Demo::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Run one command through the world, collecting the events it answers with the way the loop's receiver would.
    fn answers(world: &mut Demo, cmd: Cmd) -> Vec<AppEvent> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        world.answer(cmd, &tx);
        drop(tx);
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// The one event a command that answers once sends.
    fn only(events: Vec<AppEvent>) -> AppEvent {
        assert_eq!(events.len(), 1, "a command that answers once sends one event");
        events.into_iter().next().unwrap()
    }

    #[test]
    fn the_channels_and_the_roster_a_demo_serves() {
        let mut world = Demo::new();
        let AppEvent::Channels(Ok(channels)) = only(answers(&mut world, Cmd::LoadChannels)) else {
            panic!("a demo lists its channels");
        };
        let ids: Vec<&str> = channels.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["buzzr", "design", "random", "release", "dm-ada"]);
        assert!(channels.iter().any(|c| c.name == "release" && c.is_private), "one channel is private");

        let me = world.me().to_owned();
        let AppEvent::Roster(Ok(roster)) =
            only(answers(&mut world, Cmd::LoadRoster { channels: vec!["buzzr".into(), "random".into()] }))
        else {
            panic!("a demo answers the roster");
        };
        assert!(roster["buzzr"].contains(&me), "the demo identity is in a channel it joined");
        assert!(!roster["random"].contains(&me), "and not in one it has not");
    }

    #[test]
    fn a_demo_serves_a_window_and_a_thread_from_its_own_messages() {
        let mut world = Demo::new();
        let AppEvent::Window { result: Ok(window), .. } =
            only(answers(&mut world, Cmd::LoadWindow { channel: "buzzr".into(), cursor: None }))
        else {
            panic!("a demo serves a window");
        };
        assert!(window.events.iter().any(|e| e.content.contains("the footer clips")), "the seeded messages are there");
        assert!(!window.has_more, "the demo has nothing older behind the page");

        let root = window.events.iter().find(|e| e.content.contains("the footer clips")).unwrap().id.clone();
        let AppEvent::Thread { result: Ok(thread), .. } =
            only(answers(&mut world, Cmd::LoadThread { channel: "buzzr".into(), root }))
        else {
            panic!("a demo serves a thread");
        };
        assert!(thread.len() >= 3, "the root and its replies are the thread");
    }

    #[test]
    fn an_unknown_channel_is_answered_empty_rather_than_panicking() {
        let mut world = Demo::new();
        let AppEvent::Window { result: Ok(window), .. } =
            only(answers(&mut world, Cmd::LoadWindow { channel: "nope".into(), cursor: None }))
        else {
            panic!("a demo still answers");
        };
        assert!(window.events.is_empty());
    }

    #[test]
    fn search_finds_seeded_messages_newest_first() {
        let mut world = Demo::new();
        let AppEvent::Search { result: Ok(hits), .. } = only(answers(&mut world, Cmd::Search { query: "thread".into(), seq: 1 }))
        else {
            panic!("a demo searches its own world");
        };
        assert!(!hits.is_empty(), "the word is in the seeded messages");
        assert!(hits.windows(2).all(|w| w[0].created_at >= w[1].created_at), "hits come back newest first");
    }

    #[test]
    fn what_the_reader_writes_comes_back_as_a_live_event() {
        let mut world = Demo::new();
        let me = world.me().to_owned();
        let events = answers(
            &mut world,
            Cmd::Send { channel: "buzzr".into(), text: "hello from here".into(), root: None, mentions: Vec::new() },
        );
        // The message comes back the way a relay's echo arrives, and the send is acknowledged.
        assert!(
            events.iter().any(|e| matches!(e, AppEvent::Live(Live::Event(ev)) if ev.content == "hello from here" && ev.pubkey == me)),
            "the demo echoes what the reader writes"
        );
        assert!(events.iter().any(|e| matches!(e, AppEvent::Sent(Ok(_)))), "the send is acknowledged");
        // And it is in the world, so a window on that channel lists it.
        let AppEvent::Window { result: Ok(window), .. } =
            only(answers(&mut world, Cmd::LoadWindow { channel: "buzzr".into(), cursor: None }))
        else {
            panic!("a demo serves a window");
        };
        assert!(window.events.iter().any(|e| e.content == "hello from here"));
    }

    #[test]
    fn what_only_a_relay_can_do_says_the_demo_has_none() {
        let mut world = Demo::new();
        let cmd = Cmd::CreateAgent {
            name: "a".into(),
            instructions: "b".into(),
            harness: "claude-agent-acp".into(),
            model: String::new(),
            effort: String::new(),
        };
        let AppEvent::AgentCreated(Err(why)) = only(answers(&mut world, cmd)) else {
            panic!("a demo cannot make an agent");
        };
        assert!(why.contains("no relay"), "the answer says why: {why:?}");
    }
}
