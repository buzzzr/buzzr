//! A plugin is a folder under `~/.config/buzzr/plugins` with a `plugin.json` in it.
//!
//! The manifest says what to run and what the plugin is allowed to do; everything else (commands, status, panels) is
//! negotiated over the protocol when the process says it is ready. A manifest that is wrong is reported by `/plugins`
//! and by `buzzr plugins list` instead of being loaded.

use crate::contrib::valid_id;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const API_VERSION: u32 = 1;
/// A file in the plugin folder that turns it off without deleting anything.
pub const DISABLED: &str = "DISABLED";

/// What this build can actually enforce. An unknown permission is dropped and reported: claiming a permission buzzr does
/// not implement would only mislead the person installing the plugin.
pub const KNOWN_PERMISSIONS: &[&str] = &["read:visible", "messages:write"];

/// The heartbeat a manifest asks for with `tickMs` is clamped to this range: below it a plugin would only be asking for a
/// busy loop (the host's poll is the granularity), and above it a clock would stop being a clock.
pub const MIN_TICK_MS: u64 = 250;
pub const MAX_TICK_MS: u64 = 60_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub api_version: u32,
    #[serde(default)]
    pub description: String,
    /// The command line of the plugin process, run with the plugin folder as its working directory.
    pub entry: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Opt-in heartbeat: buzzr sends `tick` this often so a plugin can refresh something time-based. `0` (or absent)
    /// means never — a plugin that only reacts to commands and snapshots does not want to be woken.
    #[serde(default)]
    pub tick_ms: u64,
}

/// A folder found under the plugins directory, valid or not.
#[derive(Clone, Debug)]
pub struct Discovered {
    pub dir: PathBuf,
    /// `None` when the manifest is missing or wrong; `problem` says why.
    pub manifest: Option<Manifest>,
    pub problem: Option<String>,
    pub disabled: bool,
}

impl Discovered {
    pub fn id(&self) -> String {
        self.manifest.as_ref().map(|m| m.id.clone()).unwrap_or_else(|| self.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
    }
}

/// Read one plugin folder. The directory name must match the manifest id: a copied folder then never silently turns into a
/// second plugin with the same id.
pub fn load(dir: &Path) -> Result<Manifest, String> {
    let file = dir.join("plugin.json");
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut m: Manifest = serde_json::from_str(&text).map_err(|e| format!("plugin.json: {e}"))?;
    let dir_name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !valid_id(&m.id) {
        return Err(format!("the id {:?} must be lowercase letters, digits, '-', '_' or '.'", m.id));
    }
    if m.id != dir_name {
        return Err(format!("the id {:?} does not match the folder name {:?}", m.id, dir_name));
    }
    if m.api_version != API_VERSION {
        return Err(format!("apiVersion {} is not supported (this buzzr speaks {API_VERSION})", m.api_version));
    }
    m.name = crate::contrib::clean_line(&m.name).trim().to_string();
    if m.name.is_empty() {
        m.name = m.id.clone();
    }
    m.description = crate::contrib::clean_line(&m.description).trim().to_string();
    if m.entry.is_empty() || m.entry[0].trim().is_empty() {
        return Err("entry is empty: name the program to run".into());
    }
    if m.entry.len() > 16 || m.entry.iter().map(|a| a.len()).sum::<usize>() > 4096 {
        return Err("entry is too long".into());
    }
    if m.entry.iter().any(|a| a.contains('\0')) {
        return Err("entry contains a NUL byte".into());
    }
    m.permissions.retain(|p| KNOWN_PERMISSIONS.contains(&p.as_str()));
    m.permissions.sort();
    m.permissions.dedup();
    m.tick_ms = if m.tick_ms == 0 { 0 } else { m.tick_ms.clamp(MIN_TICK_MS, MAX_TICK_MS) };
    Ok(m)
}

/// Every folder under `root`, sorted by id. Unreadable or invalid folders are kept (with `problem`) so the UI can show them.
pub fn scan(root: &Path) -> Vec<Discovered> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<Discovered> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| {
            let dir = e.path();
            let (manifest, problem) = match load(&dir) {
                Ok(m) => (Some(m), None),
                Err(problem) => (None, Some(problem)),
            };
            Discovered {
                disabled: dir.join(DISABLED).exists(),
                manifest,
                problem,
                dir,
            }
        })
        .collect();
    out.sort_by_key(|d| d.id());
    out
}

/// Newest modification time in a folder, for hot reload. Bounded depth so a plugin cannot make the scan expensive.
fn newest(dir: &Path, depth: usize) -> SystemTime {
    let mut stamp = std::fs::metadata(dir).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    if depth >= 4 {
        return stamp;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return stamp };
    for e in entries.flatten().take(500) {
        let Ok(kind) = e.file_type() else { continue };
        let time = if kind.is_dir() {
            newest(&e.path(), depth + 1)
        } else {
            e.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
        };
        if time > stamp {
            stamp = time;
        }
    }
    stamp
}

/// The stamps of every plugin folder, to notice an edit between two scans.
pub fn stamps(root: &Path) -> BTreeMap<PathBuf, SystemTime> {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| (e.path(), newest(&e.path(), 0)))
                .collect()
        })
        .unwrap_or_default()
}

