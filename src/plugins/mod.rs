//! Plugins: compiled-in (Rust, in this tree) and runtime-loaded (a process the host starts).
//!
//! A **compiled-in** plugin is a Rust type registered with `crate::register_plugin!` and driven in-process through
//! `crate::plugin` (see `agents.rs` here for the worked example). A **runtime-loaded** plugin is a folder with an
//! executable entry file; `host.rs` starts it, speaks newline-delimited JSON with it (see `proto.rs`), and owns
//! everything it draws. There is no manifest: the folder name is the id, the entry's shebang picks the interpreter, and
//! the plugin declares the rest in its own code when it says it is ready. See `ARCHITECTURE.md` for the contract and
//! `PLUGIN.md` for writing one.

pub mod agents;
pub mod discover;
pub mod host;
pub mod proto;

pub use host::Host;
