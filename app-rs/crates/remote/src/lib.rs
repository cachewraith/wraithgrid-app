//! Machines (H5): a project's panes run on another computer, reached with the system `ssh`
//! (#166 port of `src/main/ssh.ts`, `remote-plan.ts`, `remote-bin.ts`, `host-bridge.ts` and
//! `remoteHost()` in `app.ts`). The window side builds on core's host client: an ssh child's
//! stdout/stdin are the stream a `PaneHost` talks over. No UI here; everything blocks, so
//! callers run it off the UI thread.

pub mod bridge;
mod host;
pub mod plan;
pub mod remote_bin;
pub mod spec;
pub mod ssh;

pub use bridge::{run_bridge, BridgeDeps, Resolve};
pub use host::{lost_message, BinaryFor, Probe, RemoteHost, RemoteHostDeps, LOCAL_LOST};
pub use plan::{path_probe_plan, remote_plan, FallbackProbe, PathProbePlan, RemotePlan};
pub use remote_bin::{
    preflight_command, prepare_remote, remote_bin_path, remote_target, upload_command, PrepareDeps,
    RunOnMachine, UNUSED_DAYS,
};
pub use spec::{parse_spawn_spec, spawn_spec_json};
pub use ssh::{
    ssh_args, ssh_config_hosts, ssh_program, SshLink, SshRun, SshRunner, StderrTail, SystemSsh,
};
