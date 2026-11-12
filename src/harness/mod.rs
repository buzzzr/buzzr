//! Harnesses: the programs that run an agent, spoken to over ACP.
//!
//! An agent's identity is buzzr's to make (see `agents.rs`), but the program that will speak for it is not: Claude Code
//! today, others later. Each one is a **harness** — a program that speaks the Agent Client Protocol (ACP) on its
//! standard input and output — and everything that differs between them is one entry in the table below, so a new one
//! (codex, say) is a new entry rather than a change to the flow that makes an agent.
//!
//! `acp` is the client: just enough to start a harness, ask it to open a session and read the models and thinking
//! levels it offers. That is what `/agents create` shows before it makes an agent, and it is also the "does this
//! harness work?" check — a harness that will not start, or will not answer ACP, is an error and nothing is written.

pub mod acp;

use crate::plugin::HarnessOffer;

/// One supported harness: the program buzzr runs for an agent, and the agent type the owner's listing names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Harness {
    /// The id — which is also the program to run, looked up on `PATH`: it speaks ACP on its standard input and output
    /// (`claude-agent-acp`). An agent's record and the panel name the harness by this.
    pub id: &'static str,
    /// A human name, for the panel (`Claude Code`).
    pub label: &'static str,
    /// The agent type the owner's listing names, `pluginId/typeId` (`buzz.claude-code/claude-code`): a Buzz client
    /// reads a listing's `runtime` tag to resolve the type, and one it cannot resolve does not show the listing.
    pub runtime: &'static str,
    /// What to say when the program cannot be started at all — it is not installed.
    pub missing: &'static str,
}

impl Harness {
    /// The program to run: the id names it.
    pub fn program(&self) -> &'static str {
        self.id
    }
}

/// Claude Code, driven over ACP by the `claude-agent-acp` adapter.
pub const CLAUDE_CODE: Harness = Harness {
    id: "claude-agent-acp",
    label: "Claude Code",
    runtime: "buzz.claude-code/claude-code",
    missing: "claude-agent-acp is not installed, or is not on PATH",
};

/// Every harness buzzr knows, in the order a menu would show them. A new one — codex, say — is a new entry here, and
/// nothing else changes: the flow asks a harness by id, and the panel lists it.
pub const ALL: &[Harness] = &[CLAUDE_CODE];

/// The harness a new agent is made for unless the flow names another.
pub const DEFAULT: &Harness = &CLAUDE_CODE;

/// The harness with this id, or `None` for one this build does not know.
pub fn get(id: &str) -> Option<&'static Harness> {
    ALL.iter().find(|h| h.id == id)
}

/// Ask a harness what it offers for a new agent: the models it lists and, when `model` is given, that model's thinking
/// levels (the session is put on `model` first, so the levels that come back are that model's). The harness is started
/// in a throwaway folder, asked to `initialize` and to open a session, and stopped before this returns — nothing it
/// writes is kept. A harness that will not start, or will not answer ACP, is an `Err`.
///
/// This blocks — a harness that never answers is waited on for `acp::TIMEOUT` — so call it from a task, never from the
/// event loop (principle 6 in `AGENTS.md`).
pub fn offer(harness: &Harness, model: Option<&str>) -> Result<HarnessOffer, String> {
    let dir = std::env::temp_dir().join(format!("buzzr-harness-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make a folder to start the harness in: {e}"))?;
    let result = acp::probe(harness.program(), &dir, model, harness.missing);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_harness_is_in_the_table_and_named_by_its_program() {
        assert_eq!(get(DEFAULT.id), Some(DEFAULT));
        assert_eq!(DEFAULT.program(), DEFAULT.id, "the id names the program to run");
        assert_eq!(get("claude-agent-acp"), Some(&CLAUDE_CODE));
        assert!(ALL.contains(&CLAUDE_CODE));
        assert_eq!(get("nope"), None, "an unknown harness is not a harness");
    }
}
