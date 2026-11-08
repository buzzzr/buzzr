//! Plugins: separate processes that add commands, status items, badges and panels to buzzr.
//!
//! A plugin is a folder with a `plugin.json`; the host starts it, speaks newline-delimited JSON with it, and owns everything
//! it draws. See `ARCHITECTURE.md` for the contract and `PLUGIN.md` for writing one.

pub mod host;
pub mod manifest;
pub mod proto;

pub use host::Host;
