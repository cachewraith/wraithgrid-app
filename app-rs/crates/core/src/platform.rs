//! Platform differences in one place (`src/main/platform.ts`, `src/main/claude-detect.ts`):
//! window chrome per desktop, shells, PATH lookups, the login-shell PATH probe and how a
//! binary is started in a pty. Plain functions of (os, env) so they are testable anywhere.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::pane_env::Os;

// ---- Desktop and window chrome ------------------------------------------------------

/// How the window frame is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chrome {
    /// Our title bar with min/max/close (GNOME, KDE, Xfce, …).
    Custom,
    /// Windows: our buttons, marked as caption areas so snap layouts still work.
    Overlay,
    /// A tiling compositor (Hyprland, sway, i3, …) manages size: only close shows.
    Tiling,
    /// Our title bar with macOS's own traffic lights inset at its left end.
    Mac,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopInfo {
    pub chrome: Chrome,
    /// First entry of XDG_CURRENT_DESKTOP, e.g. "Hyprland", "GNOME".
    pub desktop: Option<String>,
    pub wayland: bool,
}

const TILING_DESKTOPS: &[&str] = &[
    "hyprland",
    "sway",
    "i3",
    "river",
    "niri",
    "bspwm",
    "qtile",
    "dwm",
    "awesome",
    "xmonad",
    "herbstluftwm",
    "leftwm",
    "miracle-wm",
];

