//! Chat panes (ADR 0002), the chat handlers of `src/main/ipc.ts`: one headless CLI process per
//! pane (wraithgrid-chat's `ChatSession` and adapters, checked event for event against
//! `src/main/chat/*.ts`), its events sent to the window as `chat:event` `{ paneId, event }`
//! (the same `ChatEvent` shapes as src/shared/chat.ts), the saved chat views
//! (`<user data>/chats/<paneId>.json`, shared with the Electron and GPUI apps), the attach
//! dialog and the `@` file menu.
//!
//! Other groups can listen to `chat:event` on the Rust side too (`app.listen`): the usage
//! store records `turnEnd` usage and `limits` from it, as `usage.onChatEvent` did. Before the
//! app quits, [`shutdown`] has the window save its views and stops every chat's CLI.

mod env;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, WindowEvent};
use wraithgrid_chat::files::{rank_paths, system_git, FileLister, FILE_MENTIONS_MAX};
use wraithgrid_chat::transcript::{copy_codex_rollout, copy_transcript};
use wraithgrid_chat::views::{delete_view, load_view, save_view};
use wraithgrid_chat::{
    chat_adapter_for, is_valid_model, is_valid_session_id, ApprovalDecision, ArgsOptions,
    ChatAnswers, ChatImage, ChatLaunch, ChatMode, ChatSession, CHAT_EFFORTS, CHAT_SAVE_MAX,
};
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::config::Config;
use wraithgrid_core::pane_env::Os;
use wraithgrid_core::paths::{accounts_root, current_user_data_dir, home_dir, resolve_user_path};
use wraithgrid_core::spawn::{resolve_spawn, ResolveContext, SpawnSpec};
use wraithgrid_mods::{Account, SharedBrains, SharedMode};

use super::files::{pick, Pick};
use super::{emit, fail, ok};

/// How long a start waits for the login shell's PATH before it looks with what it has.
const ENV_WAIT: Duration = Duration::from_secs(5);
/// How long a flush waits for the window to say its views are saved (`flushChats`).
const FLUSH_WAIT: Duration = Duration::from_millis(1500);
/// How long a flush waits for the saves already sent to reach the disk.
const WRITES_WAIT: Duration = Duration::from_secs(3);
/// git's output for the `@` menu, past which the list is cut.
const GIT_MAX: usize = 32 << 20;

pub struct Chats {
    sessions: Mutex<HashMap<String, ChatSession>>,
    /// Bumped by every start and stop of a pane: a start still waiting on PATH or the shared
    /// brain when a newer start or a stop came in spawns nothing (#135).
    gens: Mutex<HashMap<String, u64>>,
    /// Saves sent and not on disk yet.
    writes: AtomicUsize,
    writes_done: tokio::sync::Notify,
    /// The window's answer to `chat:flush`.
    flushed: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    lister: Arc<FileLister>,
    brains: Arc<SharedBrains>,
    dir: PathBuf,
}

impl Chats {
    fn bump(&self, pane_id: &str) -> u64 {
        let mut gens = self.gens.lock().unwrap();
        let gen = gens.entry(pane_id.to_string()).or_default();
        *gen += 1;
        *gen
    }

    fn gen(&self, pane_id: &str) -> u64 {
        self.gens.lock().unwrap().get(pane_id).copied().unwrap_or(0)
    }

    fn running(&self, pane_id: &str) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(pane_id)
            .is_some_and(ChatSession::is_running)
    }

    fn with<T>(&self, pane_id: &str, f: impl FnOnce(&ChatSession) -> T) -> Option<T> {
        self.sessions.lock().unwrap().get(pane_id).map(f)
    }

    /// Stops the pane's process (quietly: no exit event) and hands it back to wait on.
    fn stop(&self, pane_id: &str) -> Option<ChatSession> {
        let s = self.sessions.lock().unwrap().remove(pane_id)?;
        s.stop();
        Some(s)
    }
}

