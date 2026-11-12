//! Plugins: separate processes that add commands, status items, badges, panes and panels to buzzr.
//!
//! A plugin is a folder with an executable entry file; the host starts it, speaks newline-delimited JSON with it, and
//! owns everything it draws. There is no manifest: the folder name is the id, the entry's shebang picks the interpreter,
//! and the plugin declares the rest in its own code when it says it is ready. See `ARCHITECTURE.md` for the contract and
//! `PLUGIN.md` for writing one.

pub mod discover;
pub mod host;
pub mod proto;

pub use host::Host;
