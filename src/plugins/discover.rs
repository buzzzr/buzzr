//! A plugin is a folder under `~/.config/buzzr/plugins` with an executable entry file in it.
//!
//! There is no manifest. The folder name is the plugin's id, the entry file's shebang picks the interpreter, and
//! everything else the plugin declares — its display name, what it is allowed to do, and whether it wants a heartbeat —
//! is declared in its own code and sent over the protocol when the process says it is `ready`. A folder whose entry is
//! missing or not executable is reported by `/plugins` and by `buzzr plugins list` instead of being loaded.

use crate::contrib::valid_id;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The contract version. A plugin reads the one in `hello` and refuses the host if it cannot speak it: a folder can no
/// longer pre-declare a version, so the side that knows its own needs is the side that checks.
pub const API_VERSION: u32 = 2;
/// A file in the plugin folder that turns it off without deleting anything.
pub const DISABLED: &str = "DISABLED";
/// What this build can actually enforce. An unknown permission is dropped and reported: claiming a permission buzzr does
/// not implement would only mislead the person installing the plugin.
pub const KNOWN_PERMISSIONS: &[&str] = &["read:visible", "messages:write"];
/// The heartbeat a plugin asks for with `tickMs` is clamped to this range: below it a plugin would only be asking for a
/// busy loop, above it for nothing a person would notice.
pub const MIN_TICK_MS: u64 = 250;
pub const MAX_TICK_MS: u64 = 60_000;
/// The entry file names buzzr looks for, in this order. The first that exists is the one it runs, through its shebang,
/// so buzzr keeps no table of interpreters and any language stays possible.
pub const ENTRY_NAMES: &[&str] = &["main.ts", "main.js", "main.mjs", "main.py", "main.rb", "main.sh", "main", "run"];

/// A folder found under the plugins directory, valid or not.
#[derive(Clone, Debug)]
pub struct Discovered {
    pub dir: PathBuf,
    /// The file buzzr would run; `None` when the folder would not load and `problem` says why.
    pub entry: Option<PathBuf>,
    pub problem: Option<String>,
    pub disabled: bool,
}