pub fn setup(app: &AppHandle) {
    env::start_probe();
    app.manage(Chats {
        sessions: Mutex::new(HashMap::new()),
        gens: Mutex::new(HashMap::new()),
        writes: AtomicUsize::new(0),
        writes_done: tokio::sync::Notify::new(),
        flushed: Mutex::new(None),
        lister: Arc::new(FileLister::new(system_git(GIT_MAX))),
        brains: Arc::new(SharedBrains::new(home_dir())),
        dir: current_user_data_dir().join("chats"),
    });
    // The window going away takes its chats' CLIs along (they lead their own process groups,
    // so nothing else would stop them); a close first has the window save its views.
    if let Some(win) = app.get_webview_window("main") {
        let app = app.clone();
        win.on_window_event(move |e| match e {
            WindowEvent::CloseRequested { .. } => emit(&app, "chat:flush", Value::Null),
            WindowEvent::Destroyed => stop_all(&app),
            _ => {}
        });
    }
}

/// Has the window save its chat views and waits for them on disk (quit, #136).
pub async fn flush(app: &AppHandle) {
    let chats = app.state::<Chats>();
    let (tx, rx) = tokio::sync::oneshot::channel();
    *chats.flushed.lock().unwrap() = Some(tx);
    emit(app, "chat:flush", Value::Null);
    let _ = tokio::time::timeout(FLUSH_WAIT, rx).await;
    chats.flushed.lock().unwrap().take();
    let _ = tokio::time::timeout(WRITES_WAIT, async {
        loop {
            let done = chats.writes_done.notified();
            if chats.writes.load(Ordering::SeqCst) == 0 {
                break;
            }
            done.await;
        }
    })
    .await;
}

/// Stops every chat's CLI (SIGTERM to its process group, SIGKILL after the grace time).
pub fn stop_all(app: &AppHandle) {
    let chats = app.state::<Chats>();
    let all: Vec<ChatSession> = chats
        .sessions
        .lock()
        .unwrap()
        .drain()
        .map(|(_, s)| s)
        .collect();
    for s in &all {
        s.stop();
    }
}

/// Chats mid-turn or with background work: what a quit would cut short (#137).
#[allow(dead_code)]
pub fn busy_count(app: &AppHandle) -> usize {
    let chats = app.state::<Chats>();
    let sessions = chats.sessions.lock().unwrap();
    sessions.values().filter(|s| s.is_busy()).count()
}

/// What a quit does with chats (`cleanup` in app.ts): views saved, then the CLIs stopped.
#[allow(dead_code)]
pub async fn shutdown(app: &AppHandle) {
    flush(app).await;
    stop_all(app);
}

// ---- Arguments (the zod schemas of ipc-contract.ts) ---------------------------------------

fn is_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn id(p: &Value, key: &str) -> Option<String> {
    p.get(key)
        .and_then(Value::as_str)
        .filter(|s| is_id(s))
        .map(String::from)
}

fn string(p: &Value, key: &str, min: usize, max: usize) -> Option<String> {
    p.get(key)
        .and_then(Value::as_str)
        .filter(|s| (min..=max).contains(&s.chars().count()))
        .map(String::from)
}

/// A nullable field: Ok(None) when null or missing, Err when present and invalid.
fn nullable<T>(p: &Value, key: &str, f: impl FnOnce(&str) -> Option<T>) -> Result<Option<T>, ()> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => f(s).map(Some).ok_or(()),
        _ => Err(()),
    }
}

struct StartArgs {
    pane_id: String,
    account_id: String,
    cwd: String,
    args: Vec<String>,
    options: ArgsOptions,
}

fn start_args(p: &Value) -> Option<StartArgs> {
    let args = match p.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) if a.len() <= 64 => a
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| s.chars().count() <= 4096)
                    .map(String::from)
            })
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    let continue_last = match p.get("continueLast") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        _ => return None,
    };
    Some(StartArgs {
        pane_id: id(p, "paneId")?,
        account_id: id(p, "accountId")?,
        cwd: string(p, "cwd", 1, 4096)?,
        args,
        options: ArgsOptions {
            session_id: nullable(p, "sessionId", |s| is_valid_session_id(s).then(|| s.into()))
                .ok()?,
            continue_last,
            model: nullable(p, "model", |s| is_valid_model(s).then(|| s.into())).ok()?,
            effort: nullable(p, "effort", |s| CHAT_EFFORTS.contains(&s).then(|| s.into())).ok()?,
            mode: nullable(p, "mode", ChatMode::parse).ok()?,
        },
    })
}

// ---- Config lookups ------------------------------------------------------------------------

fn config(app: &AppHandle) -> Config {
    app.state::<super::config::Store>()
        .0
        .lock()
        .unwrap()
        .get()
        .clone()
}

