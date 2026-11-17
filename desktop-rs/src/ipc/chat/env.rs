//! The environment a chat's CLI resolves against (`envReady` in `src/main/app.ts`): the
//! login-shell PATH that `crate::sys::env` probes once at startup. Until it answers, the
//! inherited PATH plus the common bin dirs (~/.local/bin, …), so a chat restored at startup
//! still finds its CLI when Wraithgrid was started from a launcher.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::sys::env as sys_env;

/// Waits for the login shell's PATH, but never longer than `max`.
pub async fn ready(max: Duration) {
    let until = Instant::now() + max;
    while !sys_env::probed() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// This process's environment with the best PATH known now.
pub fn env_now() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.insert(
        if cfg!(windows) { "Path" } else { "PATH" }.into(),
        sys_env::path_now(),
    );
    env
}
