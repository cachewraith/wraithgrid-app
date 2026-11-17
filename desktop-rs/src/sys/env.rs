//! The PATH everything this app starts resolves against (`envReady` in app.ts). A launcher-
//! started session often lacks the PATH a login shell sets up, so claude and the tools it runs
//! would not be found: at startup the login shell is asked once, off the main thread
//! (`resolve_process_path`). Spawns wait for that answer (bounded), so a pane restored at
//! launch never resolves its CLI against the launcher's thin PATH (the 2.0 "claude was not
//! found on PATH" bug). This process's own environment is never changed.

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use wraithgrid_core::pane_env::Os;
use wraithgrid_core::paths::home_dir;
use wraithgrid_core::platform::{extra_bin_dirs, merge_path, resolve_process_path};

static PATH: (Mutex<Option<String>>, Condvar) = (Mutex::new(None), Condvar::new());

/// The probe asks the login shell (4 s), then `/bin/sh -l` (4 s): never longer than this.
const PROBE_WAIT: Duration = Duration::from_secs(10);

const PATH_KEY: &str = if cfg!(windows) { "Path" } else { "PATH" };

/// Starts the login-shell probe; call first thing at startup.
pub fn start_probe() {
    std::thread::Builder::new()
        .name("path-probe".into())
        .spawn(|| {
            let path = resolve_process_path(Os::current(), &home_dir().to_string_lossy());
            *PATH.0.lock().unwrap() = Some(path);
            PATH.1.notify_all();
        })
        .expect("path probe thread");
}

/// The inherited PATH plus the common bin dirs (~/.local/bin, ~/.bun/bin, …).
fn fallback_path() -> String {
    let os = Os::current();
    let home = home_dir().to_string_lossy().into_owned();
    let extra =
        extra_bin_dirs(os, &home, |k| std::env::var(k).ok()).join(&os.path_delimiter().to_string());
    let inherited = std::env::var(PATH_KEY).ok();
    merge_path(os.path_delimiter(), &[inherited.as_deref(), Some(&extra)])
}

/// The resolved PATH. Blocks until the probe answered (at most PROBE_WAIT); call off the
/// main thread and the async runtime's workers (spawn_blocking).
pub fn process_path() -> String {
    let guard = PATH.0.lock().unwrap();
    let (guard, _) = PATH
        .1
        .wait_timeout_while(guard, PROBE_WAIT, |p| p.is_none())
        .unwrap();
    guard.clone().unwrap_or_else(fallback_path)
}

/// The best PATH known now, without waiting: the probe's answer, else the fallback.
pub fn path_now() -> String {
    PATH.0.lock().unwrap().clone().unwrap_or_else(fallback_path)
}

/// Has the probe answered yet?
pub fn probed() -> bool {
    PATH.0.lock().unwrap().is_some()
}

/// This process's environment with the resolved PATH (`process.env` after envReady).
pub fn spawn_env() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.insert(PATH_KEY.into(), process_path());
    env
}