/// Turn a plugin on or off without deleting it.
pub fn set_enabled(dir: &Path, enabled: bool) -> std::io::Result<()> {
    let marker = dir.join(DISABLED);
    if enabled {
        match std::fs::remove_file(&marker) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    } else {
        std::fs::write(marker, "This plugin is off. `buzzr plugins enable <id>` turns it back on.\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(root: &str) -> PathBuf {
        std::env::temp_dir().join(format!("buzzr-test-{}-{}", root, uuid::Uuid::new_v4()))
    }

    fn write(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("plugin.json"), body).unwrap();
    }

    #[test]
    fn a_valid_manifest_loads_and_keeps_only_known_permissions() {
        let root = temp("manifest");
        let dir = root.join("todo");
        write(
            &dir,
            r#"{"id":"todo","name":"  TODO  ","apiVersion":1,"entry":["sh","plugin.sh"],"permissions":["messages:write","root","read:visible"]}"#,
        );
        let m = load(&dir).unwrap();
        assert_eq!(m.id, "todo");
        assert_eq!(m.name, "TODO");
        assert_eq!(m.permissions, vec!["messages:write", "read:visible"]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_heartbeat_a_manifest_asks_for_is_clamped_and_off_by_default() {
        let root = temp("tick");
        let load_with = |body: &str| {
            let dir = root.join("tick");
            write(&dir, body);
            load(&dir).map(|m| m.tick_ms)
        };
        assert_eq!(load_with(r#"{"id":"tick","name":"T","apiVersion":1,"entry":["sh"]}"#).unwrap(), 0, "no tickMs means no heartbeat");
        assert_eq!(load_with(r#"{"id":"tick","name":"T","apiVersion":1,"entry":["sh"],"tickMs":0}"#).unwrap(), 0);
        assert_eq!(load_with(r#"{"id":"tick","name":"T","apiVersion":1,"entry":["sh"],"tickMs":1000}"#).unwrap(), 1000);
        assert_eq!(load_with(r#"{"id":"tick","name":"T","apiVersion":1,"entry":["sh"],"tickMs":1}"#).unwrap(), MIN_TICK_MS);
        assert_eq!(load_with(r#"{"id":"tick","name":"T","apiVersion":1,"entry":["sh"],"tickMs":999999}"#).unwrap(), MAX_TICK_MS);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_manifest_that_would_load_something_else_is_refused() {
        let root = temp("manifest-bad");
        let cases = [
            ("one", r#"{"id":"other","name":"x","apiVersion":1,"entry":["sh"]}"#, "does not match"),
            ("two", r#"{"id":"two","name":"x","apiVersion":2,"entry":["sh"]}"#, "not supported"),
            ("three", r#"{"id":"three","name":"x","apiVersion":1,"entry":[]}"#, "entry is empty"),
            ("four", r#"{"id":"four","name":"x","apiVersion":1}"#, "entry"),
            ("five", r#"{"id":"../x","name":"x","apiVersion":1,"entry":["sh"]}"#, "lowercase"),
            ("six", r#"not json"#, "plugin.json"),
        ];
        for (folder, body, needle) in cases.into_iter() {
            let dir = root.join(folder);
            write(&dir, body);
            let err = load(&dir).unwrap_err();
            assert!(err.contains(needle), "{body}: {err}");
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scanning_reports_broken_folders_and_the_disabled_marker() {
        let root = temp("scan");
        write(&root.join("good"), r#"{"id":"good","name":"Good","apiVersion":1,"entry":["sh"]}"#);
        write(&root.join("bad"), r#"{"id":"bad","name":"Bad","apiVersion":9,"entry":["sh"]}"#);
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("not-a-dir"), "x").unwrap();
        let found = scan(&root);
        let ids: Vec<String> = found.iter().map(Discovered::id).collect();
        assert_eq!(ids, vec!["bad", "empty", "good"]);
        assert!(found[0].manifest.is_none() && found[0].problem.as_deref().unwrap_or("").contains("not supported"));
        assert!(found[1].manifest.is_none() && found[1].problem.as_deref().unwrap_or("").contains("plugin.json"));
        assert!(found[2].manifest.is_some());

        set_enabled(&root.join("good"), false).unwrap();
        assert!(scan(&root).iter().find(|d| d.id() == "good").unwrap().disabled);
        set_enabled(&root.join("good"), true).unwrap();
        assert!(!scan(&root).iter().find(|d| d.id() == "good").unwrap().disabled);
        set_enabled(&root.join("good"), true).unwrap(); // turning an enabled plugin on again is fine
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_example_plugins_in_the_repository_are_valid_manifests() {
        for (name, tick) in [("hello", 0), ("clock", 1000)] {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/plugins").join(name);
            let m = load(&dir).unwrap_or_else(|e| panic!("examples/plugins/{name} does not load: {e}"));
            assert_eq!(m.id, name);
            assert!(!m.entry.is_empty());
            assert_eq!(m.tick_ms, tick, "examples/plugins/{name}: tickMs");
        }
    }

    #[test]
    fn a_changed_file_changes_the_stamp() {
        let root = temp("stamp");
        let dir = root.join("todo");
        write(&dir, r#"{"id":"todo","name":"TODO","apiVersion":1,"entry":["sh"]}"#);
        let before = stamps(&root);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("plugin.sh"), "echo hi\n").unwrap();
        let after = stamps(&root);
        assert_ne!(before, after);
        std::fs::remove_dir_all(root).ok();
    }
}
