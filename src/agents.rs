//! Local agents: the identities buzzr makes for agents that run on this machine.
//!
//! An agent is a Buzz identity of its own — a keypair, not the person's — together with the harness that will speak for
//! it: a local Claude Code, driven over ACP (the Agent Client Protocol). One record per agent lives in `agents.json` in
//! the config directory (see `config.rs`), and each agent gets a folder of its own to work in.
//!
//! An agent's key is the agent's, not the user's, and buzzr has no credential store to seal it in, so it is kept in that
//! mode-600 file: the same warning the setup gives for a plain key file applies — whoever can read the file can act as
//! the agent. `App` never holds a key: it keeps `Agent` (no secret), and the loop in `main.rs` makes the key when a
//! creation is accepted.

use crate::config::{self, Paths};
use nostr::nips::nip19::ToBech32;
use nostr::Keys;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// The harness a new agent is created with: a local Claude Code that speaks ACP on stdio.
pub const HARNESS: &str = "claude-code-acp";

/// The longest name accepted, so a name always fits on one line.
const NAME_MAX: usize = 64;

/// The longest instruction text kept, so a hand-edited file cannot grow the config without bound.
const INSTRUCTIONS_MAX: usize = 4096;

/// One local agent: everything the panel shows. The private key it speaks with is not here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    /// The identity's public key (hex).
    pub pubkey: String,
    /// What will run it: `claude-code-acp`.
    pub harness: String,
    /// The folder the harness works in.
    pub workspace: String,
    /// What the agent was told to do; free text, may be empty.
    pub instructions: String,
    pub created_at: u64,
}

/// What one agent looks like in `agents.json`. `nsec` is the only field `Agent` does not carry.
#[derive(serde::Deserialize, serde::Serialize)]
struct Record {
    name: String,
    pubkey: String,
    nsec: String,
    harness: String,
    workspace: String,
    #[serde(default)]
    instructions: String,
    #[serde(default)]
    created_at: u64,
}

impl Record {
    fn agent(&self) -> Agent {
        Agent {
            name: self.name.clone(),
            pubkey: self.pubkey.clone(),
            harness: self.harness.clone(),
            workspace: self.workspace.clone(),
            instructions: self.instructions.clone(),
            created_at: self.created_at,
        }
    }
}

/// The whole file: `{"agents": [...]}`.
#[derive(Default, serde::Deserialize, serde::Serialize)]
struct File {
    #[serde(default)]
    agents: Vec<Record>,
}

/// The agents in `path`, in the order saved. A missing or damaged file is no agents at all, never an error (like the
/// read positions); `create` is the one that refuses to write over a file it cannot read, so nothing is lost silently.
pub fn load(path: &Path) -> Vec<Agent> {
    read(path).map(|f| f.agents.iter().map(Record::agent).collect()).unwrap_or_default()
}

fn read(path: &Path) -> Result<File, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
        Ok(text) => serde_json::from_str(&text).map_err(|_| format!("{} is not readable JSON; move it aside to start again", path.display())),
    }
}

/// Make a new agent: a fresh key, a folder of its own, and a record appended to `agents.json`. Returns what the panel
/// shows; the key goes to that file and nowhere else.
pub fn create(paths: &Paths, name: &str, instructions: &str) -> Result<Agent, String> {
    let name = clean_name(name)?;
    let instructions = clean_instructions(instructions);
    let mut file = read(&paths.agents())?;
    if file.agents.iter().any(|r| r.name.eq_ignore_ascii_case(&name)) {
        return Err(format!("an agent called {name} already exists"));
    }
    let workspace = workspace_for(paths, &name, &file.agents);
    let keys = Keys::generate();
    let record = Record {
        name,
        pubkey: keys.public_key().to_hex(),
        nsec: keys.secret_key().to_bech32().map_err(|e| e.to_string())?,
        harness: HARNESS.to_owned(),
        workspace: workspace.display().to_string(),
        instructions,
        created_at: now(),
    };
    let agent = record.agent();
    // the folder first: a record that names a folder we could not make would be worse than an empty folder
    make_private_dir(&workspace)?;
    file.agents.push(record);
    let doc = Zeroizing::new(serde_json::to_string_pretty(&file).expect("a json value serialises") + "\n");
    config::write_private(&paths.agents(), &doc).map_err(|e| format!("could not write {}: {e}", paths.agents().display()))?;
    Ok(agent)
}