/// The machine a pane's workspace runs on (H5); None: this computer.
fn machine_of_pane(cfg: &Config, pane_id: &str) -> Option<String> {
    cfg.workspaces
        .iter()
        .find(|w| w.panes.iter().any(|p| p.id == pane_id))
        .and_then(|w| w.machine_id.clone())
}

/// The pane's folder from the config (never a renderer path).
fn pane_cwd(cfg: &Config, pane_id: &str) -> Option<PathBuf> {
    cfg.workspaces
        .iter()
        .flat_map(|w| &w.panes)
        .find(|p| p.id == pane_id)
        .map(|p| resolve_user_path(&p.cwd, &home_dir()))
}

/// Links its provider's shared instructions and skills into the account, if sharing is overall.
async fn sync_shared(app: &AppHandle, cfg: &Config, account_id: &str) {
    let Some(a) = cfg.accounts.iter().find(|a| a.id == account_id) else {
        return;
    };
    let mode = match cfg.settings.shared_mode {
        wraithgrid_core::types::SharedMode::Overall => SharedMode::Overall,
        wraithgrid_core::types::SharedMode::PerAccount => SharedMode::PerAccount,
    };
    let account = Account {
        id: a.id.clone(),
        name: a.name.clone(),
        cli: a.cli.as_str().into(),
        config_dir: a.config_dir.clone(),
    };
    let brains = app.state::<Chats>().brains.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        brains.sync_shared(mode, &account, &|m: &str| eprintln!("[wraithgrid] {m}"));
    })
    .await;
}

// ---- Handlers ------------------------------------------------------------------------------

async fn start(app: &AppHandle, p: &Value) -> Value {
    let Some(a) = start_args(p) else {
        return fail("Invalid request");
    };
    let chats = app.state::<Chats>();
    // The window reloaded (renderer crash): its process kept running.
    if chats.running(&a.pane_id) {
        return json!({ "ok": true, "pid": null, "attached": true });
    }
    let gen = chats.bump(&a.pane_id);
    let cfg = config(app);
    if machine_of_pane(&cfg, &a.pane_id).is_some() {
        return fail("Chat runs on this computer only. Switch this pane to Terminal.");
    }
    let Some(account) = cfg.accounts.iter().find(|x| x.id == a.account_id) else {
        return fail("This pane has no account. Pick one in the New pane dialog.");
    };
    let Some(adapter) = chat_adapter_for(account.cli.as_str()) else {
        return fail(format!(
            "{} has no chat view yet. Switch this pane to Terminal.",
            account.cli.as_str()
        ));
    };
    env::ready(ENV_WAIT).await;
    sync_shared(app, &cfg, &account.id).await;
    let env = env::env_now();
    let home = home_dir();
    let spec = SpawnSpec {
        pane_id: a.pane_id.clone(),
        cli: account.cli,
        shell: false,
        config_dir: Some(account.config_dir.clone()),
        cwd: a.cwd.clone(),
        args: a.args.clone(),
        resume: false,
        cols: 80,
        rows: 24,
    };
    let ctx = ResolveContext {
        os: Os::current(),
        env: &env,
        home: &home,
        claude_path: &cfg.claude_path,
        cli_paths: &cfg.settings.cli_paths,
        shell_path: &cfg.settings.shell_path,
        shell_args: &cfg.settings.shell_args,
        mod_dirs: &cfg.settings.mod_dirs,
    };
    // The pane's args with the CLI's own (codex: the CLAUDE.md project-doc fallback); the
    // adapter's (`-p --input-format stream-json …`, `app-server`) follow them.
    let req = match resolve_spawn(&spec, &ctx) {
        Ok(req) => req,
        Err(e) => return fail(e),
    };
    // Superseded: the newer start answers for the pane, a stop wanted none.
    if chats.gen(&a.pane_id) != gen {
        return json!({ "ok": true, "pid": null, "attached": true });
    }
    let Ok(args) = serde_json::from_value::<Vec<String>>(req.args) else {
        return fail("This CLI can't run as a chat here.");
    };
    let mut launch = ChatLaunch::new(req.file, req.cwd);
    launch.args = args;
    launch.env = req.env;
    launch.options = a.options;
    let sink_app = app.clone();
    let pane_id = a.pane_id.clone();
    let session = ChatSession::start_with(launch, adapter, move |event| {
        emit(
            &sink_app,
            "chat:event",
            json!({ "paneId": pane_id, "event": event }),
        );
    });
    match session {
        Ok(session) => {
            let pid = session.pid();
            // A start that raced another one (#135): the older process must not run on unseen.
            if let Some(old) = chats.sessions.lock().unwrap().insert(a.pane_id, session) {
                old.stop();
            }
            json!({ "ok": true, "pid": pid, "attached": false })
        }
        Err(e) => fail(e.to_string()),
    }
}

