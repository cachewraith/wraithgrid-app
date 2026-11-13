//! Web preview (#74, `src/shared/preview.ts` + `src/main/preview.ts`): which command runs a
//! project's dev server, which addresses may load, the address the server prints, the report
//! "Attach to chat" writes, and the server itself, run over pipes in the project folder and
//! stopped with its pane.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::Duration;

use regex::Regex;

/// Lines of server output kept for the Log drawer.
pub const PREVIEW_LOG_MAX: usize = 2000;
/// Console errors and failed requests kept per page load, so a noisy page can't grow the
/// report without end.
pub const PREVIEW_REPORT_MAX: usize = 20;
/// How much of an unfinished output line is held back waiting for its end.
const LINE_REST_MAX: usize = 64 * 1024;
/// How long a stop waits after SIGTERM before it kills the server's whole tree.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// What a dev server says, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewEvent {
    Log(String),
    /// The first local address the server printed.
    Url(String),
    Exit {
        code: Option<i32>,
        error: Option<String>,
    },
}

/// What the folder says about how it runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectFiles {
    /// package.json `scripts`, or None without one.
    pub scripts: Option<BTreeMap<String, String>>,
    /// File names in the folder (lockfiles among them).
    pub lockfiles: Vec<String>,
    /// A Laravel `artisan` file.
    pub artisan: bool,
}

const RUNNERS: [(&str, &str); 5] = [
    ("bun.lock", "bun run"),
    ("bun.lockb", "bun run"),
    ("pnpm-lock.yaml", "pnpm run"),
    ("yarn.lock", "yarn"),
    ("package-lock.json", "npm run"),
];

/// The command that starts the project's dev server, and where it came from.
pub fn detect_dev_command(files: &ProjectFiles) -> Option<(String, String)> {
    let script = ["dev", "start"].into_iter().find(|s| {
        files
            .scripts
            .as_ref()
            .and_then(|m| m.get(*s))
            .is_some_and(|v| !v.is_empty())
    });
    if let Some(script) = script {
        let found = RUNNERS
            .iter()
            .find(|(l, _)| files.lockfiles.iter().any(|f| f == l));
        let run = found.map_or("npm run", |(_, r)| r);
        let lock = found.map(|(l, _)| format!(", {l}")).unwrap_or_default();
        return Some((
            format!("{run} {script}"),
            format!("package.json \"{script}\"{lock}"),
        ));
    }
    files
        .artisan
        .then(|| ("php artisan serve".into(), "artisan".into()))
}

/// Reads package.json scripts, lockfiles and artisan; a missing folder detects nothing.
pub fn read_project_files(dir: &Path) -> ProjectFiles {
    let names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let scripts = names
        .iter()
        .any(|n| n == "package.json")
        .then(|| std::fs::read_to_string(dir.join("package.json")).ok())
        .flatten()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|pkg| {
            pkg.get("scripts")?.as_object().map(|o| {
                o.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
        });
    ProjectFiles {
        scripts,
        artisan: names.iter().any(|n| n == "artisan"),
        lockfiles: names,
    }
}

/// A URL's scheme, host (lowercased, `[::1]` kept bracketed) and the rest from the path on.
fn split_url(url: &str) -> Option<(String, String, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(end);
    let host_port = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    let host = if host_port.starts_with('[') {
        &host_port[..host_port.find(']')? + 1]
    } else {
        let host = host_port.split(':').next().unwrap_or("");
        let port = &host_port[host.len()..];
        if !port.is_empty() && !port[1..].chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        host
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), host.to_ascii_lowercase(), tail))
}

/// Only this computer's own web servers load in the preview; anything else opens in the browser.
pub fn is_local_url(url: &str) -> bool {
    let Some((scheme, host, _)) = split_url(url.trim()) else {
        return false;
    };
    (scheme == "http" || scheme == "https")
        && (["localhost", "127.0.0.1", "[::1]"].contains(&host.as_str())
            || host.ends_with(".localhost"))
}

