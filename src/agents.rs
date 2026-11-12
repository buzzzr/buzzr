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
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// The longest name accepted, so a name always fits on one line.
const NAME_MAX: usize = 64;

/// The longest instruction text kept, so a hand-edited file cannot grow the config without bound.
const INSTRUCTIONS_MAX: usize = 4096;

/// The longest model id or thinking level kept (they are short ids, not prose).
const CHOICE_MAX: usize = 64;

/// One local agent: everything the panel shows. The private key it speaks with is not here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    /// The identity's public key (hex).
    pub pubkey: String,
    /// What will run it: a harness id, the program to run (`claude-agent-acp`).
    pub harness: String,
    /// The model the harness is asked to run, when one was chosen (`None` = the harness's own default).
    pub model: Option<String>,
    /// The thinking level the harness is asked to run at, when one was chosen (`None` = its default).
    pub effort: Option<String>,
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
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
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
            model: self.model.clone(),
            effort: self.effort.clone(),
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

/// The agents in `path` with their keys: what [`load`] returns, plus the identity each one speaks with. The runner
/// (`runner.rs`) is the caller — an agent that answers a mention signs as itself, so it needs its own key. A record
/// whose key does not parse is left out rather than reported: a hand-edited file must not stop the others from
/// running, and a damaged one is already [`load`]'s to ignore.
pub fn load_with_keys(path: &Path) -> Vec<(Agent, Keys)> {
    read(path)
        .map(|f| {
            f.agents
                .iter()
                .filter_map(|r| Keys::parse(&r.nsec).ok().map(|k| (r.agent(), k)))
                .collect()
        })
        .unwrap_or_default()
}

fn read(path: &Path) -> Result<File, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
        Ok(text) => serde_json::from_str(&text).map_err(|_| format!("{} is not readable JSON; move it aside to start again", path.display())),
    }
}

/// An agent made but not yet written down: its key is in memory and its folder is not made.
///
/// Publishing happens between [`prepare`] and [`commit`], so a relay that does not take the agent leaves nothing
/// behind — no key, no folder and no record. This is the order `/agents create` takes; `create` is the two steps in one
/// for a caller that does not publish.
#[derive(Debug)]
pub struct Pending {
    agent: Agent,
    keys: Keys,
}

impl Pending {
    /// The agent as the panel will show it.
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    /// The agent's own key, for signing its own events (its kind-0 profile). Kept in memory, and written to the
    /// record only by [`commit`].
    pub fn keys(&self) -> &Keys {
        &self.keys
    }
}

/// Check a new agent and make its key, without writing anything: the name, the folder it will get, the harness it will
/// run on, and the identity it will speak with. [`commit`] writes it down once the relay has taken it.
///
/// `harness` is an id from the harness table (`harness.rs`); `model` and `effort` are what the harness offered and the
/// user picked, either of which may be empty for the harness's own choice.
pub fn prepare(paths: &Paths, name: &str, instructions: &str, harness: &str, model: &str, effort: &str) -> Result<Pending, String> {
    let name = clean_name(name)?;
    let instructions = clean_instructions(instructions);
    let harness = crate::harness::get(harness).ok_or_else(|| format!("{harness} is not a harness buzzr knows"))?;
    let file = read(&paths.agents())?;
    if file.agents.iter().any(|r| r.name.eq_ignore_ascii_case(&name)) {
        return Err(format!("an agent called {name} already exists"));
    }
    let workspace = workspace_for(paths, &name, &file.agents);
    let keys = Keys::generate();
    let agent = Agent {
        name,
        pubkey: keys.public_key().to_hex(),
        harness: harness.id.to_owned(),
        model: clean_choice(model),
        effort: clean_choice(effort),
        workspace: workspace.display().to_string(),
        instructions,
        created_at: now(),
    };
    Ok(Pending { agent, keys })
}

/// Write a prepared agent down: a folder of its own, and its record (with its key) appended to `agents.json`. Returns
/// what the panel shows; the key goes to that file and nowhere else.
pub fn commit(paths: &Paths, pending: &Pending) -> Result<Agent, String> {
    let agent = &pending.agent;
    let mut file = read(&paths.agents())?;
    if file.agents.iter().any(|r| r.name.eq_ignore_ascii_case(&agent.name)) {
        return Err(format!("an agent called {} already exists", agent.name));
    }
    let record = Record {
        name: agent.name.clone(),
        pubkey: agent.pubkey.clone(),
        nsec: pending.keys.secret_key().to_bech32().map_err(|e| e.to_string())?,
        harness: agent.harness.clone(),
        model: agent.model.clone(),
        effort: agent.effort.clone(),
        workspace: agent.workspace.clone(),
        instructions: agent.instructions.clone(),
        created_at: agent.created_at,
    };
    // the folder first: a record that names a folder we could not make would be worse than an empty folder
    make_private_dir(Path::new(&agent.workspace))?;
    file.agents.push(record);
    let doc = Zeroizing::new(serde_json::to_string_pretty(&file).expect("a json value serialises") + "\n");
    config::write_private(&paths.agents(), &doc).map_err(|e| format!("could not write {}: {e}", paths.agents().display()))?;
    Ok(agent.clone())
}

