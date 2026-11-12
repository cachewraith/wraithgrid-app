//! Starts the background pty host (`src/main/host-start.ts`) detached: the `wraithgrid-host`
//! binary of ADR 0004, unchanged, with the same `WRAITHGRID_HOST` settings. Instead of
//! masking inherited fds with /dev/null copies for the host to close (`maskedFds`), the child
//! marks every fd above stderr close-on-exec before it runs the host, so `maskedFds` is 0.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::paths::HostPaths;

pub const HOST_BIN: &str = if cfg!(windows) {
    "wraithgrid-host.exe"
} else {
    "wraithgrid-host"
};

/// Where the Windows and macOS packages keep the host, as the Electron ones do (its
/// `process.resourcesPath`): `resources\` beside `Wraithgrid.exe`, and `Contents/Resources`
/// of the app bundle. The Linux packages put it beside the binary.
fn resources_dir(exe_dir: &Path) -> Option<PathBuf> {
    if cfg!(windows) {
        Some(exe_dir.join("resources"))
    } else if cfg!(target_os = "macos") {
        exe_dir.parent().map(|contents| contents.join("Resources"))
    } else {
        None
    }
}

/// This build's host binary: `WRAITHGRID_HOST_BIN`, else beside the app's executable or in
/// its resources (the packaged layouts), else `host-rs/target/release` of a source checkout
/// above it (development).
pub fn host_binary(exe: &Path, env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(bin) = env("WRAITHGRID_HOST_BIN").filter(|b| !b.is_empty()) {
        return Some(PathBuf::from(bin));
    }
    let dir = exe.parent()?;
    let beside = dir.join(HOST_BIN);
    if beside.is_file() {
        return Some(beside);
    }
    if let Some(packaged) = resources_dir(dir)
        .map(|r| r.join(HOST_BIN))
        .filter(|p| p.is_file())
    {
        return Some(packaged);
    }
    dir.ancestors()
        .map(|a| {
            a.join("host-rs")
                .join("target")
                .join("release")
                .join(HOST_BIN)
        })
        .find(|p| p.is_file())
}

/// The host outlives the window, but an AppImage's files vanish with its mount when the app
/// quits. From an AppImage (`$APPDIR`), the host runs from a copy in the user's cache dir,
/// one per version; elsewhere the binary is used where it is.
pub fn stable_host_binary(
    bin: &Path,
    appdir: Option<&str>,
    cache: &Path,
    version: &str,
) -> PathBuf {
    let Some(appdir) = appdir.filter(|d| !d.is_empty()) else {
        return bin.to_path_buf();
    };
    if !bin.starts_with(appdir) {
        return bin.to_path_buf();
    }
    let copy = cache.join(format!("{HOST_BIN}-{version}"));
    if copy.is_file() {
        return copy;
    }
    let staged = cache.join(format!("{HOST_BIN}-{version}.{}", std::process::id()));
    let done = fs::create_dir_all(cache)
        .and_then(|()| fs::copy(bin, &staged).map(|_| ()))
        .and_then(|()| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
            }
            fs::rename(&staged, &copy)
        });
    match done {
        Ok(()) => copy,
        Err(_) => {
            let _ = fs::remove_file(&staged);
            bin.to_path_buf()
        }
    }
}

/// Creates the host dir user-only. A dir that already exists must be ours, a real directory
/// and closed to group/other: in a shared /tmp another user could have made it first.
pub fn ensure_host_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| e.to_string())?;
        let st = fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
        if !st.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        if i64::from(st.uid()) != unsafe { libc::getuid() } as i64 {
            return Err(format!("{} belongs to another user", dir.display()));
        }
        if st.mode() & 0o077 != 0 {
            return Err(format!("{} is open to other users", dir.display()));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir).map_err(|e| e.to_string())
}

/// The highest fd open in this process, or 2 when it cannot be listed.
#[cfg(unix)]
fn highest_open_fd() -> i32 {
    let dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
                .max()
                .unwrap_or(2)
                .max(2)
        })
        .unwrap_or(2)
}