pub fn detect_desktop(os: Os, env: impl Fn(&str) -> Option<String>) -> DesktopInfo {
    match os {
        Os::Windows => {
            return DesktopInfo {
                chrome: Chrome::Overlay,
                desktop: Some("Windows".into()),
                wayland: false,
            }
        }
        Os::Mac => {
            return DesktopInfo {
                chrome: Chrome::Mac,
                desktop: Some("macOS".into()),
                wayland: false,
            }
        }
        Os::Linux => {}
    }
    let set = |k: &str| env(k).filter(|v| !v.is_empty());
    let names: Vec<String> = [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .iter()
    .filter_map(|k| set(k))
    .flat_map(|v| {
        v.to_lowercase()
            .split([':', ';'])
            .map(String::from)
            .collect::<Vec<_>>()
    })
    .collect();
    let tiling = [
        "HYPRLAND_INSTANCE_SIGNATURE",
        "SWAYSOCK",
        "I3SOCK",
        "NIRI_SOCKET",
    ]
    .iter()
    .any(|k| set(k).is_some())
        || names.iter().any(|n| TILING_DESKTOPS.contains(&n.as_str()));
    DesktopInfo {
        chrome: if tiling {
            Chrome::Tiling
        } else {
            Chrome::Custom
        },
        desktop: set("XDG_CURRENT_DESKTOP").map(|d| d.split(':').next().unwrap_or("").to_string()),
        wayland: set("WAYLAND_DISPLAY").is_some()
            || env("XDG_SESSION_TYPE").as_deref() == Some("wayland"),
    }
}

/// The layout adapts to small windows, so every desktop gets the size a tiling compositor
/// needed: a half tile on 1920 px is 960 px (DECISIONS 2026-10-21).
pub const MIN_WINDOW: (f32, f32) = (640.0, 420.0);

/// Where the traffic lights sit so they center in our 36 px title bar.
pub const MAC_TRAFFIC_LIGHTS: (f32, f32) = (12.0, 11.0);

// ---- Shells and binaries ------------------------------------------------------------

fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

/// A `which` lookup: the first executable `name` on PATH, or None.
pub fn find_on_path(name: &str, env: impl Fn(&str) -> Option<String>, os: Os) -> Option<PathBuf> {
    let path = env("PATH").or_else(|| env("Path")).unwrap_or_default();
    let exts: Vec<String> = if os == Os::Windows {
        let mut e: Vec<String> = env("PATHEXT")
            .unwrap_or(".EXE;.CMD;.BAT".into())
            .split(';')
            .map(String::from)
            .collect();
        e.push(String::new());
        e
    } else {
        vec![String::new()]
    };
    for dir in path.split(os.path_delimiter()).filter(|d| !d.is_empty()) {
        for ext in &exts {
            let candidate = Path::new(dir).join(format!("{name}{ext}"));
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// The shell a plain shell pane runs: $SHELL on POSIX, PowerShell 7 → 5 → cmd on Windows.
pub fn resolve_default_shell(
    os: Os,
    env: &dyn Fn(&str) -> Option<String>,
    find: &dyn Fn(&str) -> Option<PathBuf>,
    exists: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    if os == Os::Windows {
        return find("pwsh")
            .or_else(|| find("powershell"))
            .or_else(|| env("ComSpec").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("cmd.exe"));
    }
    // macOS's default shell is zsh (since 10.15); Linux's is usually bash.
    let fallbacks: &[&str] = if os == Os::Mac {
        &["/bin/zsh", "/bin/bash", "/bin/sh"]
    } else {
        &["/bin/bash", "/usr/bin/bash", "/bin/zsh", "/bin/sh"]
    };
    env("SHELL")
        .filter(|s| !s.is_empty())
        .into_iter()
        .chain(fallbacks.iter().map(|s| s.to_string()))
        .map(PathBuf::from)
        .find(|p| exists(p))
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

/// `/usr/bin/fish` → `fish`, `C:\x\pwsh.exe` → `pwsh`: the shell's family, for its flags.
pub fn shell_kind(file: &str) -> String {
    let name = file
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(file)
        .to_lowercase();
    name.strip_suffix(".exe").map(String::from).unwrap_or(name)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShellChoice {
    pub file: PathBuf,
    pub args: Vec<String>,
}

/// The shell picked in Settings (`pick_path` '' for automatic), else the default. A picked
/// shell that is gone is an error, not a silent swap.
pub fn resolve_pane_shell(
    os: Os,
    env: &dyn Fn(&str) -> Option<String>,
    find: &dyn Fn(&str) -> Option<PathBuf>,
    exists: &dyn Fn(&Path) -> bool,
    pick_path: &str,
    pick_args: &[String],
) -> Result<ShellChoice, String> {
    if pick_path.is_empty() {
        return Ok(ShellChoice {
            file: resolve_default_shell(os, env, find, exists),
            args: vec![],
        });
    }
    let absolute = if os == Os::Windows {
        pick_path.contains(":\\") || pick_path.starts_with("\\\\")
    } else {
        pick_path.starts_with('/')
    };
    // A bare name ("pwsh", "nu") is looked up on PATH.
    let file = if absolute {
        Some(PathBuf::from(pick_path))
    } else {
        find(pick_path)
    };
    match file {
        Some(f) if exists(&f) => Ok(ShellChoice {
            file: f,
            args: pick_args.to_vec(),
        }),
        _ => Err(format!(
            "Shell not found: {pick_path}. Pick another in Settings → General."
        )),
    }
}

/// Places `claude` is commonly installed that a GUI session's PATH often lacks.
pub fn extra_bin_dirs(os: Os, home: &str, env: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let join = |a: &str, parts: &[&str]| {
        let sep = if os == Os::Windows { "\\" } else { "/" };
        std::iter::once(a.trim_end_matches(['/', '\\']))
            .chain(parts.iter().copied())
            .collect::<Vec<_>>()
            .join(sep)
    };
    if os == Os::Windows {
        let mut out = vec![join(home, &[".local", "bin"])];
        if let Some(appdata) = env("APPDATA") {
            out.push(join(&appdata, &["npm"]));
        }
        out.push(join(home, &[".bun", "bin"]));
        return out;
    }
    let mut out = vec![
        join(home, &[".local", "bin"]),
        join(home, &[".claude", "local"]),
    ];
    // Homebrew: Apple Silicon, then Intel (/usr/local/bin below).
    if os == Os::Mac {
        out.push("/opt/homebrew/bin".into());
        out.push("/opt/homebrew/sbin".into());
    }
    out.push(join(home, &[".npm-global", "bin"]));
    out.push(join(home, &[".bun", "bin"]));
    out.push(join(home, &[".volta", "bin"]));
    out.push("/usr/local/bin".into());
    out
}

/// Joins PATH lists in priority order, dropping empty and duplicate entries.
pub fn merge_path(delimiter: char, lists: &[Option<&str>]) -> String {
    let mut out: Vec<&str> = Vec::new();
    for list in lists.iter().flatten() {
        for dir in list.split(delimiter) {
            if !dir.is_empty() && !out.contains(&dir) {
                out.push(dir);
            }
        }
    }
    out.join(&delimiter.to_string())
}

/// How to start `file` in a pty: a list of args, or (Windows `.cmd` shims) one verbatim line.
#[derive(Clone, Debug, PartialEq)]
pub enum Args {
    List(Vec<String>),
    Verbatim(String),
}

/// On Windows an npm-installed claude is a `.cmd` shim, which CreateProcess cannot run
/// directly; it goes through `cmd.exe /d /s /c`, and arguments cmd.exe would reinterpret
/// are refused rather than escaped.
pub fn spawn_command_for(
    os: Os,
    file: &str,
    args: &[String],
    comspec: Option<&str>,
) -> Result<(String, Args), String> {
    let lower = file.to_lowercase();
    if os != Os::Windows || !(lower.ends_with(".cmd") || lower.ends_with(".bat")) {
        return Ok((file.to_string(), Args::List(args.to_vec())));
    }
    let unsafe_char = |s: &str| s.chars().any(|c| "&|<>^%\"\r\n".contains(c));
    if let Some(bad) = std::iter::once(file)
        .chain(args.iter().map(String::as_str))
        .find(|a| unsafe_char(a))
    {
        let base = file.rsplit(['\\', '/']).next().unwrap_or(file);
        return Err(format!(
            "\"{bad}\" contains characters that cannot pass through {base}. Remove them or install the native claude.exe."
        ));
    }
    let quote = |a: &String| {
        if a.is_empty() || a.chars().any(char::is_whitespace) {
            format!("\"{a}\"")
        } else {
            a.clone()
        }
    };
    let line = std::iter::once(format!("\"{file}\""))
        .chain(args.iter().map(quote))
        .collect::<Vec<_>>()
        .join(" ");
    Ok((
        comspec
            .filter(|c| !c.is_empty())
            .unwrap_or("cmd.exe")
            .to_string(),
        Args::Verbatim(format!("/d /s /c \"{line}\"")),
    ))
}

// ---- Login-shell environment --------------------------------------------------------

pub const ENV_MARK: &str = "__WRAITHGRID_ENV__";

/// Pulls PATH out of the probe's output framed by markers (shell noise before or after is
/// ignored): either `printenv PATH` (one line) or an `env -0` dump (NUL-separated).
pub fn parse_path_from_env_dump(output: &str) -> Option<String> {
    let start = output.find(ENV_MARK)?;
    let end = output.rfind(ENV_MARK)?;
    if end <= start {
        return None;
    }
    let body = &output[start + ENV_MARK.len()..end];
    if body.contains('\0') {
        return body
            .split('\0')
            .find_map(|e| e.strip_prefix("PATH="))
            .filter(|p| !p.is_empty())
            .map(String::from);
    }
    let line = body.trim();
    (!line.is_empty() && !line.contains('\n')).then(|| line.to_string())
}

fn probe() -> String {
    format!("printf '%s' {ENV_MARK}; printenv PATH; printf '%s' {ENV_MARK}")
}

/// How to run the probe as a login, interactive shell of this kind, so it reads the same
/// startup files as a terminal would. csh/tcsh only allow `-l` alone, so they get `-c`.
pub fn login_probe_args(shell: &str) -> Vec<String> {
    let p = probe();
    let v = |a: &[&str], cmd: String| {
        a.iter()
            .map(|s| s.to_string())
            .chain(std::iter::once(cmd))
            .collect()
    };
    match shell_kind(shell).as_str() {
        "bash" | "zsh" | "sh" | "dash" | "ash" | "ksh" | "mksh" | "oksh" | "yash" | "fish" => {
            v(&["-i", "-l", "-c"], p)
        }
        "xonsh" => v(&["-l", "-i", "-c"], p),
        // nushell has no `printf`; `^` runs the external programs.
        "nu" => v(
            &["-l", "-i", "-c"],
            format!("^printf '%s' {ENV_MARK}; ^printenv PATH; ^printf '%s' {ENV_MARK}"),
        ),
        "pwsh" => v(&["-Login", "-NoLogo", "-Command"], p),
        _ => v(&["-c"], p),
    }
}

/// Runs a probe with a timeout; its stdout, or '' on any failure.
fn run_probe(file: &str, args: &[String], timeout: Duration) -> String {
    let child = Command::new(file)
        .args(args)
        // Lets a user's rc file skip slow setup, as with VSCODE_RESOLVING_ENVIRONMENT.
        .env("WRAITHGRID_RESOLVING_ENVIRONMENT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return String::new();
    };
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return reader.join().unwrap_or_default();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    reader.join().unwrap_or_default()
}

/// Apps started from a launcher (a Hyprland `exec`, a .desktop file) do not get the PATH a
/// login shell sets up. Asks the user's login shell once, like VS Code; if that shell can't
/// answer, `/bin/sh -l` does. POSIX only; never fails.
pub fn login_shell_path(shell: Option<&str>, timeout: Duration) -> Option<String> {
    login_shell_path_with(shell, &|file, args| run_probe(file, args, timeout))
}

pub fn login_shell_path_with(
    shell: Option<&str>,
    run: &dyn Fn(&str, &[String]) -> String,
) -> Option<String> {
    if let Some(shell) = shell.filter(|s| !s.is_empty()) {
        if let Some(found) = parse_path_from_env_dump(&run(shell, &login_probe_args(shell))) {
            return Some(found);
        }
    }
    parse_path_from_env_dump(&run("/bin/sh", &["-l".into(), "-c".into(), probe()]))
}

/// The PATH this process should use, as app.ts sets it before anything spawns: the login
/// shell's (unless started from a terminal), then the inherited one, then common bin dirs.
pub fn resolve_process_path(os: Os, home: &str) -> String {
    let env = |k: &str| std::env::var(k).ok();
    let shell_path = if os != Os::Windows && env("TERM").is_none() {
        login_shell_path(env("SHELL").as_deref(), Duration::from_secs(4))
    } else {
        None
    };
    let inherited = env("PATH");
    let extra = extra_bin_dirs(os, home, env).join(&os.path_delimiter().to_string());
    merge_path(
        os.path_delimiter(),
        &[shell_path.as_deref(), inherited.as_deref(), Some(&extra)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktops() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|p| p.0 == k).map(|p| p.1.to_string())
        };
        assert_eq!(
            detect_desktop(Os::Linux, env(&[("HYPRLAND_INSTANCE_SIGNATURE", "x")])).chrome,
            Chrome::Tiling
        );
        assert_eq!(
            detect_desktop(Os::Linux, env(&[("XDG_CURRENT_DESKTOP", "ubuntu:GNOME")])).chrome,
            Chrome::Custom
        );
        assert_eq!(
            detect_desktop(Os::Linux, env(&[("XDG_CURRENT_DESKTOP", "ubuntu:GNOME")]))
                .desktop
                .as_deref(),
            Some("ubuntu")
        );
        assert_eq!(
            detect_desktop(Os::Linux, env(&[("DESKTOP_SESSION", "Sway")])).chrome,
            Chrome::Tiling
        );
        assert!(detect_desktop(Os::Linux, env(&[("XDG_SESSION_TYPE", "wayland")])).wayland);
        assert_eq!(
            detect_desktop(Os::Windows, env(&[])).chrome,
            Chrome::Overlay
        );
        assert_eq!(detect_desktop(Os::Mac, env(&[])).chrome, Chrome::Mac);
    }

    #[test]
    fn shells() {
        assert_eq!(shell_kind("/usr/bin/fish"), "fish");
        assert_eq!(shell_kind("C:\\x\\PWSH.exe"), "pwsh");
        let env = |k: &str| (k == "SHELL").then(|| "/usr/bin/fish".to_string());
        let none = |_: &str| None;
        let exists_all = |_: &Path| true;
        let exists_none = |_: &Path| false;
        assert_eq!(
            resolve_default_shell(Os::Linux, &env, &none, &exists_all),
            PathBuf::from("/usr/bin/fish")
        );
        assert_eq!(
            resolve_default_shell(Os::Linux, &env, &none, &exists_none),
            PathBuf::from("/bin/sh")
        );
        let found = |n: &str| (n == "nu").then(|| PathBuf::from("/usr/bin/nu"));
        assert_eq!(
            resolve_pane_shell(Os::Linux, &env, &found, &exists_all, "nu", &["-l".into()])
                .unwrap()
                .file,
            PathBuf::from("/usr/bin/nu")
        );
        assert!(
            resolve_pane_shell(Os::Linux, &env, &none, &exists_all, "xonsh", &[])
                .unwrap_err()
                .starts_with("Shell not found: xonsh.")
        );
    }

    #[test]
    fn paths() {
        assert_eq!(
            merge_path(':', &[Some("/a:/b"), None, Some("/b::/c")]),
            "/a:/b:/c"
        );
        let dirs = extra_bin_dirs(Os::Linux, "/home/u", |_| None);
        assert_eq!(dirs[0], "/home/u/.local/bin");
        assert_eq!(dirs.last().unwrap(), "/usr/local/bin");
        assert!(extra_bin_dirs(Os::Mac, "/Users/u", |_| None)
            .contains(&"/opt/homebrew/bin".to_string()));
    }

    #[test]
    fn windows_cmd_shims() {
        let args = vec!["--model".to_string(), "a b".to_string()];
        assert_eq!(
            spawn_command_for(Os::Linux, "/x/claude.cmd", &args, None)
                .unwrap()
                .1,
            Args::List(args.clone())
        );
        let (file, a) =
            spawn_command_for(Os::Windows, "C:\\n\\claude.cmd", &args, Some("C:\\cmd.exe"))
                .unwrap();
        assert_eq!(file, "C:\\cmd.exe");
        assert_eq!(
            a,
            Args::Verbatim("/d /s /c \"\"C:\\n\\claude.cmd\" --model \"a b\"\"".into())
        );
        assert!(
            spawn_command_for(Os::Windows, "c.cmd", &["a&b".into()], None)
                .unwrap_err()
                .contains("\"a&b\" contains characters")
        );
    }

    #[test]
    fn env_dump() {
        let out = format!("noise{ENV_MARK}/usr/bin:/bin\n{ENV_MARK}more");
        assert_eq!(
            parse_path_from_env_dump(&out).as_deref(),
            Some("/usr/bin:/bin")
        );
        let nul = format!("{ENV_MARK}A=1\0PATH=/x\0{ENV_MARK}");
        assert_eq!(parse_path_from_env_dump(&nul).as_deref(), Some("/x"));
        assert_eq!(parse_path_from_env_dump("nothing"), None);
        assert_eq!(login_probe_args("/bin/tcsh")[0], "-c");
        assert_eq!(login_probe_args("/usr/bin/fish")[..3], ["-i", "-l", "-c"]);
    }

    #[test]
    fn login_probe_falls_back_to_sh() {
        let run = |file: &str, _: &[String]| {
            if file == "/bin/sh" {
                format!("{ENV_MARK}/from/sh{ENV_MARK}")
            } else {
                "broken".into()
            }
        };
        assert_eq!(
            login_shell_path_with(Some("/bin/weird"), &run).as_deref(),
            Some("/from/sh")
        );
    }
}