impl Discovered {
    /// The plugin's id: the folder name. Kept even for a broken folder, so the UI can name it.
    pub fn id(&self) -> String {
        self.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// Find the entry file of one plugin folder, or say why there is none. The folder name must be a safe id: it is what
/// the plugin is called everywhere, and it reaches paths and JSON.
pub fn load(dir: &Path) -> Result<PathBuf, String> {
    let id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !valid_id(&id) {
        return Err(format!("the folder name {id:?} must be lowercase letters, digits, '-', '_' or '.'"));
    }
    for name in ENTRY_NAMES {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        let meta = std::fs::metadata(&path).map_err(|e| format!("{name}: {e}"))?;
        if !meta.is_file() {
            return Err(format!("{name} is not a file"));
        }
        if !is_executable(&meta) {
            return Err(format!("{name} is not executable: run `chmod +x {name}` in the plugin folder"));
        }
        return Ok(path);
    }
    Err(format!("no entry file: add an executable one of {}", ENTRY_NAMES.join(", ")))
}

/// A file buzzr can run has the executable bit set. On a host without that bit, let the spawn decide.
#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    true
}

/// Every folder under `root`, sorted by id. Unreadable or invalid folders are kept (with `problem`) so the UI can show
/// them.
pub fn scan(root: &Path) -> Vec<Discovered> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<Discovered> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| {
            let dir = e.path();
            let (entry, problem) = match load(&dir) {
                Ok(path) => (Some(path), None),
                Err(problem) => (None, Some(problem)),
            };
            Discovered {
                disabled: dir.join(DISABLED).exists(),
                entry,
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

    /// Write an entry file the way a plugin ships one: with a shebang and the executable bit.
    fn entry(dir: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn the_folder_name_is_the_id_and_its_executable_is_the_entry() {
        let root = temp("discover");
        let dir = root.join("todo");
        let path = entry(&dir, "main.sh", "#!/bin/sh\nexit 0\n");
        assert_eq!(load(&dir).unwrap(), path);
        let found = scan(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id(), "todo");
        assert_eq!(found[0].entry.as_deref(), Some(path.as_path()));
        assert!(found[0].problem.is_none());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_first_entry_name_that_exists_wins() {
        let root = temp("order");
        let dir = root.join("p");
        entry(&dir, "main.py", "#!/usr/bin/env python3\n");
        entry(&dir, "main.sh", "#!/bin/sh\n");
        assert!(load(&dir).unwrap().ends_with("main.py"), "the order in ENTRY_NAMES decides");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_folder_that_would_not_load_is_refused_with_a_reason() {
        let root = temp("discover-bad");
        // no entry file at all
        std::fs::create_dir_all(root.join("empty")).unwrap();
        assert!(load(&root.join("empty")).unwrap_err().contains("no entry file"));
        // a folder name that is not a safe id
        std::fs::create_dir_all(root.join("Bad Name")).unwrap();
        assert!(load(&root.join("Bad Name")).unwrap_err().contains("lowercase"));
        // an entry that exists but is a directory
        std::fs::create_dir_all(root.join("dirmain/main.sh")).unwrap();
        assert!(load(&root.join("dirmain")).unwrap_err().contains("not a file"));
        std::fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_without_the_executable_bit_is_refused() {
        let root = temp("noexec");
        let dir = root.join("p");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.sh"), "#!/bin/sh\n").unwrap(); // no +x
        let err = load(&dir).unwrap_err();
        assert!(err.contains("not executable"), "{err}");
        assert!(err.contains("chmod +x"), "{err}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scanning_reports_broken_folders_and_the_disabled_marker() {
        let root = temp("scan");
        entry(&root.join("good"), "main.sh", "#!/bin/sh\n");
        std::fs::create_dir_all(root.join("bad")).unwrap();
        std::fs::write(root.join("not-a-dir"), "x").unwrap();
        let found = scan(&root);
        let ids: Vec<String> = found.iter().map(Discovered::id).collect();
        assert_eq!(ids, vec!["bad", "good"]);
        assert!(found[0].entry.is_none() && found[0].problem.as_deref().unwrap_or("").contains("no entry file"));
        assert!(found[1].entry.is_some());

        set_enabled(&root.join("good"), false).unwrap();
        assert!(scan(&root).iter().find(|d| d.id() == "good").unwrap().disabled);
        set_enabled(&root.join("good"), true).unwrap();
        assert!(!scan(&root).iter().find(|d| d.id() == "good").unwrap().disabled);
        set_enabled(&root.join("good"), true).unwrap(); // turning an enabled plugin on again is fine
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_example_plugins_in_the_repository_load() {
        for id in ["hello", "clock"] {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/plugins").join(id);
            let path = load(&dir).unwrap_or_else(|e| panic!("examples/plugins/{id} does not load: {e}"));
            assert!(path.exists(), "examples/plugins/{id}: {:?}", path);
        }
    }

    /// The SDK is one file. `examples/plugins/clock` ships a copy of it so that the example folder can be copied on its own;
    /// nothing but this test keeps the copy in step with `sdk/typescript/buzzr.ts`.
    #[test]
    fn the_clock_example_carries_an_identical_copy_of_the_sdk() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read(root.join("sdk/typescript/buzzr.ts")).expect("sdk/typescript/buzzr.ts");
        let vendored = std::fs::read(root.join("examples/plugins/clock/buzzr.ts")).expect("examples/plugins/clock/buzzr.ts");
        assert_eq!(source, vendored, "examples/plugins/clock/buzzr.ts must be a copy of sdk/typescript/buzzr.ts, byte for byte");
    }

    #[test]
    fn a_changed_file_changes_the_stamp() {
        let root = temp("stamp");
        let dir = root.join("todo");
        entry(&dir, "main.sh", "#!/bin/sh\n");
        let before = stamps(&root);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("main.sh"), "#!/bin/sh\necho hi\n").unwrap();
        let after = stamps(&root);
        assert_ne!(before, after);
        std::fs::remove_dir_all(root).ok();
    }
}
