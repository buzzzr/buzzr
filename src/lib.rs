//! buzzr, as a library.
//!
//! The modules the binary is built from, exposed so that a **compiled-in** plugin can live in a crate of its own: that
//! crate depends on `buzzr`, implements [`plugin::Plugin`], and registers it with [`register_plugin!`] — `inventory`
//! collects the submission into the same registry this crate's own plugins use (see `plugin.rs`). The binary in
//! `main.rs` is a thin shell over this crate: it parses the arguments, sets the terminal up, and drives the loop.
//!
//! Nothing here is a second copy of the program. `plugin::builtins` reads one registry, whoever registered into it.

pub mod agents;
pub mod app;
pub mod config;
pub mod contrib;
pub mod editor;
pub mod emoji;
pub mod keyfile;
pub mod list;
pub mod live;
pub mod markdown;
pub mod model;
pub mod plugin;
pub mod plugins;
pub mod relay;
pub mod release;
pub mod selftest;
pub mod setup;
pub mod theme;
pub mod ui;
pub mod update;
