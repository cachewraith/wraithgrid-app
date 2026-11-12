//! The Rust host on a machine reached over ssh (ADR 0004 phase 5, `src/main/remote-bin.ts`):
//! copied there once per version, so a remote pane needs no Wraithgrid installed on that
//! machine. Linux only; any other machine, or one this build has no binary for, keeps
//! `wraithgrid host-stdio`.

use crate::ssh::SshRun;

/// `uname -m` → the musl target whose static binary runs there.
fn target_for(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" | "amd64" => Some("x86_64-unknown-linux-musl"),
        "aarch64" | "arm64" => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

const PRESENT: &str = "__WRAITHGRID_HOST_PRESENT__";

/// From `uname -sm` ("Linux x86_64"); None on anything but Linux on x64/arm64.
pub fn remote_target(uname: &str) -> Option<&'static str> {
    let mut parts = uname.split_whitespace();
    let (os, arch) = (parts.next()?, parts.next().unwrap_or(""));
    (os == "Linux").then(|| target_for(arch)).flatten()
}

/// Relative to the remote home, where ssh starts.
pub fn remote_bin_path(version: &str) -> String {
    format!(".wraithgrid/bin/wraithgrid-host-{version}")
}

/// Other versions unused this long are deleted from a machine; 0 days would make two
/// Wraithgrid versions that share one machine copy theirs over again on every connect.
pub const UNUSED_DAYS: u32 = 30;

/// POSIX sh and fish both run these: plain commands joined by `;` and `&&`. A binary counts
/// only once it ran there (`--version`), so one that can't run falls back instead of failing.
/// Found, it is touched (its date says when it was last used), and other versions unused for
/// UNUSED_DAYS go; a host still running one keeps running, since Linux keeps a deleted file
/// open for it.
pub fn preflight_command(version: &str) -> String {
    let bin = remote_bin_path(version);
    // `command`: a shell function named find (a fish config's) must not stand in for it.
    let sweep = format!("command find .wraithgrid/bin -maxdepth 1 -type f -name 'wraithgrid-host-*' ! -name 'wraithgrid-host-{version}' -mtime +{UNUSED_DAYS} -delete 2>/dev/null");
    format!("uname -sm; test -x {bin} && {bin} --version >/dev/null && touch {bin} && echo {PRESENT}; {sweep}")
}

pub fn upload_command(version: &str) -> String {
    let tmp = format!(".wraithgrid/bin/.wraithgrid-host-{version}.part");
    let bin = remote_bin_path(version);
    format!("mkdir -p .wraithgrid/bin && cat > {tmp} && chmod 700 {tmp} && {tmp} --version >/dev/null && mv -f {tmp} {bin}")
}

/// Runs a command on the machine; the bytes go to its stdin.
pub type RunOnMachine<'a> = dyn Fn(&str, Option<&[u8]>) -> std::io::Result<SshRun> + 'a;

pub struct PrepareDeps<'a> {
    pub version: &'a str,
    pub run: &'a RunOnMachine<'a>,
    /// This build's binary for a target, or None when it has none.
    pub binary: &'a dyn Fn(&str) -> Option<Vec<u8>>,
    pub log: &'a dyn Fn(&str),
}

/// Makes sure the machine has this version's Rust host. Ok(Some) is the ssh command that runs
/// its bridge, Ok(None) falls back to `wraithgrid host-stdio`; Err when ssh could not run.
pub fn prepare_remote(d: &PrepareDeps) -> Result<Option<Vec<String>>, String> {
    // The version lands in a shell command: only what a release version can hold.
    let safe = |c: char| c.is_ascii_alphanumeric() || ".+-".contains(c);
    if d.version.is_empty() || !d.version.chars().all(safe) {
        return Ok(None);
    }
    let pre = (d.run)(&preflight_command(d.version), None).map_err(|e| e.to_string())?;
    let first = pre.stdout.split('\n').next().unwrap_or("");
    let Some(target) = remote_target(first) else {
        (d.log)(&format!(
            "no Rust host for \"{}\"; using wraithgrid host-stdio",
            first.trim()
        ));
        return Ok(None);
    };
    if !pre.stdout.contains(PRESENT) {
        let Some(bin) = (d.binary)(target) else {
            (d.log)(&format!(
                "this build has no {target} host; using wraithgrid host-stdio"
            ));
            return Ok(None);
        };
        let up = (d.run)(&upload_command(d.version), Some(&bin)).map_err(|e| e.to_string())?;
        if up.code != Some(0) {
            let code = up.code.map_or("null".to_string(), |c| c.to_string());
            (d.log)(&format!(
                "copying the host failed ({code}); using wraithgrid host-stdio"
            ));
            return Ok(None);
        }
    }
    Ok(Some(vec![
        remote_bin_path(d.version),
        "stdio".into(),
        d.version.into(),
    ]))
}
