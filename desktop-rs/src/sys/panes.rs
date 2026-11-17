//! Terminal panes live in the background pty host (ADR 0001), so they outlive the window:
//! this computer's host (`host-rs`, through core's `PaneHost`), and one host per machine
//! reached over ssh (H5, the remote crate). Like `PaneRouter` in pane-router.ts, each pane's
//! work goes to the host of its project's machine. Host output goes to the renderer as
//! `pty:data` / `pty:exit`, batched per frame: a busy pane makes many small frames, and each
//! Tauri event is a script evaluation in the webview.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::AppHandle;
use wraithgrid_core::config::{Config, Machine};
use wraithgrid_core::host::client::{
    connect_socket, HostEvent, MismatchChoice, PaneHost, PaneHostDeps,
};
use wraithgrid_core::host::paths::current_host_paths;
use wraithgrid_core::host::start::{host_binary, stable_host_binary, start_host_process};
use wraithgrid_core::paths::{current_user_data_dir, home_dir};
use wraithgrid_remote::{ssh_program, BinaryFor, Probe, RemoteHost, RemoteHostDeps, SystemSsh};

use super::native::{message_box, MessageBox};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long output is gathered before it goes out: about one frame.
const BATCH_WINDOW: Duration = Duration::from_millis(6);
/// A pane's batched output is cut at this size, so one event never grows without end.
const BATCH_MAX: usize = 512 * 1024;

pub type ConfigFn = Arc<dyn Fn() -> Config + Send + Sync>;

// ---- Output to the renderer -----------------------------------------------------------

enum Out {
    Data {
        pane_id: String,
        data: String,
        replay: bool,
    },
    Exit(Value),
}

/// Gathers host output for a frame and sends it as few events as it can: a pane's
/// consecutive data frames are joined (live output and a replay never are). Exits go
/// through the same queue, so they always come after the pane's last output.
struct Batcher {
    app: AppHandle,
    queue: Mutex<Vec<Out>>,
    ready: Condvar,
    /// Held while sending, so a flush from `attach` and the frame thread keep the order.
    sending: Mutex<()>,
}

impl Batcher {
    fn new(app: AppHandle) -> Arc<Batcher> {
        let b = Arc::new(Batcher {
            app,
            queue: Mutex::new(Vec::new()),
            ready: Condvar::new(),
            sending: Mutex::new(()),
        });
        let worker = b.clone();
        std::thread::Builder::new()
            .name("pty-output".into())
            .spawn(move || loop {
                {
                    let q = worker.queue.lock().unwrap();
                    drop(worker.ready.wait_while(q, |q| q.is_empty()).unwrap());
                }
                std::thread::sleep(BATCH_WINDOW);
                worker.flush();
            })
            .expect("pty output thread");
        b
    }

    fn push(&self, out: Out) {
        let mut q = self.queue.lock().unwrap();
        let was_empty = q.is_empty();
        match (&out, q.last_mut()) {
            (
                Out::Data {
                    pane_id,
                    data,
                    replay,
                },
                Some(Out::Data {
                    pane_id: last_id,
                    data: last,
                    replay: last_replay,
                }),
            ) if last_id == pane_id && last_replay == replay && last.len() < BATCH_MAX => {
                last.push_str(data);
            }
            _ => q.push(out),
        }
        if was_empty {
            self.ready.notify_one();
        }
    }

    /// Sends everything queued now.
    fn flush(&self) {
        let _sending = self.sending.lock().unwrap();
        let batch = std::mem::take(&mut *self.queue.lock().unwrap());
        for out in batch {
            match out {
                Out::Data {
                    pane_id,
                    data,
                    replay,
                } => crate::ipc::emit(
                    &self.app,
                    "pty:data",
                    json!({ "paneId": pane_id, "data": data, "replay": replay }),
                ),
                Out::Exit(e) => crate::ipc::emit(&self.app, "pty:exit", e),
            }
        }
    }
}

// ---- `wraithgrid new|split|sent` (H6): the host relays them, the window owns the layout -

type CliReply = mpsc::Sender<Value>;

#[derive(Default)]
struct CliRelay {
    next: AtomicU64,
    pending: Mutex<HashMap<u64, CliReply>>,
}

