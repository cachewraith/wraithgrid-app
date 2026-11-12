//! Where the pty host listens and keeps its token (`src/host/paths.ts`). One host per
//! user-data dir, so a test run (WRAITHGRID_USER_DATA_DIR) never meets the user's real agents.
//! The same hash as the Electron app, so both find the same host.

use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::pane_env::Os;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HostPaths {
    /// Holds the socket, token, lock and log (POSIX); the token, lock and log on Windows.
    pub dir: PathBuf,
    /// Unix socket path, or a named pipe on Windows.
    pub socket: PathBuf,
    pub token: PathBuf,
    pub lock: PathBuf,
    pub log: PathBuf,
}

fn short_hash(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

pub struct HostPathInput<'a> {
    pub os: Os,
    pub xdg_runtime_dir: Option<&'a str>,
    pub user_data: &'a Path,
    pub tmp_dir: &'a Path,
    /// POSIX uid; -1 on Windows.
    pub uid: i64,
    pub user_name: &'a str,
}

pub fn host_paths(i: HostPathInput) -> HostPaths {
    let user_data = i.user_data.to_string_lossy();
    if i.os == Os::Windows {
        let dir = i.user_data.join("run");
        return HostPaths {
            socket: PathBuf::from(format!(
                "\\\\.\\pipe\\wraithgrid-host-{}",
                short_hash(&format!("{}\0{user_data}", i.user_name))
            )),
            token: dir.join("host.token"),
            lock: dir.join("host.lock"),
            log: dir.join("host.log"),
            dir,
        };
    }
    // XDG_RUNTIME_DIR is per-user and 0700 already; the temp dir is per-user on macOS. The
    // name stays short: Unix socket paths are limited to 104 bytes on macOS.
    let base = match i.xdg_runtime_dir.filter(|d| d.starts_with('/')) {
        Some(d) => PathBuf::from(d),
        None => i.tmp_dir.to_path_buf(),
    };
    let dir = base.join(format!("wraithgrid-{}-{}", i.uid, short_hash(&user_data)));
    HostPaths {
        socket: dir.join("host.sock"),
        token: dir.join("host.token"),
        lock: dir.join("host.lock"),
        log: dir.join("host.log"),
        dir,
    }
}

/// This process's host paths.
pub fn current_host_paths(user_data: &Path) -> HostPaths {
    #[cfg(unix)]
    let uid = unsafe { libc::getuid() } as i64;
    #[cfg(not(unix))]
    let uid = -1;
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    let xdg = std::env::var("XDG_RUNTIME_DIR").ok();
    // Node's os.tmpdir(): TMPDIR without a trailing slash, else /tmp.
    let tmp = std::env::temp_dir();
    let tmp = PathBuf::from(tmp.to_string_lossy().trim_end_matches('/'));
    host_paths(HostPathInput {
        os: Os::current(),
        xdg_runtime_dir: xdg.as_deref(),
        user_data,
        tmp_dir: &tmp,
        uid,
        user_name: &user,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_paths_as_the_ts() {
        let p = host_paths(HostPathInput {
            os: Os::Linux,
            xdg_runtime_dir: Some("/run/user/1000"),
            user_data: Path::new("/home/u/.config/wraithgrid"),
            tmp_dir: Path::new("/tmp"),
            uid: 1000,
            user_name: "u",
        });
        // sha256("/home/u/.config/wraithgrid") starts with this (checked against node:crypto).
        let hash = short_hash("/home/u/.config/wraithgrid");
        assert_eq!(hash.len(), 12);
        assert_eq!(
            p.socket,
            PathBuf::from(format!("/run/user/1000/wraithgrid-1000-{hash}/host.sock"))
        );
        let relative = host_paths(HostPathInput {
            xdg_runtime_dir: Some("relative"),
            ..HostPathInput {
                os: Os::Linux,
                xdg_runtime_dir: None,
                user_data: Path::new("/x"),
                tmp_dir: Path::new("/tmp"),
                uid: 1,
                user_name: "u",
            }
        });
        assert!(relative.dir.starts_with("/tmp"));
    }

    #[test]
    fn windows_pipe() {
        let p = host_paths(HostPathInput {
            os: Os::Windows,
            xdg_runtime_dir: None,
            user_data: Path::new("C:\\Users\\u\\AppData\\Roaming\\wraithgrid"),
            tmp_dir: Path::new("C:\\tmp"),
            uid: -1,
            user_name: "u",
        });
        assert!(p
            .socket
            .to_string_lossy()
            .starts_with("\\\\.\\pipe\\wraithgrid-host-"));
    }
}
