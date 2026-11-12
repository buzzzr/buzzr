//! Where buzzr keeps its settings, and the few environment variables it reads.
//!
//! Files (all in one directory, `$BUZZR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/buzzr`, else `~/.config/buzzr`):
//! - `config.json`: `{"relay": "wss://..."}`, written by the first-run setup
//! - `read-state.json`: the last-read positions per channel and per thread, plus the local unread marks (see `load_read_state`)
//! - `archived.json`: the channel ids the user archived, kept out of the channel list (see `load_archived`)
//! - `drafts.json`: the messages typed but not sent, per channel and per thread (see `load_drafts`)
//! - `key.enc`: the private key sealed with a password (see `keyfile.rs`)
//! - `key`: the private key in plain text, for people who chose that (mode 600)

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// `BUZZR_<NAME>`, if set and not empty.
pub fn env(name: &str) -> Option<String> {
    std::env::var(format!("BUZZR_{name}")).ok().filter(|v| !v.is_empty())
}

#[derive(Clone, Debug)]
pub struct Paths {
    dir: PathBuf,
}

impl Paths {
    pub fn new(dir: PathBuf) -> Paths {
        Paths { dir }
    }

    pub fn from_env() -> Paths {
        if let Some(d) = env("CONFIG_DIR") {
            return Paths::new(PathBuf::from(d));
        }
        let base = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config"));
        Paths::new(base.join("buzzr"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn config(&self) -> PathBuf {
        self.dir.join("config.json")
    }
    pub fn encrypted_key(&self) -> PathBuf {
        self.dir.join("key.enc")
    }
    pub fn plain_key(&self) -> PathBuf {
        self.dir.join("key")
    }
    /// Plugins live next to the settings by default; `BUZZR_PLUGINS_DIR` moves them (tests, a throwaway profile).
    pub fn plugins(&self) -> PathBuf {
        env("PLUGINS_DIR").map(PathBuf::from).unwrap_or_else(|| self.dir.join("plugins"))
    }
    /// stderr of every plugin process, one file per plugin.
    pub fn plugin_logs(&self) -> PathBuf {
        self.dir.join("plugin-logs")
    }
    /// What the release feed last learned (`release.rs`): when we asked, and the newest version we saw.
    pub fn release_state(&self) -> PathBuf {
        self.dir.join("release-state.json")
    }
    /// The last-read positions per channel and per thread (`read-state.json`).
    pub fn read_state(&self) -> PathBuf {
        self.dir.join("read-state.json")
    }
    /// The channel ids the user archived: out of the channel list, found again by search (`archived.json`).
    pub fn archived(&self) -> PathBuf {
        self.dir.join("archived.json")
    }
    /// The messages typed but not sent, per channel and thread (`drafts.json`).
    pub fn drafts(&self) -> PathBuf {
        self.dir.join("drafts.json")
    }
}

/// Where the reader left off: for each channel and each thread, the `created_at` read through. `channels` is keyed by channel
/// id, `threads` by the id of the message a thread starts from (its root). `marks` holds the messages the reader forced unread
/// with `u`, per channel: that is intent, not a position, so it is kept apart from the frontiers and never leaves this device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadState {
    pub channels: HashMap<String, u64>,
    pub threads: HashMap<String, u64>,
    pub marks: HashMap<String, BTreeSet<String>>,
}

/// The last-read positions, from `read-state.json`. A missing or unreadable file is simply no state at all, never an error:
/// unread counts then start from zero, which is how they behaved before this file. A file written before threads were tracked
/// has only `channels`, and one written before the marks has no `marks`: both load with the missing part empty.
pub fn load_read_state(path: &Path) -> ReadState {
    let Ok(text) = std::fs::read_to_string(path) else {
        return ReadState::default();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        return ReadState::default();
    };
    let positions = |key: &str| {
        doc.get(key)
            .and_then(|c| c.as_object())
            .map(|o| o.iter().filter_map(|(id, at)| at.as_u64().map(|t| (id.clone(), t))).collect())
            .unwrap_or_default()
    };
    let mut marks: HashMap<String, BTreeSet<String>> = HashMap::new();
    if let Some(obj) = doc.get("marks").and_then(|m| m.as_object()) {
        for (id, list) in obj {
            let set: BTreeSet<String> = list.as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            if !set.is_empty() {
                marks.insert(id.clone(), set);
            }
        }
    }
    ReadState { channels: positions("channels"), threads: positions("threads"), marks }
}

/// Write the last-read positions and the unread marks (mode 600, atomically). Sorted keys, so an unchanged state is an
/// unchanged file, and an empty mark set leaves no trace of its channel.
pub fn save_read_state(path: &Path, state: &ReadState) -> std::io::Result<()> {
    let channels: BTreeMap<&str, u64> = state.channels.iter().map(|(id, at)| (id.as_str(), *at)).collect();
    let threads: BTreeMap<&str, u64> = state.threads.iter().map(|(id, at)| (id.as_str(), *at)).collect();
    let marks: BTreeMap<&str, Vec<&str>> =
        state.marks.iter().filter(|(_, ids)| !ids.is_empty()).map(|(id, ids)| (id.as_str(), ids.iter().map(String::as_str).collect())).collect();
    let doc = serde_json::json!({ "channels": channels, "threads": threads, "marks": marks });
    write_private(path, &(serde_json::to_string_pretty(&doc).expect("a json value serialises") + "\n"))
}

/// The archived channel ids, from `archived.json`. A missing or unreadable file is simply nothing archived, never an error: the
/// list then shows every channel, which is how it behaved before this file. Archiving is a per-user view preference, so it is
/// kept here and not on the relay — a channel is a relay event everyone sees, unlike this.
pub fn load_archived(path: &Path) -> HashSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashSet::new();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashSet::new();
    };
    doc.get("channels")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// Write the archived channel ids (mode 600, atomically). Sorted, so an unchanged set is an unchanged file.
pub fn save_archived(path: &Path, ids: &HashSet<String>) -> std::io::Result<()> {
    let mut channels: Vec<&str> = ids.iter().map(String::as_str).collect();
    channels.sort_unstable();
    let doc = serde_json::json!({ "channels": channels });
    write_private(path, &(serde_json::to_string_pretty(&doc).expect("a json value serialises") + "\n"))
}

/// The messages typed but not sent, from `drafts.json`: keyed by channel id, and inside a channel by the thread the text
/// is a reply into — the empty key is the channel's own timeline, which no message id can be. A missing or unreadable file
/// is simply no drafts, never an error. What is in the box is not a relay event until it is sent, so it is kept here, on
/// this device, like the read positions.
pub fn load_drafts(path: &Path) -> HashMap<(String, Option<String>), String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashMap::new();
    };
    let mut drafts = HashMap::new();
    let Some(channels) = doc.get("drafts").and_then(|d| d.as_object()) else {
        return drafts;
    };
    for (channel, threads) in channels {
        let Some(threads) = threads.as_object() else { continue };
        for (thread, text) in threads {
            let Some(text) = text.as_str().filter(|t| !t.is_empty()) else { continue };
            let root = (!thread.is_empty()).then(|| thread.clone());
            drafts.insert((channel.clone(), root), text.to_owned());
        }
    }
    drafts
}