/// What the address bar's text loads: as typed with a scheme, else over http.
pub fn address_url(text: &str) -> String {
    let t = text.trim();
    let scheme = t
        .split_once("://")
        .is_some_and(|(s, _)| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic()));
    if scheme {
        t.to_string()
    } else {
        format!("http://{t}")
    }
}

/// A URL's path and query (`/` for none), or None when it isn't a URL.
fn path_and_query(url: &str) -> Option<String> {
    let (_, _, tail) = split_url(url)?;
    let tail = tail.split('#').next().unwrap_or("");
    Some(if tail.starts_with('/') {
        tail.to_string()
    } else {
        format!("/{tail}")
    })
}

fn ansi() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07").unwrap())
}

/// A line of server output without its colours.
pub fn plain_line(line: &str) -> String {
    ansi().replace_all(line, "").replace('\r', "")
}

/// The local address in a line of server output (`0.0.0.0` read as localhost), or None.
pub fn find_local_url(line: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(?::\d+)?[^\s'"<>)\]]*"#,
        )
        .unwrap()
    });
    let plain = plain_line(line);
    let m = re.find(&plain)?;
    let url =
        m.as_str()
            .trim_end_matches(['.', ',', ';', ':'])
            .replacen("//0.0.0.0", "//localhost", 1);
    is_local_url(&url).then_some(url)
}

/// How the command runs: the platform's shell, so `bun run dev` and `&&` work as typed.
/// Windows' line is one raw argument (`cmd /d /s /c "…"`), as Node passes it.
pub fn shell_launch(command: &str, windows: bool, comspec: Option<&str>) -> (String, Vec<String>) {
    if windows {
        let file = comspec.filter(|c| !c.is_empty()).unwrap_or("cmd.exe");
        (file.into(), vec![format!("/d /s /c \"{command}\"")])
    } else {
        ("/bin/sh".into(), vec!["-c".into(), command.into()])
    }
}

/// A request from the preview's page that failed (status ≥ 400 or a network error).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewFailure {
    pub method: String,
    pub url: String,
    /// The HTTP status, or the engine's error.
    pub result: String,
}

/// What "Attach to chat" tells the agent about the page, as text, and the chip's summary.
pub fn preview_report(
    url: &str,
    size: Option<(u32, u32)>,
    errors: &[String],
    failed: &[PreviewFailure],
) -> (String, String) {
    let path = path_and_query(url).unwrap_or_else(|| url.to_string());
    let n = |k: usize, one: &str| format!("{k} {one}{}", if k == 1 { "" } else { "s" });
    let mut lines = vec![format!(
        "Preview of {url}{}",
        size.map(|(w, h)| format!(" ({w}×{h})")).unwrap_or_default()
    )];
    lines.push(
        if errors.is_empty() {
            "Console errors: none"
        } else {
            "Console errors:"
        }
        .into(),
    );
    for e in errors {
        let one: String = e.split_whitespace().collect::<Vec<_>>().join(" ");
        lines.push(format!("  {}", one.chars().take(500).collect::<String>()));
    }
    lines.push(
        if failed.is_empty() {
            "Failed requests: none"
        } else {
            "Failed requests:"
        }
        .into(),
    );
    for f in failed {
        let at = if is_local_url(&f.url) {
            path_and_query(&f.url).unwrap_or_else(|| f.url.clone())
        } else {
            f.url.clone()
        };
        lines.push(format!("  {} {at} → {}", f.method, f.result));
    }
    (
        format!(
            "Preview {path} · {} · {}",
            n(errors.len(), "console error"),
            n(failed.len(), "failed request")
        ),
        lines.join("\n"),
    )
}

/// What a running server shares with its reader and waiter threads.
struct Run {
    pid: u32,
    /// The pane asked it to stop: nothing more is said.
    stopped: bool,
    exited: bool,
    url_seen: bool,
    lines: usize,
}

/// A preview pane's dev server (`DevServers`): over pipes, in its own process group, so a
/// stop also ends what it started. Dropping it stops it.
pub struct DevServer {
    run: Arc<Mutex<Run>>,
}

