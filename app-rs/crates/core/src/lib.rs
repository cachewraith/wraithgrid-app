//! Wraithgrid's core: everything the app does that isn't drawing (#166, ADR 0005). Ported
//! from the Electron main process and `src/shared`, so both apps read the same files and
//! talk to the same background pane host.

pub mod accounts;
pub mod agent_cli;
pub mod args;
pub mod cli;
pub mod config;
pub mod folders;
pub mod host;
pub mod ids;
pub mod layout;
pub mod login;
pub mod pane_env;
pub mod paths;
pub mod platform;
pub mod preview;
pub mod release_notes;
pub mod shortcuts;
pub mod spawn;
pub mod store;
pub mod types;
pub mod update;