/// Switching a chat's account (#29, #60): the conversation file is copied into the other
/// account's dir; both accounts come from the config, use the same CLI (claude or codex) and
/// live in Wraithgrid's own folder.
async fn move_account(app: &AppHandle, p: &Value) -> Value {
    let (Some(pane_id), Some(to_id), Some(session_id)) = (
        id(p, "paneId"),
        id(p, "toAccountId"),
        p.get("sessionId")
            .and_then(Value::as_str)
            .filter(|s| is_valid_session_id(s)),
    ) else {
        return fail("Could not switch the account");
    };
    let cfg = config(app);
    let Some(pane) = cfg
        .workspaces
        .iter()
        .flat_map(|w| &w.panes)
        .find(|p| p.id == pane_id)
    else {
        return fail("That pane is gone.");
    };
    // The renderer stopped it already; make sure the process is gone before copying.
    if let Some(s) = app.state::<Chats>().stop(&pane_id) {
        let _ =
            tauri::async_runtime::spawn_blocking(move || s.wait_exit(Duration::from_secs(3))).await;
    }
    let from = pane
        .account_id
        .as_deref()
        .and_then(|id| cfg.accounts.iter().find(|a| a.id == id));
    let to = cfg.accounts.iter().find(|a| a.id == to_id);
    let (Some(from), Some(to)) = (from, to) else {
        return fail("That account is gone.");
    };
    if from.cli != to.cli || !from.cli.can_continue_as() {
        return fail("This chat can only switch to another account of the same CLI.");
    }
    let home = home_dir();
    let (from_dir, to_dir) = (
        resolve_user_path(&from.config_dir, &home),
        resolve_user_path(&to.config_dir, &home),
    );
    let codex = from.cli == AgentCli::Codex;
    let session_id = session_id.to_string();
    let copied = tauri::async_runtime::spawn_blocking(move || {
        let root = accounts_root(&home);
        if codex {
            copy_codex_rollout(&from_dir, &to_dir, &session_id, &root)
        } else {
            copy_transcript(&from_dir, &to_dir, &session_id, &root)
        }
    })
    .await;
    match copied {
        Ok(Ok(())) => ok(),
        Ok(Err(e)) => fail(e),
        Err(_) => fail("Could not switch the account"),
    }
}

fn save(app: &AppHandle, p: &Value) {
    let (Some(pane_id), Some(data)) = (
        id(p, "paneId"),
        p.get("data")
            .and_then(Value::as_str)
            .filter(|d| d.chars().count() <= CHAT_SAVE_MAX),
    ) else {
        return;
    };
    let data = data.to_string();
    let chats = app.state::<Chats>();
    chats.writes.fetch_add(1, Ordering::SeqCst);
    let app = app.clone();
    let dir = chats.dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = save_view(&dir, &pane_id, &data) {
            eprintln!("[wraithgrid] could not save chat {pane_id}: {e}");
        }
        let chats = app.state::<Chats>();
        chats.writes.fetch_sub(1, Ordering::SeqCst);
        chats.writes_done.notify_waiters();
    });
}

async fn list_files(app: &AppHandle, p: &Value) -> Value {
    let (Some(pane_id), Some(query)) = (id(p, "paneId"), string(p, "query", 0, 500)) else {
        return json!([]);
    };
    // The `@` menu (#159): files under the pane's own folder; chats on ssh machines get none.
    let cfg = config(app);
    if machine_of_pane(&cfg, &pane_id).is_some() {
        return json!([]);
    }
    let Some(cwd) = pane_cwd(&cfg, &pane_id) else {
        return json!([]);
    };
    env::ready(ENV_WAIT).await;
    let lister = app.state::<Chats>().lister.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let paths = lister.list(&cwd);
        Value::from(rank_paths(&paths, &query, FILE_MENTIONS_MAX))
    })
    .await
    .unwrap_or_else(|_| json!([]))
}