fn fail(error: impl Into<String>) -> Value {
    json!({ "ok": false, "error": error.into() })
}

/// `cliActionSchema` with its defaults filled in, or None when invalid.
pub fn parse_cli_action(raw: &Value) -> Option<Value> {
    use super::args::{arg_list, is_id};
    let s = |key: &str, max: usize| -> Option<Option<String>> {
        match raw.get(key) {
            None | Some(Value::Null) => Some(None),
            Some(v) => v
                .as_str()
                .filter(|s| s.encode_utf16().count() <= max)
                .map(|s| Some(s.to_string())),
        }
    };
    let cwd = || -> Option<Option<String>> {
        let c = s("cwd", 4096)?;
        match c {
            Some(c) if c.is_empty() || c.contains('\0') => None,
            c => Some(c),
        }
    };
    let pane_id = || {
        raw.get("paneId")?
            .as_str()
            .filter(|p| is_id(p))
            .map(str::to_string)
    };
    let mut out = serde_json::Map::new();
    let put = |out: &mut serde_json::Map<String, Value>, k: &str, v: Option<String>| {
        if let Some(v) = v {
            out.insert(k.into(), Value::String(v));
        }
    };
    match raw.get("kind")?.as_str()? {
        "new" => {
            out.insert("kind".into(), "new".into());
            put(&mut out, "account", s("account", 200)?);
            put(&mut out, "cwd", cwd()?);
            let shell = match raw.get("shell") {
                None | Some(Value::Null) => false,
                Some(v) => v.as_bool()?,
            };
            out.insert("shell".into(), shell.into());
            let args = match raw.get("args") {
                None | Some(Value::Null) => vec![],
                Some(v) => arg_list(v)?,
            };
            out.insert("args".into(), json!(args));
            put(&mut out, "workspace", s("workspace", 200)?);
        }
        "split" => {
            out.insert("kind".into(), "split".into());
            out.insert("paneId".into(), pane_id()?.into());
            put(&mut out, "account", s("account", 200)?);
            put(&mut out, "cwd", cwd()?);
            match raw.get("shell") {
                None | Some(Value::Null) => {}
                Some(v) => {
                    out.insert("shell".into(), v.as_bool()?.into());
                }
            }
            match raw.get("args") {
                None | Some(Value::Null) => {}
                Some(v) => {
                    out.insert("args".into(), json!(arg_list(v)?));
                }
            }
        }
        "sent" => {
            out.insert("kind".into(), "sent".into());
            out.insert("paneId".into(), pane_id()?.into());
        }
        _ => return None,
    }
    Some(Value::Object(out))
}

/// A `cli:result` from the renderer: `CliActionResult`, checked like `cliResultArgs`.
pub fn parse_cli_result(raw: &Value) -> Option<(u64, Value)> {
    let id = raw.get("id")?.as_u64()?;
    let r = raw.get("result")?;
    let result = match r.get("ok")?.as_bool()? {
        true => {
            let pane = r
                .get("paneId")?
                .as_str()
                .filter(|p| super::args::is_id(p))?;
            json!({ "ok": true, "paneId": pane })
        }
        false => {
            let e = r
                .get("error")?
                .as_str()
                .filter(|e| e.encode_utf16().count() <= 500)?;
            fail(e)
        }
    };
    Some((id, result))
}

// ---- The hosts --------------------------------------------------------------------------

pub struct Panes {
    app: AppHandle,
    config: ConfigFn,
    local: Arc<PaneHost>,
    remotes: Mutex<HashMap<String, (String, Arc<RemoteHost>)>>,
    out: Arc<Batcher>,
    cli: CliRelay,
}

/// A host of another protocol still runs agents: restart them, or leave them running.
fn ask_mismatch(app: &AppHandle, version: &str, live: usize) -> MismatchChoice {
    let message = format!(
        "{live} agent{} running in an older background host",
        if live == 1 { " is" } else { "s are" }
    );
    let detail = format!(
        "They were started by Wraithgrid {version}. Restart them now to see them here (panes that had a conversation continue it), or keep them running and quit this window."
    );
    let a = message_box(
        app,
        MessageBox {
            message: &message,
            detail: &detail,
            buttons: &["Restart them now", "Keep them running"],
            default: 0,
            cancel: 1,
            checkbox: None,
        },
    );
    if a.button == 0 {
        MismatchChoice::Restart
    } else {
        MismatchChoice::Keep
    }
}