type Sink = Arc<dyn Fn(PreviewEvent) + Send + Sync>;

impl DevServer {
    /// Starts `file args` in `cwd` with exactly `env`; events go to `sink` from other threads.
    pub fn start(
        file: &str,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        sink: impl Fn(PreviewEvent) + Send + Sync + 'static,
    ) -> Result<DevServer, String> {
        let mut cmd = Command::new(file);
        #[cfg(windows)]
        for a in args {
            std::os::windows::process::CommandExt::raw_arg(&mut cmd, a);
        }
        #[cfg(not(windows))]
        cmd.args(args);
        cmd.current_dir(cwd)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let run = Arc::new(Mutex::new(Run {
            pid: child.id(),
            stopped: false,
            exited: false,
            url_seen: false,
            lines: 0,
        }));
        let sink: Sink = Arc::new(sink);
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let streams: Vec<Box<dyn Read + Send>> = [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        .collect();
        let readers = streams.len();
        for stream in streams {
            let (run, sink, done) = (run.clone(), sink.clone(), done_tx.clone());
            thread::spawn(move || {
                read_lines(stream, |line| on_line(&run, &sink, line));
                let _ = done.send(());
            });
        }
        let waiter = run.clone();
        thread::spawn(move || {
            let status = child.wait();
            // What the streams still hold comes first; a grandchild holding them open
            // doesn't keep the exit back.
            for _ in 0..readers {
                if done_rx.recv_timeout(Duration::from_millis(300)).is_err() {
                    break;
                }
            }
            let quiet = {
                let mut r = waiter.lock().unwrap_or_else(PoisonError::into_inner);
                r.exited = true;
                r.stopped
            };
            if !quiet {
                sink(match status {
                    Ok(s) => PreviewEvent::Exit {
                        code: s.code(),
                        error: None,
                    },
                    Err(e) => PreviewEvent::Exit {
                        code: None,
                        error: Some(e.to_string()),
                    },
                });
            }
        });
        Ok(DevServer { run })
    }

    /// Stops the server (and what it started), quietly: SIGTERM to its group, then a forced
    /// kill of the whole tree after a grace time.
    pub fn stop(&self) {
        let (pid, exited) = {
            let mut r = self.run.lock().unwrap_or_else(PoisonError::into_inner);
            if r.stopped {
                return;
            }
            r.stopped = true;
            (r.pid, r.exited)
        };
        signal(pid, false, exited);
        let run = self.run.clone();
        thread::spawn(move || {
            thread::sleep(STOP_GRACE);
            let exited = run.lock().unwrap_or_else(PoisonError::into_inner).exited;
            signal(pid, true, exited);
        });
    }

    pub fn running(&self) -> bool {
        let r = self.run.lock().unwrap_or_else(PoisonError::into_inner);
        !r.stopped && !r.exited
    }
}

impl Drop for DevServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn on_line(run: &Mutex<Run>, sink: &Sink, raw: &str) {
    let line = plain_line(raw);
    if line.trim().is_empty() {
        return;
    }
    let url = {
        let mut r = run.lock().unwrap_or_else(PoisonError::into_inner);
        // A server stuck printing forever still leaves the app responsive.
        r.lines += 1;
        if r.stopped || r.lines > PREVIEW_LOG_MAX * 10 {
            return;
        }
        let url = if r.url_seen {
            None
        } else {
            find_local_url(&line)
        };
        r.url_seen |= url.is_some();
        url
    };
    sink(PreviewEvent::Log(line));
    if let Some(url) = url {
        sink(PreviewEvent::Url(url));
    }
}

/// Line by line, so a URL cut across two reads is read whole (#140); a line that never ends
/// (a progress bar redrawn with `\r`) still shows, in pieces.
fn read_lines(stream: impl Read, mut each: impl FnMut(&str)) {
    let mut r = BufReader::new(stream);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let (consumed, done) = match r.fill_buf() {
            Ok([]) | Err(_) => break,
            Ok(chunk) => match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    buf.extend_from_slice(&chunk[..i]);
                    (i + 1, true)
                }
                None => {
                    buf.extend_from_slice(chunk);
                    (chunk.len(), buf.len() > LINE_REST_MAX)
                }
            },
        };
        r.consume(consumed);
        if done {
            each(&String::from_utf8_lossy(&buf));
            buf.clear();
        }
    }
    if !buf.is_empty() {
        each(&String::from_utf8_lossy(&buf));
    }
}