/// Attach files to a chat message: picked in a native dialog, starting in the pane's folder;
/// images are read here (the paths came from our own dialog), the rest go on as paths.
async fn pick_files(app: &AppHandle, p: &Value) -> Value {
    let Some(pane_id) = id(p, "paneId") else {
        return Value::Null;
    };
    let dir = pane_cwd(&config(app), &pane_id).unwrap_or_else(home_dir);
    let Some(paths) = pick(app, Pick::Files, Some("Attach files"), &dir).await else {
        return Value::Null;
    };
    tauri::async_runtime::spawn_blocking(move || {
        serde_json::to_value(wraithgrid_chat::attach::read_picked(&paths)).unwrap_or(Value::Null)
    })
    .await
    .unwrap_or(Value::Null)
}

fn chat_dir(app: &AppHandle) -> PathBuf {
    app.state::<Chats>().dir.clone()
}

fn load(dir: &Path, pane_id: &str) -> Value {
    load_view(dir, pane_id).map_or(Value::Null, Value::from)
}

pub async fn handle(app: &AppHandle, channel: &str, p: Value) -> Option<Result<Value, String>> {
    let chats = app.state::<Chats>();
    Some(Ok(match channel {
        "chat:start" => start(app, &p).await,
        "chat:moveAccount" => move_account(app, &p).await,
        "chat:send" => {
            let images = match p.get("images") {
                None | Some(Value::Null) => Some(Vec::new()),
                Some(v) => serde_json::from_value::<Vec<ChatImage>>(v.clone()).ok(),
            };
            if let (Some(pane_id), Some(text), Some(images)) = (
                id(&p, "paneId"),
                p.get("text").and_then(Value::as_str),
                images,
            ) {
                chats.with(&pane_id, |s| s.send(text, &images, &[]));
            }
            Value::Null
        }
        "chat:approve" => {
            let decision = p
                .get("decision")
                .and_then(|d| serde_json::from_value::<ApprovalDecision>(d.clone()).ok());
            let answers = match p.get("answers") {
                None | Some(Value::Null) => Some(None),
                Some(v) => serde_json::from_value::<ChatAnswers>(v.clone())
                    .ok()
                    .filter(|a| {
                        a.iter()
                            .all(|(k, v)| k.chars().count() <= 2000 && v.chars().count() <= 4000)
                    })
                    .map(Some),
            };
            if let (Some(pane_id), Some(req_id), Some(decision), Some(answers)) = (
                id(&p, "paneId"),
                string(&p, "reqId", 1, 200),
                decision,
                answers,
            ) {
                chats.with(&pane_id, |s| s.approve(&req_id, decision, answers));
            }
            Value::Null
        }
        "chat:interrupt" => {
            if let Some(pane_id) = id(&p, "paneId") {
                chats.with(&pane_id, ChatSession::interrupt);
            }
            Value::Null
        }
        "chat:setModel" => {
            if let (Some(pane_id), Some(model)) =
                (id(&p, "paneId"), p.get("model").and_then(Value::as_str))
            {
                chats.with(&pane_id, |s| s.set_model(model));
            }
            Value::Null
        }
        "chat:setMode" => {
            let mode = p
                .get("mode")
                .and_then(Value::as_str)
                .and_then(ChatMode::parse);
            if let (Some(pane_id), Some(mode)) = (id(&p, "paneId"), mode) {
                chats.with(&pane_id, |s| s.set_mode(mode));
            }
            Value::Null
        }
        "chat:stop" => {
            if let (Some(pane_id), Some(forget)) =
                (id(&p, "paneId"), p.get("forget").and_then(Value::as_bool))
            {
                chats.bump(&pane_id);
                chats.stop(&pane_id);
                if forget {
                    let dir = chat_dir(app);
                    tauri::async_runtime::spawn_blocking(move || {
                        let _ = delete_view(&dir, &pane_id);
                    });
                }
            }
            Value::Null
        }
        "chat:load" => match id(&p, "paneId") {
            Some(pane_id) => {
                let dir = chat_dir(app);
                tauri::async_runtime::spawn_blocking(move || load(&dir, &pane_id))
                    .await
                    .unwrap_or(Value::Null)
            }
            None => Value::Null,
        },
        "chat:save" => {
            save(app, &p);
            Value::Null
        }
        "chat:flushed" => {
            if let Some(tx) = chats.flushed.lock().unwrap().take() {
                let _ = tx.send(());
            }
            Value::Null
        }
        "chat:pickFiles" => pick_files(app, &p).await,
        "chat:listFiles" => list_files(app, &p).await,
        _ => return None,
    }))
}
