//! The process-wide plumbing the window needs: the PATH panes resolve against, the pane
//! host connection, and the router that hands host output to each pane's terminal on the
//! host's reader thread (no UI-thread hop, so a busy pane never queues frames).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use wraithgrid_core::host::client::{
    connect_socket, HostEvent, MismatchChoice, PaneHost, PaneHostDeps,
};
use wraithgrid_core::host::paths::{current_host_paths, HostPaths};
use wraithgrid_core::host::start::{host_binary, start_host_process};
use wraithgrid_core::pane_env::Os;
use wraithgrid_core::paths::home_dir;
use wraithgrid_core::platform::resolve_process_path;

use crate::terminal::TermCore;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

static PROCESS_PATH: OnceLock<String> = OnceLock::new();

/// Asks the login shell for PATH off the UI thread (it can take seconds); spawns wait on it.
pub fn resolve_path_in_background() {
    std::thread::spawn(|| {
        let path = resolve_process_path(Os::current(), &home_dir().to_string_lossy());
        let _ = PROCESS_PATH.set(path);
    });
}

/// This process's environment with the resolved PATH, for spawn resolution.
pub fn spawn_env() -> BTreeMap<String, String> {
    let path = PROCESS_PATH.wait().clone();
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.insert(if cfg!(windows) { "Path" } else { "PATH" }.into(), path);
    restore_gdk_backend(&mut env);
    env
}

/// The session's `GDK_BACKEND` before the app forced X11 for the preview's webview (Linux);
/// what the app starts gets it back.
pub static USER_GDK_BACKEND: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// WebKitGTK embeds only through X11, and a `GDK_BACKEND=wayland` in the session would break
/// it (ADR 0005): the app runs GTK on X11 and keeps the session's value for its children.
pub fn force_gdk_x11() {
    if cfg!(target_os = "linux") {
        USER_GDK_BACKEND.get_or_init(|| std::env::var("GDK_BACKEND").ok());
        // SAFETY: called first thing in main, before any thread starts.
        unsafe { std::env::set_var("GDK_BACKEND", "x11") };
    }
}

fn restore_gdk_backend(env: &mut BTreeMap<String, String>) {
    match USER_GDK_BACKEND.get() {
        Some(Some(v)) => {
            env.insert("GDK_BACKEND".into(), v.clone());
        }
        Some(None) => {
            env.remove("GDK_BACKEND");
        }
        None => {}
    }
}

/// The same, without waiting for the login shell's answer: the inherited PATH until it came.
pub fn spawn_env_now() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    // Before the login-shell probe answers, a launcher's PATH often lacks ~/.local/bin and
    // friends: add the common bin dirs so a pane restored at startup still finds its CLI.
    let path = PROCESS_PATH.get().cloned().unwrap_or_else(|| {
        let os = Os::current();
        let home = home_dir().to_string_lossy().into_owned();
        let extra = wraithgrid_core::platform::extra_bin_dirs(os, &home, |k| std::env::var(k).ok())
            .join(&os.path_delimiter().to_string());
        let inherited = std::env::var("PATH").ok();
        wraithgrid_core::platform::merge_path(
            os.path_delimiter(),
            &[inherited.as_deref(), Some(&extra)],
        )
    });
    env.insert(if cfg!(windows) { "Path" } else { "PATH" }.into(), path);
    restore_gdk_backend(&mut env);
    env
}

/// Hands each pane's output to its terminal. Output for a pane whose view isn't up yet is
/// kept until it registers (a reattach's screen arrives ahead of the attach reply).
#[derive(Default)]
pub struct Router {
    cores: Mutex<HashMap<String, Arc<TermCore>>>,
    pending: Mutex<HashMap<String, Vec<HostEvent>>>,
    /// `wraithgrid new|split|send` commands the host relays to the window.
    gui: Mutex<Option<futures::channel::mpsc::UnboundedSender<(u64, serde_json::Value)>>>,
}

impl Router {
    pub fn register(&self, pane_id: &str, core: Arc<TermCore>) {
        self.cores
            .lock()
            .unwrap()
            .insert(pane_id.into(), core.clone());
        for event in self
            .pending
            .lock()
            .unwrap()
            .remove(pane_id)
            .unwrap_or_default()
        {
            deliver(&core, event);
        }
    }

