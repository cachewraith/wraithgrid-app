//! Sign-in from a Wraithgrid dialog (`Logins` in src/main/login.ts): the CLI's own login runs
//! over pipes in the account's config dir (claude `auth login`, codex `login --device-auth`).
//! The CLI does the OAuth and keeps the tokens; this passes on the sign-in link (and codex's
//! one-time code) as `login:event`, the code the user pastes, and what the CLI's status
//! command says at the end. One sign-in at most per account.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::AppHandle;
use wraithgrid_core::host::protocol::SpawnRequest;
use wraithgrid_core::login::{login_url, LoginFlow};

const OUTPUT_MAX: usize = 64_000;

/// A resolved command (`ChatLaunch`).
#[derive(Clone)]
pub struct Launch {
    pub file: String,
    /// A list, or one verbatim command line (Windows `.cmd` shims).
    pub args: Value,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

impl From<SpawnRequest> for Launch {
    fn from(r: SpawnRequest) -> Launch {
        Launch {
            file: r.file,
            args: r.args,
            cwd: r.cwd,
            env: r.env,
        }
    }
}

impl Launch {
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.file);
        match &self.args {
            Value::Array(list) => {
                cmd.args(list.iter().filter_map(Value::as_str));
            }
            Value::String(line) => {
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.raw_arg(line);
                }
                #[cfg(not(windows))]
                cmd.arg(line);
            }
            _ => {}
        }
        cmd.current_dir(&self.cwd).env_clear().envs(&self.env);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        cmd
    }
}

struct Run {
    child: Arc<Mutex<Option<Child>>>,
    stdin: Mutex<Option<ChildStdin>>,
    cancelled: AtomicBool,
}

#[derive(Default)]
pub struct Logins {
    runs: Mutex<HashMap<String, Arc<Run>>>,
}

/// Runs a command to its end (15 s at most): its stdout, and its stderr after it.
fn run_status(launch: &Launch, stderr: bool) -> String {
    let mut cmd = launch.command();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Ok(mut child) = cmd.spawn() else {
        return String::new();
    };
    let read = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut s);
            }
            String::from_utf8_lossy(&s).into_owned()
        })
    };
    let out = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            _ => break,
        }
    }
    let mut text = out.join().unwrap_or_default();
    if stderr {
        text.push('\n');
        text.push_str(&err.join().unwrap_or_default());
    }
    text
}

impl Logins {
    pub fn start(
        self: &Arc<Self>,
        app: &AppHandle,
        account_id: &str,
        login: Launch,
        status: Launch,
        flow: LoginFlow,
    ) {
        self.cancel(account_id);
        let mut cmd = login.command();
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let spawned = cmd.spawn();
        let run = Arc::new(Run {
            child: Arc::new(Mutex::new(None)),
            stdin: Mutex::new(None),
            cancelled: AtomicBool::new(false),
        });
        self.runs
            .lock()
            .unwrap()
            .insert(account_id.into(), run.clone());
        let flow = Arc::new(flow);
        let out = Arc::new(Mutex::new((String::new(), false)));
        let mut readers = Vec::new();
        let error = match spawned {
            Ok(mut child) => {
                *run.stdin.lock().unwrap() = child.stdin.take();
                let pipes: [Option<Box<dyn Read + Send>>; 2] = [
                    child.stdout.take().map(|p| Box::new(p) as _),
                    child.stderr.take().map(|p| Box::new(p) as _),
                ];
                *run.child.lock().unwrap() = Some(child);
                for mut pipe in pipes.into_iter().flatten() {
                    let (out, flow, app, id) = (
                        out.clone(),
                        flow.clone(),
                        app.clone(),
                        account_id.to_string(),
                    );
                    readers.push(std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = pipe.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            let mut o = out.lock().unwrap();
                            if o.0.len() < OUTPUT_MAX {
                                o.0.push_str(&String::from_utf8_lossy(&buf[..n]));
                            }
                            if o.1 {
                                continue;
                            }
                            let url = login_url(&o.0, flow.hosts);
                            // A device sign-in shows its code after the link: wait for both.
                            let code = flow.device_code(&o.0);
                            if let (Some(url), true) = (url, code.is_some() || !flow.device) {
                                o.1 = true;
                                let mut e = json!({ "accountId": id, "state": "url", "url": url });
                                if let Some(code) = code {
                                    e["deviceCode"] = code.into();
                                }
                                crate::ipc::emit(&app, "login:event", e);
                            }
                        }
                    }));
                }
                None
            }
            Err(e) => Some(format!("Could not run {}: {e}", login.file)),
        };
        let (logins, app, id) = (self.clone(), app.clone(), account_id.to_string());
        std::thread::spawn(move || {
            // Waits for the process to end, then for its last output (2 s at most: a helper
            // it left running can hold a pipe open).
            loop {
                let done = match run.child.lock().unwrap().as_mut() {
                    Some(c) => !matches!(c.try_wait(), Ok(None)),
                    None => true,
                };
                if done {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            let until = Instant::now() + Duration::from_secs(2);
            while readers.iter().any(|r| !r.is_finished()) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            {
                let mut runs = logins.runs.lock().unwrap();
                if runs.get(&id).is_some_and(|r| Arc::ptr_eq(r, &run)) {
                    runs.remove(&id);
                }
            }
            if run.cancelled.load(Ordering::Relaxed) {
                return;
            }
            let text = run_status(&status, flow.status_stderr);
            let s = flow.parse_status(&text);
            let all = out.lock().unwrap().0.clone();
            // claude's last line, without its code prompt (printed with no line break after it).
            let last = all.trim().lines().last().unwrap_or("").to_string();
            let last = match last.find("prompted >") {
                Some(i) => last[i + "prompted >".len()..].trim().to_string(),
                None => last.trim().to_string(),
            };
            let error = if s.logged_in {
                Value::Null
            } else {
                error
                    .filter(|e| !e.is_empty())
                    .or(Some(last).filter(|l| !l.is_empty()))
                    .unwrap_or_else(|| "Sign-in did not finish.".into())
                    .into()
            };
            crate::ipc::emit(
                &app,
                "login:event",
                json!({
                    "accountId": id,
                    "state": "done",
                    "ok": s.logged_in,
                    "email": s.email,
                    "plan": s.plan,
                    "error": error,
                }),
            );
        });
    }

    /// The code the sign-in page showed, typed into claude's prompt.
    pub fn code(&self, account_id: &str, code: &str) -> bool {
        let Some(run) = self.runs.lock().unwrap().get(account_id).cloned() else {
            return false;
        };
        let mut stdin = run.stdin.lock().unwrap();
        let Some(stdin) = stdin.as_mut() else {
            return false;
        };
        stdin.write_all(format!("{code}\n").as_bytes()).is_ok() && stdin.flush().is_ok()
    }

    pub fn cancel(&self, account_id: &str) {
        let Some(run) = self.runs.lock().unwrap().remove(account_id) else {
            return;
        };
        run.cancelled.store(true, Ordering::Relaxed);
        let mut child = run.child.lock().unwrap();
        if let Some(child) = child.as_mut() {
            let _ = child.kill();
        }
    }

    pub fn stop_all(&self) {
        let ids: Vec<String> = self.runs.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.cancel(&id);
        }
    }
}
