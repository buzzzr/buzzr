//! Plain data: events as the relay sends them, and the per-channel store that turns them into what the screen shows.
//!
//! Nothing here touches the network or the terminal, so all of it is unit tested.

use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Event kinds shown as messages in a channel (the same set the `buzz` CLI reads).
pub const MESSAGE_KINDS: [u64; 5] = [9, 40002, 40008, 45001, 45003];
pub const KIND_PROFILE: u64 = 0;
pub const KIND_DELETION: u64 = 5;
pub const KIND_REACTION: u64 = 7;
pub const KIND_TYPING: u64 = 20002;
pub const KIND_GROUP_METADATA: u64 = 39000;
pub const KIND_GROUP_MEMBERS: u64 = 39002;
pub const KIND_THREAD_SUMMARY: u64 = 39005;
pub const KIND_WINDOW_BOUNDS: u64 = 39006;
pub const KIND_EDIT: u64 = 40003;
pub const KIND_DELETE_MESSAGE: u64 = 9005;

/// A Nostr event, reduced to what the client reads. Signatures are not checked here: the transport is authenticated (HTTPS with
/// NIP-98 or a NIP-42 WebSocket) and the relay is trusted for what it serves.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Ev {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u64,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tags: Vec<Vec<String>>,
}

impl Ev {
    pub fn from_value(v: &Value) -> Option<Ev> {
        serde_json::from_value(v.clone()).ok()
    }

    /// Every tag called `name`, whole (`["p", pubkey, relay, role]`). The one place that reads a tag's name: callers do not
    /// compare `t.first()` themselves.
    pub fn tags_named<'a, 'n>(&'a self, name: &'n str) -> impl Iterator<Item = &'a [String]> + use<'a, 'n> {
        self.tags.iter().filter(move |t| t.first().map(String::as_str) == Some(name)).map(Vec::as_slice)
    }

    /// First value of the first tag called `name`.
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags_named(name).next().and_then(|t| t.get(1)).map(String::as_str)
    }

    /// The first value of every tag called `name`, in order (the pubkeys of the `p` tags, the ids of the `e` tags).
    pub fn tag_values<'a, 'n>(&'a self, name: &'n str) -> impl Iterator<Item = &'a str> + use<'a, 'n> {
        self.tags_named(name).filter_map(|t| t.get(1)).map(String::as_str)
    }

    /// Whether a tag called `name` has exactly this first value.
    pub fn has_tag(&self, name: &str, value: &str) -> bool {
        self.tag_values(name).any(|v| v == value)
    }

    pub fn root_tag(&self) -> Option<&str> {
        self.e_marked("root")
    }

    pub fn reply_tag(&self) -> Option<&str> {
        self.e_marked("reply")
    }

    /// The `e` tag with a given marker (`root` or `reply`).
    fn e_marked(&self, marker: &str) -> Option<&str> {
        self.tags_named("e").find(|t| t.get(3).map(String::as_str) == Some(marker)).and_then(|t| t.get(1)).map(String::as_str)
    }

    /// The message this one replies to directly.
    pub fn parent(&self) -> Option<&str> {
        self.e_marked("reply").or_else(|| self.e_marked("root"))
    }

    /// The message that starts this one's thread, as far as its own tags say: a nested reply carries a `root` tag, a direct reply
    /// only a `reply` tag (its parent is the root).
    pub fn root_hint(&self) -> Option<&str> {
        self.e_marked("root").or_else(|| self.e_marked("reply"))
    }

    pub fn channel(&self) -> Option<&str> {
        self.tag("h")
    }

    /// A reply that its author also sent to the channel (`["broadcast","1"]`) is listed on the timeline.
    pub fn is_broadcast(&self) -> bool {
        self.has_tag("broadcast", "1")
    }

    /// Whether the channel timeline lists this message itself, rather than inside a thread.
    pub fn is_top_level(&self) -> bool {
        match (self.e_marked("root"), self.e_marked("reply")) {
            (None, None) => true,
            (None, Some(_)) => self.is_broadcast(), // depth 1 and broadcast
            (Some(_), _) => false,
        }
    }

}