/// Make a new agent in one step, on the default harness and its own choice of model: a fresh key, a folder of its own,
/// and a record appended to `agents.json`. Returns what the panel shows; the key goes to that file and nowhere else. A
/// caller that publishes the agent uses [`prepare`] and [`commit`] instead, so a relay that refuses it leaves nothing
/// behind; the flow that picks a harness, model and effort uses those too.
pub fn create(paths: &Paths, name: &str, instructions: &str) -> Result<Agent, String> {
    let pending = prepare(paths, name, instructions, crate::harness::DEFAULT.id, "", "")?;
    commit(paths, &pending)
}

/// Delete an agent's record — its identity, and the only copy of its key — from `agents.json`, returning it as it was.
///
/// The folder the agent worked in is left alone: deleting an agent in Buzz deletes its key from the device and leaves
/// its working directory (it may hold the agent's own work). A pubkey that is not in the file is refused, and a file
/// that cannot be read is not written over, so nothing else in it is lost.
pub fn remove(paths: &Paths, pubkey: &str) -> Result<Agent, String> {
    let mut file = read(&paths.agents())?;
    let Some(at) = file.agents.iter().position(|r| r.pubkey == pubkey) else {
        return Err(format!("{pubkey} is not an agent on this device"));
    };
    let gone = file.agents.remove(at).agent();
    let doc = Zeroizing::new(serde_json::to_string_pretty(&file).expect("a json value serialises") + "\n");
    config::write_private(&paths.agents(), &doc).map_err(|e| format!("could not write {}: {e}", paths.agents().display()))?;
    Ok(gone)
}

/// The agent's own key, read back from the record file — the only place it is kept. Used to sign the agent's own
/// events (its kind-0 profile), which are the agent's, not the person's.
pub fn keys_of(path: &Path, pubkey: &str) -> Result<Keys, String> {
    let file = read(path)?;
    let record = file.agents.iter().find(|r| r.pubkey == pubkey).ok_or_else(|| format!("{pubkey} is not in {}", path.display()))?;
    Keys::parse(&record.nsec).map_err(|e| format!("the saved key for {} is not usable: {e}", record.name))
}

