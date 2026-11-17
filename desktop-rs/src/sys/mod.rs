//! Process-wide plumbing behind the terminal and system channels (ipc/pty.rs, misc.rs,
//! window.rs): the PATH panes resolve against, the pane hosts, sign-ins, the tray, native
//! dialogs and notifications. The Electron app kept these in `src/main/app.ts`.

pub mod args;
pub mod env;
pub mod login;
pub mod native;
pub mod notify;
pub mod panes;
pub mod tray;