/// What the relay says about one thread: `kind:39005`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub reply_count: u64,
    pub last_reply_at: Option<u64>,
    pub participants: Vec<String>,
}

impl Summary {
    pub fn from_event(e: &Ev) -> Option<(String, Summary)> {
        let root = e.tag("e").or_else(|| e.tag("d"))?.to_owned();
        let v: Value = serde_json::from_str(&e.content).ok()?;
        Some((
            root,
            Summary {
                reply_count: v.get("reply_count").and_then(Value::as_u64).unwrap_or(0),
                last_reply_at: v.get("last_reply_at").and_then(Value::as_u64),
                participants: v
                    .get("participants")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|p| p.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default(),
            },
        ))
    }
}

/// Which section of the channel list a channel belongs to; the variant order is the order on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    /// Solo chats: channels carrying the `buzz.solo` label.
    Solo,
    /// Open channels.
    Channels,
    /// Direct messages.
    Dms,
}

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Section::Solo => "Solo",
            Section::Channels => "Channels",
            Section::Dms => "DMs",
        }
    }
}

/// The NIP-CL label the Buzz apps put on a Solo chat.
const SOLO_LABEL: &str = "buzz.solo";

#[derive(Debug, Clone)]
pub struct Channel {
    pub id: String,
    pub name: String,
    /// What the group metadata says the channel is for (`about`); shown in a search result for the channel.
    pub about: String,
    #[allow(dead_code)]
    pub created_at: u64,
    pub section: Section,
    pub label: String,
    /// A private (restricted) channel: the relay's group metadata says `["private"]` instead of `["public"]`. Shown as `*` in
    /// the channel list, against `#` for an open one; a DM is shown as `@` (see `marker`).
    pub is_private: bool,
}

impl Channel {
    pub fn from_event(e: &Ev) -> Option<Channel> {
        let id = e.tag("d")?.to_owned();
        let name = e.tag("name").unwrap_or("").to_owned();
        // Direct messages all come with the generic name "DM"; they are labelled with the other participants instead.
        let section = if name == "DM" {
            Section::Dms
        } else if e.has_tag("t", SOLO_LABEL) {
            Section::Solo
        } else {
            Section::Channels
        };
        Some(Channel {
            section,
            label: name.clone(),
            about: e.tag("about").unwrap_or("").to_owned(),
            created_at: e.created_at,
            is_private: e.tags_named("private").next().is_some(),
            id,
            name,
        })
    }

    pub fn is_dm(&self) -> bool {
        self.section == Section::Dms
    }

    /// The one character that opens a channel's row: `@` for a direct message (it is a person, not a room), `*` for a
    /// private channel (RFC 2811's private-channel prefix) and `#` for an open one. All three are ASCII, so no terminal
    /// font can be missing them — the row is never a box or a blank.
    pub fn marker(&self) -> &'static str {
        if self.is_dm() {
            "@"
        } else if self.is_private {
            "*"
        } else {
            "#"
        }
    }
}

/// Everything loaded for one channel: its messages (top-level and replies together), thread summaries, edits, deletions,
/// reactions. Views (the channel timeline, one thread) are filters over this.
#[derive(Debug, Default)]
pub struct Store {
    pub msgs: HashMap<String, Ev>,
    /// Latest edit per target message: `(created_at, new content)`.
    pub edits: HashMap<String, (u64, String)>,
    pub deleted: HashSet<String>,
    pub summaries: HashMap<String, Summary>,
    /// target message -> emoji -> reactor pubkeys
    pub reactions: HashMap<String, BTreeMap<String, HashSet<String>>>,
    /// reaction event id -> (target message, emoji, reactor). A `kind:5` deletion names the reaction event, not the message,
    /// so this is how the removal finds the reaction to undo.
    pub reaction_ids: HashMap<String, (String, String, String)>,
    /// The next page of older top-level messages, from the relay's window bounds (`None` once exhausted).
    pub next_cursor: Option<(u64, String)>,
    pub has_more: bool,
    pub head_loaded: bool,
    /// Newest message time, for unread markers.
    pub latest: u64,
}