/// The owner's attestation for one agent, as the JSON an `auth` tag holds: `["auth", <owner>, <conditions>, <sig>]`.
///
/// The owner signs the SHA-256 of `nostr:agent-auth:<agent>:<conditions>`; empty conditions (the only kind made here)
/// mean no limit. A Buzz client reads this tag from the agent's own newest kind-0 profile to tell whose agent it is, so
/// it is what makes the agent's identity provably the owner's rather than a bare local record.
pub fn attestation(owner: &Keys, agent_pubkey: &str) -> Result<String, String> {
    let digest: [u8; 32] = Sha256::digest(format!("nostr:agent-auth:{agent_pubkey}:").as_bytes()).into();
    let message = nostr::secp256k1::Message::from_digest(digest);
    let signature = owner.sign_schnorr(&message);
    Ok(serde_json::to_string(&["auth", &owner.public_key().to_hex(), "", &signature.to_string()]).expect("a JSON array serialises"))
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

/// A model id or thinking level the harness offered: control characters go, the length is capped, and an empty one is
/// the harness's own choice (`None`), so an unset field and an empty string are the same thing.
fn clean_choice(text: &str) -> Option<String> {
    let text = crate::markdown::strip_controls(text);
    let text: String = text.trim().chars().take(CHOICE_MAX).collect();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
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
        assert_eq!(a.harness, crate::harness::DEFAULT.id);
        assert_eq!(a.model, None, "the default harness's own choice of model is not pinned");
        assert_eq!(a.effort, None);
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
    fn a_prepared_agent_is_not_written_until_it_is_committed() {
        let paths = temp_paths();
        let pending = prepare(&paths, "  Scout  ", "watch the repo", crate::harness::DEFAULT.id, "sonnet", "high").unwrap();
        // nothing is on disk yet, so a relay that refuses the agent leaves nothing behind
        assert!(!paths.agents().exists(), "prepare writes no record");
        assert!(!Path::new(&pending.agent().workspace).exists(), "prepare makes no folder");
        assert_eq!(pending.agent().name, "Scout", "the name is tidied before anything is written");
        // commit is what writes it down: the folder, and the record with the key
        let a = commit(&paths, &pending).unwrap();
        assert_eq!(a.pubkey, pending.agent().pubkey, "the committed agent is the prepared one");
        assert_eq!(a.model.as_deref(), Some("sonnet"), "the chosen model is kept");
        assert_eq!(a.effort.as_deref(), Some("high"), "and the thinking level");
        assert!(Path::new(&a.workspace).is_dir(), "the folder is made");
        assert_eq!(mode(&paths.agents()), 0o600, "the record file is private");
        assert_eq!(load(&paths.agents()), vec![a], "and reads back");
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn an_agent_is_made_on_a_harness_buzzr_knows_and_keeps_the_choices_tidied() {
        let paths = temp_paths();
        // a harness this build does not know is refused before anything is made
        let err = prepare(&paths, "Scout", "", "not-a-harness", "", "").unwrap_err();
        assert!(err.contains("not a harness"), "{err}");
        assert!(!paths.agents().exists(), "nothing was written");
        // model and effort are trimmed, and an empty one is the harness's own choice, not an empty string
        let pending = prepare(&paths, "Scout", "", crate::harness::DEFAULT.id, "  opus ", "   ").unwrap();
        assert_eq!(pending.agent().model.as_deref(), Some("opus"));
        assert_eq!(pending.agent().effort, None, "a blank level is no choice at all");
        let _ = std::fs::remove_dir_all(paths.dir()); // `prepare` wrote nothing, so there may be no directory to remove
    }

    #[test]
    fn a_record_written_before_agents_had_a_model_still_reads_back() {
        let paths = temp_paths();
        std::fs::create_dir_all(paths.dir()).unwrap();
        // an older build wrote no `model`/`effort`; the record must still load, with no choice pinned
        std::fs::write(
            paths.agents(),
            r#"{"agents":[{"name":"Scout","pubkey":"aa","nsec":"x","harness":"claude-agent-acp","workspace":"/tmp/x","instructions":"","created_at":1}]}"#,
        )
        .unwrap();
        let agents = load(&paths.agents());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].model, None);
        assert_eq!(agents[0].effort, None);
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
    fn removing_an_agent_drops_its_record_and_keeps_the_others() {
        let paths = temp_paths();
        let a = create(&paths, "Scout", "watch the repo").unwrap();
        let b = create(&paths, "Ranger", "watch the other repo").unwrap();
        // the record that was removed is returned as it was, and it is gone from the file
        let gone = remove(&paths, &a.pubkey).unwrap();
        assert_eq!(gone, a, "the agent that was removed is returned");
        assert_eq!(load(&paths.agents()), vec![b.clone()], "the other agent is left alone");
        assert_eq!(mode(&paths.agents()), 0o600, "the record file stays private");
        // the folder is left alone: it may hold the agent's own work
        assert!(Path::new(&gone.workspace).is_dir(), "its working directory is kept");
        // a pubkey that is not an agent here is refused, and the file is not touched
        let err = remove(&paths, &"00".repeat(32)).unwrap_err();
        assert!(err.contains("not an agent"), "{err}");
        assert_eq!(load(&paths.agents()), vec![b], "nothing else was dropped");
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn removing_from_a_damaged_record_file_writes_nothing() {
        let paths = temp_paths();
        std::fs::create_dir_all(paths.dir()).unwrap();
        std::fs::write(paths.agents(), "not json").unwrap();
        let err = remove(&paths, &"aa".repeat(32)).unwrap_err();
        assert!(err.contains("not readable JSON"), "{err}");
        assert_eq!(std::fs::read_to_string(paths.agents()).unwrap(), "not json", "the file is left alone");
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

    #[test]
    fn the_attestation_is_the_owners_signature_over_this_agent_only() {
        use nostr::secp256k1::{schnorr::Signature, Message, Secp256k1, XOnlyPublicKey};
        use std::str::FromStr;
        let owner = Keys::parse(&"11".repeat(32)).unwrap();
        let agent = Keys::generate();
        let tag: Vec<String> = serde_json::from_str(&attestation(&owner, &agent.public_key().to_hex()).unwrap()).unwrap();
        assert_eq!(tag.len(), 4, "an auth tag is exactly [auth, owner, conditions, sig]");
        assert_eq!(tag[0], "auth");
        assert_eq!(tag[1], owner.public_key().to_hex());
        assert_eq!(tag[2], "", "no conditions: the grant has no limits");
        let verify = |agent: &Keys| {
            let digest: [u8; 32] = Sha256::digest(format!("nostr:agent-auth:{}:", agent.public_key().to_hex()).as_bytes()).into();
            Secp256k1::verification_only()
                .verify_schnorr(&Signature::from_str(&tag[3]).unwrap(), &Message::from_digest(digest), &XOnlyPublicKey::from_str(&tag[1]).unwrap())
        };
        assert!(verify(&agent).is_ok(), "the owner signed this agent");
        assert!(verify(&Keys::generate()).is_err(), "and only this agent");
    }

    #[test]
    fn an_agents_key_is_read_back_so_it_can_sign_its_own_events() {
        let paths = temp_paths();
        let a = create(&paths, "Scout", "").unwrap();
        assert_eq!(keys_of(&paths.agents(), &a.pubkey).unwrap().public_key().to_hex(), a.pubkey, "the saved key is the agent's");
        assert!(keys_of(&paths.agents(), &"00".repeat(32)).is_err(), "an agent that is not in the file has no key");
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }
}