/// Write the drafts (mode 600, atomically). Sorted by channel and thread, so an unchanged set is an unchanged file, and an
/// empty draft leaves no trace of the place it was typed in.
pub fn save_drafts(path: &Path, drafts: &HashMap<(String, Option<String>), String>) -> std::io::Result<()> {
    let mut channels: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    for ((channel, root), text) in drafts {
        if text.is_empty() {
            continue;
        }
        channels.entry(channel.as_str()).or_default().insert(root.as_deref().unwrap_or(""), text.as_str());
    }
    let doc = serde_json::json!({ "drafts": channels });
    write_private(path, &(serde_json::to_string_pretty(&doc).expect("a json value serialises") + "\n"))
}

/// Turn what a person typed into a relay URL: a bare host gets `wss://`, the scheme must be ws, wss, http or https.
pub fn normalize_relay(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("no relay entered".into());
    }
    let (scheme, rest) = s.split_once("://").unwrap_or(("wss", s));
    if !matches!(scheme, "ws" | "wss" | "http" | "https") {
        return Err(format!("the relay address must start with wss://, ws://, https:// or http:// (not {scheme}://)"));
    }
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() || rest.starts_with('/') || rest.contains(char::is_whitespace) {
        return Err("that is not a valid relay address".into());
    }
    Ok(format!("{scheme}://{rest}"))
}