/// A name with its whitespace tidied: what is shown and what the agent is called. Public so the two-step flow can
/// refuse a name the moment it is typed, rather than after the instructions as well.
pub fn clean_name(name: &str) -> Result<String, String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err("an agent needs a name".into());
    }
    if name.chars().count() > NAME_MAX {
        return Err(format!("a name can be at most {NAME_MAX} characters"));
    }
    if name.chars().any(char::is_control) {
        return Err("a name cannot contain control characters".into());
    }
    Ok(name)
}

/// Instructions come from the person and are edited by hand in the file, so control characters go and the length is
/// capped before they are stored (they are shown as plain text, never as terminal output).
fn clean_instructions(text: &str) -> String {
    let text = crate::markdown::strip_controls(text);
    let text = text.trim();
    text.chars().take(INSTRUCTIONS_MAX).collect()
}

/// `<config dir>/agents/<slug>`, with `-2`, `-3`… when that folder (or another agent's) is already there.
fn workspace_for(paths: &Paths, name: &str, existing: &[Record]) -> PathBuf {
    let base = paths.dir().join("agents");
    let slug = slug_of(name);
    let taken = |dir: &Path| dir.exists() || existing.iter().any(|r| Path::new(&r.workspace) == dir);
    let mut dir = base.join(&slug);
    let mut n = 2;
    while taken(&dir) {
        dir = base.join(format!("{slug}-{n}"));
        n += 1;
    }
    dir
}

/// A folder name taken from the agent's name: lower case, letters and digits, one dash between words.
fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').chars().take(32).collect::<String>();
    if slug.is_empty() {
        "agent".to_owned()
    } else {
        slug
    }
}

/// A folder only the owner can look into.
fn make_private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("could not protect {}: {e}", dir.display()))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_paths() -> Paths {
        Paths::new(std::env::temp_dir().join(format!("buzzr-agents-{}", uuid::Uuid::new_v4())))
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn an_agent_gets_its_own_key_folder_and_private_record() {
        let paths = temp_paths();
        let a = create(&paths, "  Scout  ", "watch the repo").unwrap();
        assert_eq!(a.name, "Scout", "the name is tidied");
        assert_eq!(a.harness, HARNESS);
        assert_eq!(a.pubkey.len(), 64, "a hex public key");
        assert!(a.workspace.ends_with("/agents/scout"), "{}", a.workspace);
        assert!(a.instructions.contains("watch the repo"));
        assert!(Path::new(&a.workspace).is_dir(), "the folder is made");
        assert_eq!(mode(Path::new(&a.workspace)), 0o700, "the folder is private");
        assert_eq!(mode(&paths.agents()), 0o600, "the record file is private");
        assert_eq!(load(&paths.agents()), vec![a.clone()], "and reads back");
        // the key in the file is this agent's: it parses, and belongs to the recorded public key
        let text = std::fs::read_to_string(paths.agents()).unwrap();
        assert!(text.contains("nsec1"), "the key is kept in the file");
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let nsec = doc["agents"][0]["nsec"].as_str().unwrap();
        assert_eq!(Keys::parse(nsec).unwrap().public_key().to_hex(), a.pubkey);
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn two_agents_never_share_a_key_folder_or_name() {
        let paths = temp_paths();
        let a = create(&paths, "Scout", "").unwrap();
        let b = create(&paths, "Scout!", "").unwrap(); // a different name that wants the same folder
        assert_ne!(a.pubkey, b.pubkey);
        assert_eq!(b.workspace, a.workspace.replace("scout", "scout-2"), "a taken folder gets a suffix");
        assert!(create(&paths, "Scout", "").unwrap_err().contains("already exists"));
        assert_eq!(load(&paths.agents()).len(), 2);
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn a_name_that_cannot_be_shown_is_refused() {
        let paths = temp_paths();
        for bad in ["", "   ", "\u{1b}[31mred", &"x".repeat(NAME_MAX + 1)] {
            let err = create(&paths, bad, "").unwrap_err();
            assert!(!err.is_empty(), "{bad:?} must be refused");
        }
        assert!(!paths.agents().exists(), "nothing was written");
        let _ = std::fs::remove_dir_all(paths.dir());
    }

    #[test]
    fn a_damaged_record_file_is_no_agents_but_is_not_written_over() {
        let paths = temp_paths();
        std::fs::create_dir_all(paths.dir()).unwrap();
        std::fs::write(paths.agents(), "not json").unwrap();
        assert!(load(&paths.agents()).is_empty(), "a damaged file is no agents, not a panic");
        let err = create(&paths, "Scout", "").unwrap_err();
        assert!(err.contains("not readable JSON"), "{err}");
        assert_eq!(std::fs::read_to_string(paths.agents()).unwrap(), "not json", "the file is left alone");
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }
}