impl Store {
    /// Add an event of any kind the client cares about. Returns true when something visible changed.
    pub fn apply(&mut self, e: &Ev) -> bool {
        match e.kind {
            k if MESSAGE_KINDS.contains(&k) => {
                if self.deleted.contains(&e.id) {
                    return false;
                }
                self.latest = self.latest.max(e.created_at);
                self.msgs.insert(e.id.clone(), e.clone()).is_none()
            }
            KIND_EDIT => {
                let Some(target) = e.tag("e") else { return false };
                // only the author may edit, and only a newer edit replaces an older one
                let by_author = self.msgs.get(target).map(|m| m.pubkey == e.pubkey).unwrap_or(true);
                if !by_author {
                    return false;
                }
                let newer = self.edits.get(target).map(|(t, _)| e.created_at >= *t).unwrap_or(true);
                if newer {
                    self.edits.insert(target.to_owned(), (e.created_at, e.content.clone()));
                }
                newer
            }
            KIND_DELETION | KIND_DELETE_MESSAGE => {
                let mut changed = false;
                for t in e.tags_named("e") {
                    let Some(target) = t.get(1) else { continue };
                    // A deletion naming a reaction event undoes that reaction (NIP-09 over a kind:7); the target is a reaction,
                    // not a message, so it is not in `msgs` and must be pruned from `reactions` instead.
                    if let Some((msg, emoji, reactor)) = self.reaction_ids.remove(target) {
                        if let Some(bucket) = self.reactions.get_mut(&msg).and_then(|m| m.get_mut(&emoji)) {
                            bucket.remove(&reactor);
                            if bucket.is_empty() {
                                if let Some(m) = self.reactions.get_mut(&msg) {
                                    m.remove(&emoji);
                                }
                            }
                        }
                        changed = true;
                        continue;
                    }
                    self.deleted.insert(target.clone());
                    changed |= self.msgs.remove(target).is_some();
                }
                changed
            }
            KIND_REACTION => {
                let Some(target) = e.tag("e") else { return false };
                let added = self.reactions.entry(target.to_owned()).or_default().entry(e.content.clone()).or_default().insert(e.pubkey.clone());
                self.reaction_ids.insert(e.id.clone(), (target.to_owned(), e.content.clone(), e.pubkey.clone()));
                added
            }
            KIND_THREAD_SUMMARY => match Summary::from_event(e) {
                Some((root, s)) => {
                    let changed = self.summaries.get(&root) != Some(&s);
                    self.summaries.insert(root, s);
                    changed
                }
                None => false,
            },
            _ => false,
        }
    }