pub fn start_host_process(
    paths: &HostPaths,
    app_version: &str,
    bin: Option<&Path>,
    cwd: &Path,
) -> Result<(), String> {
    let bin = bin.ok_or("This build has no background host (wraithgrid-host).")?;
    ensure_host_dir(&paths.dir)?;
    let size = fs::metadata(&paths.log).map(|m| m.len()).unwrap_or(0);
    let mut opts = fs::OpenOptions::new();
    if size > 1024 * 1024 {
        opts.write(true).create(true).truncate(true);
    } else {
        opts.append(true).create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let log = opts.open(&paths.log).map_err(|e| e.to_string())?;
    let settings = serde_json::json!({ "paths": paths, "appVersion": app_version, "maskedFds": 0 });
    let mut cmd = Command::new(bin);
    cmd.env("WRAITHGRID_HOST", settings.to_string())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let max_fd = highest_open_fd();
        // SAFETY: only async-signal-safe calls (setsid, fcntl) between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                // Its own session, like Node's `detached`, so it outlives this process.
                libc::setsid();
                // Nothing else this process holds open (GPU, Wayland, sockets) may leak into
                // the long-lived host.
                for fd in 3..=max_fd {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags >= 0 {
                        libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                    }
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    // A spawn that fails (missing binary, no permission) is reported by the connect that
    // follows ("did not start"), as in the Electron app.
    match cmd.spawn() {
        Ok(mut child) => {
            // Reap it whenever it exits so it never lingers as a zombie of this process.
            std::thread::spawn(move || child.wait());
            Ok(())
        }
        Err(err) => {
            eprintln!("[wraithgrid] host spawn failed: {err}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_lookup() {
        let env_bin = |k: &str| (k == "WRAITHGRID_HOST_BIN").then(|| "/opt/h".to_string());
        assert_eq!(
            host_binary(Path::new("/x/app"), env_bin),
            Some(PathBuf::from("/opt/h"))
        );
        let dir = std::env::temp_dir().join(format!("wg-hostbin-{}", std::process::id()));
        let release = dir.join("host-rs/target/release");
        fs::create_dir_all(&release).unwrap();
        fs::create_dir_all(dir.join("app-rs/target/debug")).unwrap();
        fs::write(release.join(HOST_BIN), "").unwrap();
        let exe = dir.join("app-rs/target/debug/wraithgrid");
        assert_eq!(host_binary(&exe, |_| None), Some(release.join(HOST_BIN)));
        fs::write(dir.join("app-rs/target/debug").join(HOST_BIN), "").unwrap();
        assert_eq!(
            host_binary(&exe, |_| None),
            Some(dir.join("app-rs/target/debug").join(HOST_BIN))
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn packaged_host_in_resources() {
        let dir = std::env::temp_dir().join(format!("wg-hostres-{}", std::process::id()));
        let (exe_dir, res) = if cfg!(windows) {
            (dir.clone(), dir.join("resources"))
        } else {
            (dir.join("Contents/MacOS"), dir.join("Contents/Resources"))
        };
        fs::create_dir_all(&exe_dir).unwrap();
        fs::create_dir_all(&res).unwrap();
        fs::write(res.join(HOST_BIN), "").unwrap();
        assert_eq!(
            host_binary(&exe_dir.join("Wraithgrid"), |_| None),
            Some(res.join(HOST_BIN))
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn appimage_hosts_run_from_a_copy() {
        let dir = std::env::temp_dir().join(format!("wg-stable-{}", std::process::id()));
        let mount = dir.join("mount");
        fs::create_dir_all(&mount).unwrap();
        let bin = mount.join(HOST_BIN);
        fs::write(&bin, "host").unwrap();
        let cache = dir.join("cache");
        assert_eq!(stable_host_binary(&bin, None, &cache, "2.0.0"), bin);
        let copy = stable_host_binary(&bin, Some(mount.to_str().unwrap()), &cache, "2.0.0");
        assert_eq!(copy, cache.join(format!("{HOST_BIN}-2.0.0")));
        assert_eq!(fs::read_to_string(&copy).unwrap(), "host");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn host_dir_is_private() {
        let dir = std::env::temp_dir()
            .join(format!("wg-hostdir-{}", std::process::id()))
            .join("run");
        ensure_host_dir(&dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ensure_host_dir(&dir)
            .unwrap_err()
            .contains("open to other users"));
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }
}