/// `{ paneId, exitCode, signal, error?, disconnected? }` of `PtyExitEvent`.
fn exit_event(
    pane_id: String,
    exit_code: i64,
    signal: Option<i64>,
    error: Option<String>,
    remote: bool,
) -> Value {
    let mut e = json!({ "paneId": pane_id, "exitCode": exit_code, "signal": signal });
    if let Some(error) = error {
        e["error"] = error.into();
        // A machine's link dropped: its process may still run there (H5).
        if remote {
            e["disconnected"] = true.into();
        }
    }
    e
}

/// This build's static hosts for machines (`remoteHostBinary`): `remote/` beside the host.
fn remote_binary() -> Arc<BinaryFor> {
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = host_binary(&exe, |k| std::env::var(k).ok())
        .and_then(|b| b.parent().map(|d| d.join("remote")));
    Arc::new(move |target: &str| {
        let dir = dir.as_ref()?;
        std::fs::read(dir.join(format!("wraithgrid-host-{target}"))).ok()
    })
}

impl Panes {
    pub fn new(app: &AppHandle, config: ConfigFn) -> Arc<Panes> {
        let out = Batcher::new(app.clone());
        let paths = current_host_paths(&current_user_data_dir());
        let exe = std::env::current_exe().unwrap_or_default();
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".cache"))
            .join("wraithgrid");
        let appdir = std::env::var("APPDIR").ok();
        let bin = host_binary(&exe, |k| std::env::var(k).ok())
            .map(|b| stable_host_binary(&b, appdir.as_deref(), &cache, APP_VERSION));
        let (connect_paths, start_paths, token_paths) =
            (paths.clone(), paths.clone(), paths.clone());
        let events = {
            let (app, out) = (app.clone(), out.clone());
            Arc::new(move |e: HostEvent| match e {
                HostEvent::Data {
                    pane_id,
                    data,
                    replay,
                } => out.push(Out::Data {
                    pane_id,
                    data,
                    replay,
                }),
                HostEvent::Exit {
                    pane_id,
                    exit_code,
                    signal,
                    error,
                } => out.push(Out::Exit(exit_event(
                    pane_id, exit_code, signal, error, false,
                ))),
                HostEvent::Gui { id, action } => {
                    let app = app.clone();
                    std::thread::spawn(move || {
                        use tauri::Manager;
                        let Some(panes) = app.try_state::<Arc<Panes>>() else {
                            return;
                        };
                        let reply = panes.run_cli_action(&action);
                        panes.local.gui_reply(id, reply);
                    });
                }
            })
        };
        let mismatch_app = app.clone();
        let local = PaneHost::new(PaneHostDeps {
            connect: Box::new(move || connect_socket(&connect_paths.socket)),
            start: Box::new(move || {
                // The host inherits this environment: with the login shell's PATH known first.
                super::env::process_path();
                start_host_process(&start_paths, APP_VERSION, bin.as_deref(), &home_dir())
            }),
            read_token: Box::new(move || std::fs::read_to_string(&token_paths.token)),
            app_version: APP_VERSION.into(),
            on_event: events,
            on_mismatch: Box::new(move |v, live| ask_mismatch(&mismatch_app, v, live)),
            start_timeout: Duration::from_secs(10),
            hello_timeout: Duration::from_secs(5),
        });
        Arc::new(Panes {
            app: app.clone(),
            config,
            local,
            remotes: Mutex::default(),
            out,
            cli: CliRelay::default(),
        })
    }

    pub fn config(&self) -> Config {
        (self.config)()
    }

    pub fn local(&self) -> &Arc<PaneHost> {
        &self.local
    }

    fn remote_host(&self, m: &Machine) -> RemoteHost {
        let out = self.out.clone();
        let mismatch_app = self.app.clone();
        RemoteHost::new(RemoteHostDeps {
            machine: m.clone(),
            app_version: APP_VERSION.into(),
            program: ssh_program(|k| std::env::var(k).ok()),
            ssh: Arc::new(SystemSsh),
            binary: remote_binary(),
            on_event: Arc::new(move |e| match e {
                HostEvent::Data {
                    pane_id,
                    data,
                    replay,
                } => out.push(Out::Data {
                    pane_id,
                    data,
                    replay,
                }),
                HostEvent::Exit {
                    pane_id,
                    exit_code,
                    signal,
                    error,
                } => out.push(Out::Exit(exit_event(
                    pane_id, exit_code, signal, error, true,
                ))),
                // Commands run on a machine have no window there.
                HostEvent::Gui { .. } => {}
            }),
            on_mismatch: Box::new(move |v, live| ask_mismatch(&mismatch_app, v, live)),
            log: Arc::new(|m| eprintln!("[wraithgrid] machine {m}")),
        })
    }

    /// The machine's host, made the first time (again when its ssh host changed; the old
    /// link is dropped, its agents keep running).
    pub fn remote_for(&self, m: &Machine) -> Arc<RemoteHost> {
        let mut remotes = self.remotes.lock().unwrap();
        if let Some((ssh_host, r)) = remotes.get(&m.id) {
            if *ssh_host == m.ssh_host {
                return r.clone();
            }
            r.detach();
        }
        let r = Arc::new(self.remote_host(m));
        remotes.insert(m.id.clone(), (m.ssh_host.clone(), r.clone()));
        r
    }

    /// The machine id of a pane's project (`machineOfPane`); None: this computer.
    pub fn machine_id_of(cfg: &Config, pane_id: &str) -> Option<String> {
        cfg.workspaces
            .iter()
            .find(|w| w.panes.iter().any(|p| p.id == pane_id))?
            .machine_id
            .clone()
    }

    /// The host a pane lives in; None when its project's machine was removed.
    pub fn host_of(&self, pane_id: &str) -> Option<Arc<PaneHost>> {
        let cfg = self.config();
        match Self::machine_id_of(&cfg, pane_id) {
            None => Some(self.local.clone()),
            Some(id) => {
                let m = cfg.machines.iter().find(|m| m.id == id)?;
                Some(self.remote_for(m).pane_host())
            }
        }
    }

    /// Sends what is queued for the renderer now (a reattach's screen, before its answer).
    pub fn flush_output(&self) {
        self.out.flush();
    }

    /// Connects to a machine once, to show in Settings whether it works.
    pub fn probe(&self, m: &Machine) -> Result<Probe, String> {
        let host = self.remote_host(m);
        let r = host.probe();
        host.detach();
        r
    }

    /// Leaves every machine's agents running: quitting here never stops another computer's.
    pub fn detach_remotes(&self) {
        for (_, (_, r)) in self.remotes.lock().unwrap().drain() {
            r.detach();
        }
    }

    /// Carries out a relayed `wraithgrid new|split|sent` in the window (`runCliAction`).
    pub fn run_cli_action(&self, raw: &Value) -> Value {
        use tauri::Manager;
        let Some(action) = parse_cli_action(raw) else {
            return fail("Invalid command");
        };
        if let Some(cwd) = action.get("cwd").and_then(Value::as_str) {
            let dir = wraithgrid_core::paths::resolve_user_path(cwd, &home_dir());
            match std::fs::metadata(&dir) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return fail(format!("Not a folder: {cwd}")),
                Err(_) => return fail(format!("Folder not found: {cwd}")),
            }
        }
        if self.app.get_webview_window("main").is_none() {
            return fail("The Wraithgrid window is closed. Open it to add panes.");
        }
        let id = self.cli.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::channel();
        self.cli.pending.lock().unwrap().insert(id, tx);
        crate::ipc::emit(
            &self.app,
            "cli:action",
            json!({ "id": id, "action": action }),
        );
        let reply = rx.recv_timeout(Duration::from_secs(20));
        self.cli.pending.lock().unwrap().remove(&id);
        reply.unwrap_or_else(|_| fail("The Wraithgrid window did not answer."))
    }

    /// The window's answer to a relayed command (`cli:result`).
    pub fn cli_result(&self, id: u64, result: Value) {
        if let Some(tx) = self.cli.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(result);
        }
    }
}