    pub fn content_of<'a>(&'a self, m: &'a Ev) -> (&'a str, bool) {
        match self.edits.get(&m.id) {
            Some((_, text)) => (text.as_str(), true),
            None => (m.content.as_str(), false),
        }
    }

    /// The root of `m`'s thread: itself if it is not a reply. A nested reply names its root in a `root` tag; a direct reply names
    /// only its parent, which is the root unless the parent is itself a reply (older clients), in which case we climb.
    pub fn thread_root<'a>(&'a self, m: &'a Ev) -> &'a str {
        self.root_inner(m, 0)
    }

    fn root_inner<'a>(&'a self, m: &'a Ev, depth: usize) -> &'a str {
        if let Some(root) = m.root_tag() {
            return root;
        }
        match m.reply_tag() {
            None => &m.id,
            Some(parent) => match self.msgs.get(parent) {
                Some(p) if depth < 32 => self.root_inner(p, depth + 1),
                _ => parent,
            },
        }
    }

    /// Messages in time order (oldest first), deleted ones excluded.
    fn sorted<'a>(&'a self, keep: impl Fn(&Ev) -> bool) -> Vec<&'a Ev> {
        let mut v: Vec<&Ev> = self.msgs.values().filter(|m| !self.deleted.contains(&m.id) && keep(m)).collect();
        v.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        v
    }

    /// What the channel timeline lists: top-level messages, plus replies whose parent is not loaded (so that nothing
    /// disappears), oldest first.
    pub fn timeline(&self) -> Vec<&Ev> {
        self.sorted(|m| m.is_top_level() || m.root_hint().map(|r| !self.msgs.contains_key(r)).unwrap_or(false))
    }

    /// A thread: the root (if loaded) and every message that belongs to it, oldest first.
    pub fn thread(&self, root: &str) -> Vec<&Ev> {
        self.sorted(|m| m.id == root || self.thread_root(m) == root)
    }

    /// Reply count and time of the latest reply for a timeline row: the relay's summary when we have it, otherwise what is
    /// loaded.
    pub fn replies_of(&self, m: &Ev) -> (u64, Option<u64>) {
        let loaded: Vec<&Ev> = self.msgs.values().filter(|r| r.id != m.id && !self.deleted.contains(&r.id) && self.thread_root(r) == m.id).collect();
        let from_loaded = (loaded.len() as u64, loaded.iter().map(|r| r.created_at).max());
        match self.summaries.get(&m.id) {
            Some(s) if s.reply_count >= from_loaded.0 => (s.reply_count, s.last_reply_at.or(from_loaded.1)),
            _ => from_loaded,
        }
    }

    pub fn reaction_summary(&self, id: &str) -> Vec<(String, usize)> {
        self.reactions.get(id).map(|m| m.iter().map(|(emoji, who)| (emoji.clone(), who.len())).collect()).unwrap_or_default()
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn ev(id: &str, at: u64, content: &str, tags: &[&[&str]]) -> Ev {
        Ev {
            id: id.into(),
            pubkey: "alice".into(),
            created_at: at,
            kind: 9,
            content: content.into(),
            tags: tags.iter().map(|t| t.iter().map(|s| s.to_string()).collect()).collect(),
        }
    }

    fn reply(id: &str, at: u64, parent: &str) -> Ev {
        ev(id, at, "reply", &[&["e", parent, "", "reply"]])
    }

    fn nested(id: &str, at: u64, root: &str, parent: &str) -> Ev {
        ev(id, at, "nested", &[&["e", root, "", "root"], &["e", parent, "", "reply"]])
    }

    #[test]
    fn tag_accessors_read_names_and_first_values() {
        let e = ev("a", 1, "x", &[&["h", "chan"], &["p", "alice"], &["p", "bob", "", "admin"], &["broadcast", "1"]]);
        assert_eq!(e.tag("h"), Some("chan"));
        assert_eq!(e.tag("nope"), None);
        assert_eq!(e.tag_values("p").collect::<Vec<_>>(), ["alice", "bob"]);
        assert!(e.has_tag("p", "bob") && !e.has_tag("p", "chan"), "the value must be on a tag of that name");
        assert_eq!(e.tags_named("p").nth(1).and_then(|t| t.get(3)).map(String::as_str), Some("admin"), "the whole tag is available");
        assert!(e.is_broadcast());
    }

    #[test]
    fn tags_decide_top_level_parent_and_root() {
        let top = ev("a", 1, "x", &[&["h", "chan"]]);
        let direct = reply("b", 2, "a");
        let deep = nested("c", 3, "a", "b");
        assert!(top.is_top_level() && !direct.is_top_level() && !deep.is_top_level());
        assert_eq!(direct.parent(), Some("a"));
        assert_eq!(deep.parent(), Some("b"));
        assert_eq!(deep.root_hint(), Some("a"));
        assert_eq!(direct.root_hint(), Some("a"));
        let mut broadcast = reply("d", 4, "a");
        broadcast.tags.push(vec!["broadcast".into(), "1".into()]);
        assert!(broadcast.is_top_level(), "a broadcast reply is listed on the timeline");
    }

    #[test]
    fn timeline_lists_top_level_only_and_orphans() {
        let mut s = Store::default();
        for e in [ev("a", 1, "one", &[]), reply("b", 2, "a"), nested("c", 3, "a", "b"), ev("d", 4, "two", &[]), reply("o", 5, "gone")] {
            s.apply(&e);
        }
        let ids: Vec<&str> = s.timeline().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a", "d", "o"], "replies stay in their thread; an orphan reply stays visible");
        assert_eq!(s.replies_of(&s.msgs["a"].clone()), (2, Some(3)), "nested replies count toward the top message");
    }

    #[test]
    fn thread_contains_root_and_all_descendants() {
        let mut s = Store::default();
        for e in [ev("a", 1, "one", &[]), reply("b", 2, "a"), nested("c", 3, "a", "b"), ev("d", 4, "other", &[]), reply("e", 5, "d")] {
            s.apply(&e);
        }
        let ids: Vec<&str> = s.thread("a").iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        // an old-style nested reply without a root tag still finds its root through its parent
        s.apply(&reply("f", 6, "b"));
        assert_eq!(s.thread_root(&s.msgs["f"].clone()), "a");
    }

    #[test]
    fn relay_summary_beats_what_is_loaded() {
        let mut s = Store::default();
        s.apply(&ev("a", 1, "one", &[]));
        s.apply(&reply("b", 2, "a"));
        let summary = Ev { kind: 39005, content: r#"{"reply_count":5,"descendant_count":5,"last_reply_at":99,"participants":["x"]}"#.into(), tags: vec![vec!["e".into(), "a".into()], vec!["d".into(), "a".into()]], ..ev("s", 9, "", &[]) };
        assert!(s.apply(&summary));
        assert_eq!(s.replies_of(&s.msgs["a"].clone()), (5, Some(99)));
        assert!(!s.apply(&summary), "the same summary again changes nothing");
    }

    #[test]
    fn edits_deletions_and_reactions() {
        let mut s = Store::default();
        s.apply(&ev("a", 1, "typo", &[]));
        let edit = Ev { kind: 40003, content: "fixed".into(), tags: vec![vec!["e".into(), "a".into()]], ..ev("e1", 5, "", &[]) };
        assert!(s.apply(&edit));
        assert_eq!(s.content_of(&s.msgs["a"].clone()), ("fixed", true));
        let stranger = Ev { pubkey: "mallory".into(), content: "hacked".into(), ..edit.clone() };
        let stranger = Ev { id: "e2".into(), created_at: 9, ..stranger };
        assert!(!s.apply(&stranger), "only the author may edit");
        assert_eq!(s.content_of(&s.msgs["a"].clone()).0, "fixed");
        let like = Ev { kind: 7, content: "👍".into(), tags: vec![vec!["e".into(), "a".into()]], ..ev("r1", 6, "", &[]) };
        assert!(s.apply(&like));
        assert!(!s.apply(&like), "a repeated reaction is not new");
        assert_eq!(s.reaction_summary("a"), vec![("👍".to_string(), 1)]);
        let del = Ev { kind: 5, tags: vec![vec!["e".into(), "a".into()]], ..ev("d1", 7, "", &[]) };
        assert!(s.apply(&del));
        assert!(s.timeline().is_empty());
        assert!(!s.apply(&ev("a", 1, "typo", &[])), "a deleted message does not come back");
    }

    #[test]
    fn a_deletion_naming_a_reaction_removes_it() {
        let mut s = Store::default();
        s.apply(&ev("a", 1, "one", &[]));
        let react = |id: &str, who: &str, emoji: &str| Ev { kind: 7, pubkey: who.into(), content: emoji.into(), tags: vec![vec!["e".into(), "a".into()]], ..ev(id, 6, "", &[]) };
        assert!(s.apply(&react("r1", "alice", "👍")));
        assert!(s.apply(&react("r2", "bob", "👍")));
        assert!(s.apply(&react("r3", "bob", "🎉")));
        assert_eq!(s.reaction_summary("a"), vec![("🎉".to_string(), 1), ("👍".to_string(), 2)]);
        // A removal (kind:5) names the reaction event, not the message: only that reactor's reaction goes.
        let remove = |id: &str, target: &str| Ev { kind: 5, pubkey: "bob".into(), tags: vec![vec!["e".into(), target.into()]], ..ev(id, 7, "", &[]) };
        assert!(s.apply(&remove("d1", "r3")), "removing a reaction is a visible change");
        assert_eq!(s.reaction_summary("a"), vec![("👍".to_string(), 2)]);
        assert_eq!(s.timeline().len(), 1, "the message itself is untouched");
        assert!(!s.apply(&remove("d2", "r3")), "the same removal again changes nothing");
        // Emptying an emoji's bucket drops it from the summary entirely.
        s.apply(&remove("d3", "r1"));
        s.apply(&remove("d4", "r2"));
        assert!(s.reaction_summary("a").is_empty(), "no reactions left to show");
    }

    #[test]
    fn channel_metadata_and_dm_naming() {
        let e = Ev { kind: 39000, tags: vec![vec!["d".into(), "chan1".into()], vec!["name".into(), "DM".into()]], ..ev("m", 3, "", &[]) };
        let c = Channel::from_event(&e).unwrap();
        assert!(c.is_dm() && c.id == "chan1");
        assert_eq!(c.marker(), "@", "a DM opens with a marker every font can draw");
    }

    #[test]
    fn channel_sections_come_from_the_metadata_tags() {
        let section = |name: &str, extra: &[&[&str]]| {
            let mut tags = vec![vec!["d".to_string(), "c".into()], vec!["name".into(), name.into()]];
            tags.extend(extra.iter().map(|t| t.iter().map(|s| s.to_string()).collect()));
            Channel::from_event(&Ev { kind: 39000, tags, ..ev("m", 3, "", &[]) }).unwrap().section
        };
        assert_eq!(section("general", &[]), Section::Channels);
        assert_eq!(section("general", &[&["public"], &["t", "stream"]]), Section::Channels);
        assert_eq!(section("team", &[&["private"]]), Section::Channels, "private and open channels are the same section");
        assert_eq!(section("chat", &[&["private"], &["t", "buzz.solo"]]), Section::Solo);
        assert_eq!(section("chat", &[&["t", "buzz.solo"]]), Section::Solo, "the label decides, as in the desktop app");
        assert_eq!(section("DM", &[&["private"]]), Section::Dms);
        assert_eq!(section("x", &[&["t", "other"]]), Section::Channels);
    }

    #[test]
    fn a_channel_knows_whether_it_is_private() {
        let channel = |tags: &[&[&str]]| {
            let mut t = vec![vec!["d".to_string(), "c".into()], vec!["name".into(), "general".into()]];
            t.extend(tags.iter().map(|tag| tag.iter().map(|s| s.to_string()).collect()));
            Channel::from_event(&Ev { kind: 39000, tags: t, ..ev("m", 3, "", &[]) }).unwrap()
        };
        assert!(!channel(&[&["public"]]).is_private, "an open channel says [\"public\"]");
        assert!(channel(&[&["private"], &["restricted"], &["closed"]]).is_private, "a private channel says [\"private\"]");
        assert!(!channel(&[]).is_private, "neither tag: treated as open rather than guessing");
    }
}
