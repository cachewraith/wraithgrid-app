//! The environment a chat's CLI resolves against (`envReady` in `src/main/app.ts`): PATH from
//! the login shell, asked once in the background (it can take seconds). Until it answers, the
//! inherited PATH plus the common bin dirs (~/.local/bin, …), so a chat restored at startup
//! still finds its CLI when Wraithgrid was started from a launcher.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use wraithgrid_core::pane_env::Os;
use wraithgrid_core::paths::home_dir;
use wraithgrid_core::platform::{extra_bin_dirs, merge_path, resolve_process_path};

static PROBED: OnceLock<String> = OnceLock::new();

/// Asks the login shell for PATH off the main thread.
pub fn start_probe() {
    std::thread::spawn(|| {
        let path = resolve_process_path(Os::current(), &home_dir().to_string_lossy());
        let _ = PROBED.set(path);
    });
}

/// Waits for the login shell's PATH, but never longer than `max`.
pub async fn ready(max: Duration) {
    let until = Instant::now() + max;
    while PROBED.get().is_none() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// This process's environment with the best PATH known now.
pub fn env_now() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    let path = PROBED.get().cloned().unwrap_or_else(|| {
        let os = Os::current();
        let home = home_dir().to_string_lossy().into_owned();
        let extra = extra_bin_dirs(os, &home, |k| std::env::var(k).ok())
            .join(&os.path_delimiter().to_string());
        let inherited = std::env::var("PATH").ok();
        merge_path(os.path_delimiter(), &[inherited.as_deref(), Some(&extra)])
    });
    env.insert(if cfg!(windows) { "Path" } else { "PATH" }.into(), path);
    env
}