/// SIGTERM (SIGKILL when `force`) to the server's process group; the leader alone when the
/// group is gone and it hasn't exited. Windows: ends the tree (`taskkill /T`, `/F` when forced).
fn signal(pid: u32, force: bool, exited: bool) {
    #[cfg(unix)]
    {
        let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
        // SAFETY: kill(2) on the group we started; it only sends a signal.
        let group = unsafe { libc::kill(-(pid as libc::pid_t), sig) } == 0;
        if !group && !exited {
            // SAFETY: as above, the leader we started and haven't reaped.
            unsafe { libc::kill(pid as libc::pid_t, sig) };
        }
    }
    #[cfg(not(unix))]
    {
        let _ = exited;
        let mut args = vec!["/PID".to_string(), pid.to_string(), "/T".into()];
        if force {
            args.push("/F".into());
        }
        let _ = Command::new("taskkill")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(scripts: &[(&str, &str)], lock: &[&str], artisan: bool) -> ProjectFiles {
        ProjectFiles {
            scripts: (!scripts.is_empty()).then(|| {
                scripts
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            }),
            lockfiles: lock.iter().map(|s| s.to_string()).collect(),
            artisan,
        }
    }

    #[test]
    fn detects_the_dev_command() {
        assert_eq!(
            detect_dev_command(&files(
                &[("dev", "vite"), ("start", "x")],
                &["bun.lock"],
                false
            )),
            Some((
                "bun run dev".into(),
                "package.json \"dev\", bun.lock".into()
            ))
        );
        assert_eq!(
            detect_dev_command(&files(&[("start", "node s")], &[], false)),
            Some(("npm run start".into(), "package.json \"start\"".into()))
        );
        assert_eq!(
            detect_dev_command(&files(&[("dev", "x")], &["yarn.lock"], false))
                .unwrap()
                .0,
            "yarn dev"
        );
        assert_eq!(
            detect_dev_command(&files(&[("build", "x")], &[], true)),
            Some(("php artisan serve".into(), "artisan".into()))
        );
        assert_eq!(detect_dev_command(&files(&[], &[], false)), None);
    }

    #[test]
    fn reads_project_files() {
        let dir = std::env::temp_dir().join(format!("wg-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"scripts":{"dev":"vite","n":1}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        let f = read_project_files(&dir);
        assert_eq!(
            detect_dev_command(&f).unwrap().0,
            "pnpm run dev".to_string()
        );
        assert_eq!(f.scripts.unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read_project_files(&dir), ProjectFiles::default());
    }

    #[test]
    fn local_urls_only() {
        for ok in [
            "http://localhost:5173/",
            "https://127.0.0.1",
            "http://[::1]:3000/x",
            "http://app.localhost",
            "HTTP://LOCALHOST/",
            "http://user@localhost:80/",
        ] {
            assert!(is_local_url(ok), "{ok}");
        }
        for no in [
            "https://example.com",
            "file:///etc/passwd",
            "http://localhost.evil.com",
            "javascript:alert(1)",
            "localhost:3000",
            "http://localhost@evil.com/",
            "",
        ] {
            assert!(!is_local_url(no), "{no}");
        }
    }

    #[test]
    fn finds_the_printed_address() {
        assert_eq!(
            find_local_url("  \x1b[32m➜\x1b[39m  Local:   \x1b[36mhttp://localhost:\x1b[1m5173\x1b[22m/\x1b[39m"),
            Some("http://localhost:5173/".into())
        );
        assert_eq!(
            find_local_url("Server running on [http://127.0.0.1:8000]."),
            Some("http://127.0.0.1:8000".into())
        );
        assert_eq!(
            find_local_url("listening at http://0.0.0.0:3000, ready"),
            Some("http://localhost:3000".into())
        );
        assert_eq!(find_local_url("see https://vitejs.dev"), None);
        assert_eq!(plain_line("a\x1b]0;t\x07b\r"), "ab");
    }

    #[test]
    fn address_and_shell() {
        assert_eq!(address_url(" localhost:3000 "), "http://localhost:3000");
        assert_eq!(address_url("https://x.dev"), "https://x.dev");
        assert_eq!(
            shell_launch("bun run dev", false, None),
            ("/bin/sh".into(), vec!["-c".into(), "bun run dev".into()])
        );
        assert_eq!(
            shell_launch("npm start", true, None),
            ("cmd.exe".into(), vec!["/d /s /c \"npm start\"".into()])
        );
    }

    #[test]
    fn writes_the_report() {
        let (label, text) = preview_report(
            "http://localhost:5173/app?x=1",
            Some((800, 600)),
            &["boom\n  at x".into()],
            &[
                PreviewFailure {
                    method: "GET".into(),
                    url: "http://localhost:5173/missing.png".into(),
                    result: "404".into(),
                },
                PreviewFailure {
                    method: "GET".into(),
                    url: "https://cdn.example.com/a.js".into(),
                    result: "failed".into(),
                },
            ],
        );
        assert_eq!(
            label,
            "Preview /app?x=1 · 1 console error · 2 failed requests"
        );
        assert_eq!(
            text,
            "Preview of http://localhost:5173/app?x=1 (800×600)\nConsole errors:\n  boom at x\nFailed requests:\n  GET /missing.png → 404\n  GET https://cdn.example.com/a.js → failed"
        );
        let (label, text) = preview_report("http://localhost:3000", None, &[], &[]);
        assert_eq!(label, "Preview / · 0 console errors · 0 failed requests");
        assert_eq!(
            text,
            "Preview of http://localhost:3000\nConsole errors: none\nFailed requests: none"
        );
    }

    #[cfg(unix)]
    #[test]
    fn runs_reads_and_stops_a_server() {
        let (tx, rx) = mpsc::channel();
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let (file, args) = shell_launch(
            "printf 'starting\\nLocal: http://localhost:'; sleep 0.2; printf '51'; sleep 0.1; printf '73/\\n'; echo again http://localhost:9/; exit 3",
            false,
            None,
        );
        let server = DevServer::start(&file, &args, &std::env::temp_dir(), &env, move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        let mut got = vec![];
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(5)) {
            let end = matches!(e, PreviewEvent::Exit { .. });
            got.push(e);
            if end {
                break;
            }
        }
        assert_eq!(
            got,
            vec![
                PreviewEvent::Log("starting".into()),
                PreviewEvent::Log("Local: http://localhost:5173/".into()),
                PreviewEvent::Url("http://localhost:5173/".into()),
                PreviewEvent::Log("again http://localhost:9/".into()),
                PreviewEvent::Exit {
                    code: Some(3),
                    error: None
                },
            ]
        );
        assert!(!server.running());

        // A stop is quiet and ends the server's children too.
        let (tx, rx) = mpsc::channel();
        let (file, args) = shell_launch("sleep 30 & sleep 30; echo late", false, None);
        let server = DevServer::start(&file, &args, &std::env::temp_dir(), &env, move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        assert!(server.running());
        let pid = server.run.lock().unwrap().pid as libc::pid_t;
        server.stop();
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_err());
        // SAFETY: probing our own group with signal 0.
        assert_ne!(unsafe { libc::kill(-pid, 0) }, 0);
    }

    #[test]
    fn a_missing_folder_fails_to_start() {
        let env = BTreeMap::new();
        let (file, args) = shell_launch("true", cfg!(windows), None);
        assert!(
            DevServer::start(&file, &args, Path::new("/no/such/wg-folder"), &env, |_| {}).is_err()
        );
    }
}
