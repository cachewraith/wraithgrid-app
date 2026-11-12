//! Machines (H5): a project's panes run on another computer, reached with the system `ssh`.
//! One `RemoteHost` per machine, made on first use; a pane talks to the host of its project's
//! machine (`machineOfPane`), else to this computer's. Agents there keep running when the
//! window disconnects or quits.

use std::sync::Arc;

use gpui_kit::*;
use wraithgrid_core::config::Machine;
use wraithgrid_core::host::client::{MismatchChoice, PaneHost};
use wraithgrid_remote::{ssh_program, BinaryFor, Probe, RemoteHost, RemoteHostDeps, SystemSsh};

use super::Shell;
use crate::runtime::Router;

/// This build's static host for a machine's target (`remoteHostBinary`): `remote/` beside the
/// host binary in the app's resources.
fn remote_binary() -> Arc<BinaryFor> {
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = wraithgrid_core::host::start::host_binary(&exe, |k| std::env::var(k).ok())
        .and_then(|b| b.parent().map(|d| d.join("remote")));
    Arc::new(move |target: &str| {
        let dir = dir.as_ref()?;
        std::fs::read(dir.join(format!("wraithgrid-host-{target}"))).ok()
    })
}

/// A machine's host; its panes' output goes to the same router as this computer's.
pub fn remote_host(m: &Machine, router: Option<Arc<Router>>) -> RemoteHost {
    RemoteHost::new(RemoteHostDeps {
        machine: m.clone(),
        app_version: crate::runtime::APP_VERSION.into(),
        program: ssh_program(|k| std::env::var(k).ok()),
        ssh: Arc::new(SystemSsh),
        binary: remote_binary(),
        on_event: Arc::new(move |e| {
            if let Some(router) = &router {
                router.event(e)
            }
        }),
        // Only an older host with live agents asks; they keep running.
        on_mismatch: Box::new(|_, _| MismatchChoice::Keep),
        log: Arc::new(|m| eprintln!("[wraithgrid] machine {m}")),
    })
}

/// "Connected: Wraithgrid 2.0.0, 1 agent running", or why not (`machines.test`).
pub fn probe_text(r: Result<Probe, String>) -> String {
    match r {
        Ok(p) => format!(
            "Connected: Wraithgrid {}, {} agent{} running",
            if p.app_version.is_empty() {
                "?"
            } else {
                &p.app_version
            },
            p.live,
            if p.live == 1 { "" } else { "s" }
        ),
        Err(e) => e,
    }
}

impl Shell {
    /// The machine a pane's project runs on (`machineOfPane`); None: this computer.
    pub(super) fn machine_of_pane(&self, pane_id: &str) -> Option<Machine> {
        let c = self.config();
        let ws = c
            .workspaces
            .iter()
            .find(|w| w.panes.iter().any(|p| p.id == pane_id))?;
        let id = ws.machine_id.as_deref()?;
        c.machines.iter().find(|m| m.id == id).cloned()
    }

    /// The machine's host, made the first time (again when its ssh host changed).
    pub(super) fn remote_for(&mut self, m: &Machine) -> Arc<RemoteHost> {
        if let Some((host, r)) = self.remotes.get(&m.id) {
            if *host == m.ssh_host {
                return r.clone();
            }
        }
        let r = Arc::new(remote_host(m, Some(self.router.clone())));
        self.remotes
            .insert(m.id.clone(), (m.ssh_host.clone(), r.clone()));
        r
    }

    /// The host a pane lives in: its project's machine's, else this computer's.
    pub(super) fn host_for(&mut self, pane_id: &str) -> Arc<PaneHost> {
        match self.machine_of_pane(pane_id) {
            Some(m) => self.remote_for(&m).pane_host(),
            None => self.host.clone(),
        }
    }
}