/// The relay saved by the first-run setup, if any.
pub fn load_relay(paths: &Paths) -> Option<String> {
    let text = std::fs::read_to_string(paths.config()).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    normalize_relay(doc.get("relay")?.as_str()?).ok()
}

pub fn save_relay(paths: &Paths, relay: &str) -> std::io::Result<()> {
    let doc = serde_json::json!({ "relay": relay });
    write_private(&paths.config(), &(serde_json::to_string_pretty(&doc).expect("a json value serialises") + "\n"))
}

/// Write a file that only the owner can read (directory 700, file 600), replacing any earlier one atomically.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_addresses_are_normalised_or_refused() {
        assert_eq!(normalize_relay("chat.example.com").unwrap(), "wss://chat.example.com");
        assert_eq!(normalize_relay("  wss://chat.example.com/ ").unwrap(), "wss://chat.example.com");
        assert_eq!(normalize_relay("http://100.64.0.1:3000").unwrap(), "http://100.64.0.1:3000");
        assert!(normalize_relay("").is_err());
        assert!(normalize_relay("ftp://example.com").is_err());
        assert!(normalize_relay("wss://").is_err());
        assert!(normalize_relay("wss://has space.example.com").is_err());
    }

    #[test]
    fn the_saved_relay_is_read_back_and_the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let paths = Paths::new(std::env::temp_dir().join(format!("buzzr-test-{}", uuid::Uuid::new_v4())));
        assert_eq!(load_relay(&paths), None);
        save_relay(&paths, "wss://chat.example.com").unwrap();
        assert_eq!(load_relay(&paths).as_deref(), Some("wss://chat.example.com"));
        assert_eq!(std::fs::metadata(paths.config()).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn the_last_read_positions_are_read_back_and_the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let paths = Paths::new(std::env::temp_dir().join(format!("buzzr-test-{}", uuid::Uuid::new_v4())));
        assert_eq!(load_read_state(&paths.read_state()), ReadState::default(), "no file yet: no state, not an error");
        let mut state = ReadState::default();
        state.channels.insert("c1".to_owned(), 1700);
        state.channels.insert("c2".to_owned(), 42);
        state.threads.insert("t1".to_owned(), 9);
        state.marks.insert("c1".to_owned(), ["m2".to_owned(), "m1".to_owned()].into_iter().collect());
        save_read_state(&paths.read_state(), &state).unwrap();
        assert_eq!(load_read_state(&paths.read_state()), state);
        assert_eq!(std::fs::metadata(paths.read_state()).unwrap().permissions().mode() & 0o777, 0o600);
        // a file written before threads were tracked loads with an empty `threads`, rather than losing the channel positions
        std::fs::write(paths.read_state(), "{\"channels\":{\"c1\":7}}").unwrap();
        let old = load_read_state(&paths.read_state());
        assert_eq!(old.channels.get("c1"), Some(&7));
        assert!(old.threads.is_empty());
        assert!(old.marks.is_empty(), "a file written before the marks has none");
        // a damaged file is no state either, rather than a panic on the next start
        std::fs::write(paths.read_state(), "not json").unwrap();
        assert_eq!(load_read_state(&paths.read_state()), ReadState::default());
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn unread_marks_are_written_in_order_and_a_channel_without_one_leaves_no_trace() {
        let paths = Paths::new(std::env::temp_dir().join(format!("buzzr-test-{}", uuid::Uuid::new_v4())));
        let mut state = ReadState::default();
        state.marks.insert("c1".to_owned(), ["m2".to_owned(), "m1".to_owned()].into_iter().collect());
        state.marks.insert("c2".to_owned(), BTreeSet::new());
        save_read_state(&paths.read_state(), &state).unwrap();
        let text = std::fs::read_to_string(paths.read_state()).unwrap();
        let (m1, m2) = (text.find("m1").expect("the first mark is written"), text.find("m2").expect("the second mark is written"));
        assert!(m1 < m2, "the marks are written sorted, so an unchanged state is an unchanged file: {text}");
        assert!(!text.contains("c2"), "a channel with no mark left is not written at all: {text}");
        assert_eq!(load_read_state(&paths.read_state()).marks.get("c1").map(|s| s.len()), Some(2));
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn the_archived_channels_are_read_back_and_the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let paths = Paths::new(std::env::temp_dir().join(format!("buzzr-test-{}", uuid::Uuid::new_v4())));
        assert!(load_archived(&paths.archived()).is_empty(), "no file yet: nothing archived, not an error");
        let ids: HashSet<String> = ["c1".to_owned(), "c2".to_owned()].into_iter().collect();
        save_archived(&paths.archived(), &ids).unwrap();
        assert_eq!(load_archived(&paths.archived()), ids);
        assert_eq!(std::fs::metadata(paths.archived()).unwrap().permissions().mode() & 0o777, 0o600);
        // sorted, so an unchanged set is an unchanged file
        let text = std::fs::read_to_string(paths.archived()).unwrap();
        assert!(text.find("c1").unwrap() < text.find("c2").unwrap(), "the ids are written sorted: {text}");
        // a damaged file is nothing archived either, rather than a panic on the next start
        std::fs::write(paths.archived(), "not json").unwrap();
        assert!(load_archived(&paths.archived()).is_empty());
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn the_drafts_are_read_back_per_channel_and_thread_and_the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let paths = Paths::new(std::env::temp_dir().join(format!("buzzr-test-{}", uuid::Uuid::new_v4())));
        assert!(load_drafts(&paths.drafts()).is_empty(), "no file yet: no drafts, not an error");
        let mut drafts: HashMap<(String, Option<String>), String> = HashMap::new();
        drafts.insert(("c1".to_owned(), None), "a half-written message".to_owned());
        drafts.insert(("c1".to_owned(), Some("root1".to_owned())), "a half-written reply".to_owned());
        drafts.insert(("c2".to_owned(), Some("root2".to_owned())), "another one".to_owned());
        drafts.insert(("c3".to_owned(), None), String::new());
        save_drafts(&paths.drafts(), &drafts).unwrap();
        drafts.remove(&("c3".to_owned(), None));
        assert_eq!(load_drafts(&paths.drafts()), drafts, "each draft comes back where it was typed, and an empty one is dropped");
        assert_eq!(std::fs::metadata(paths.drafts()).unwrap().permissions().mode() & 0o777, 0o600);
        // sorted, so an unchanged set is an unchanged file
        let text = std::fs::read_to_string(paths.drafts()).unwrap();
        assert!(text.find("c1").unwrap() < text.find("c2").unwrap(), "the channels are written sorted: {text}");
        assert!(!text.contains("c3"), "a channel whose draft was sent leaves no trace: {text}");
        // a damaged file is no drafts either, rather than a panic on the next start
        std::fs::write(paths.drafts(), "not json").unwrap();
        assert!(load_drafts(&paths.drafts()).is_empty());
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }
}