    /// Where relayed commands go; the window answers each with `PaneHost::gui_reply`.
    pub fn gui_commands(
        &self,
    ) -> futures::channel::mpsc::UnboundedReceiver<(u64, serde_json::Value)> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        *self.gui.lock().unwrap() = Some(tx);
        rx
    }

    pub fn unregister(&self, pane_id: &str) {
        self.cores.lock().unwrap().remove(pane_id);
        self.pending.lock().unwrap().remove(pane_id);
    }

    pub fn event(&self, event: HostEvent) {
        let pane_id = match &event {
            HostEvent::Data { pane_id, .. } | HostEvent::Exit { pane_id, .. } => pane_id.clone(),
            HostEvent::Gui { id, action } => {
                if let Some(tx) = self.gui.lock().unwrap().as_ref() {
                    let _ = tx.unbounded_send((*id, action.clone()));
                }
                return;
            }
        };
        let core = self.cores.lock().unwrap().get(&pane_id).cloned();
        match core {
            Some(core) => deliver(&core, event),
            None => self
                .pending
                .lock()
                .unwrap()
                .entry(pane_id)
                .or_default()
                .push(event),
        }
    }
}

fn deliver(core: &TermCore, event: HostEvent) {
    match event {
        HostEvent::Data { data, .. } => core.feed(&data),
        HostEvent::Exit {
            exit_code,
            signal,
            error,
            ..
        } => core.end(match (error, signal) {
            (Some(error), _) => error,
            (None, Some(signal)) if signal != 0 => format!("Stopped by signal {signal}."),
            (None, _) => format!("Exited with code {exit_code}."),
        }),
        HostEvent::Gui { .. } => {}
    }
}

pub fn pane_host(user_data: &Path, router: Arc<Router>) -> (Arc<PaneHost>, HostPaths) {
    let paths = current_host_paths(user_data);
    let exe = std::env::current_exe().unwrap_or_default();
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".cache"))
        .join("wraithgrid");
    let appdir = std::env::var("APPDIR").ok();
    let bin: Option<PathBuf> = host_binary(&exe, |k| std::env::var(k).ok()).map(|b| {
        wraithgrid_core::host::start::stable_host_binary(&b, appdir.as_deref(), &cache, APP_VERSION)
    });
    let (connect_paths, start_paths, token_paths) = (paths.clone(), paths.clone(), paths.clone());
    let host = PaneHost::new(PaneHostDeps {
        connect: Box::new(move || connect_socket(&connect_paths.socket)),
        start: Box::new(move || {
            start_host_process(&start_paths, APP_VERSION, bin.as_deref(), &home_dir())
        }),
        read_token: Box::new(move || std::fs::read_to_string(&token_paths.token)),
        app_version: APP_VERSION.into(),
        on_event: Arc::new(move |e| router.event(e)),
        // Only an older host with live agents asks; keep them until a dialog exists.
        on_mismatch: Box::new(|_, _| MismatchChoice::Keep),
        start_timeout: Duration::from_secs(10),
        hello_timeout: Duration::from_secs(5),
    });
    (host, paths)
}

/// The Electron app's pid when it runs on the same user-data dir: Chromium keeps a
/// `SingletonLock` symlink to `<host>-<pid>` there. Both apps write config.json, so the new
/// one must not run beside it.
pub fn electron_running(user_data: &Path) -> Option<u32> {
    let target = std::fs::read_link(user_data.join("SingletonLock")).ok()?;
    let pid: u32 = target.to_string_lossy().rsplit('-').next()?.parse().ok()?;
    #[cfg(unix)]
    {
        // kill(pid, 0): does the process exist (EPERM also means it does).
        // SAFETY: signal 0 only checks that the process exists.
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        alive.then_some(pid)
    }
    #[cfg(not(unix))]
    Some(pid)
}

/// Who this launch is (`requestSingleInstanceLock`): the app for this user-data dir, or a
/// second launch that asked the running one to show its window and should exit.
pub enum Instance {
    First(Option<std::sync::mpsc::Receiver<()>>),
    /// Only unix has the socket so far; elsewhere every launch is the first.
    #[cfg_attr(not(unix), allow(dead_code))]
    Second,
}

/// One app per user-data dir: a second launch connects to the first one's socket and asks it
/// to show its window (`second-instance`); the first gets a message per such launch.
#[cfg(unix)]
pub fn single_instance(user_data: &Path) -> Instance {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    let path = user_data.join("wraithgrid-window.sock");
    if let Ok(mut s) = UnixStream::connect(&path) {
        if s.write_all(b"open\n").is_ok() {
            return Instance::Second;
        }
    }
    // Left by an app that ended without cleaning up.
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else {
        return Instance::First(None);
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_ok()
                && line.trim() == "open"
                && tx.send(()).is_err()
            {
                break;
            }
        }
    });
    Instance::First(Some(rx))
}

#[cfg(not(unix))]
pub fn single_instance(_user_data: &Path) -> Instance {
    Instance::First(None)
}
