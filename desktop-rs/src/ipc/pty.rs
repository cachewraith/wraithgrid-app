//! Terminal panes: `pty:create` / `write` / `resize` / `kill`, with `pty:data` and `pty:exit`
//! going back (ipc.ts's PTY section). The processes run in the background pty host
//! (sys/panes.rs), so a pane reattaches with its screen after an app restart.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::pane_env::Os;
use wraithgrid_core::paths::home_dir;
use wraithgrid_core::spawn::{resolve_spawn, ResolveContext, SpawnSpec};
use wraithgrid_mods::{SharedBrains, SharedMode};

use super::{config, fail};
use crate::sys::args;
use crate::sys::panes::Panes;

/// One brain per provider (~/.wraithgrid/shared/<cli>), seeded once per run (`sharedFor`).
pub struct Shared(pub SharedBrains);

pub fn setup(app: &AppHandle) {
    crate::sys::env::start_probe();
    let cfg_app = app.clone();
    let config: crate::sys::panes::ConfigFn = Arc::new(move || {
        cfg_app
            .state::<config::Store>()
            .0
            .lock()
            .unwrap()
            .get()
            .clone()
    });
    let panes = Panes::new(app, config);
    app.manage(panes.clone());
    app.manage(Shared(SharedBrains::new(home_dir())));
    // `wraithgrid` commands may connect only with settings.cliAccess; the host is joined now
    // when they may, so they work before any pane exists. Followed as the config changes.
    std::thread::spawn(move || {
        let mut last: Option<bool> = None;
        loop {
            let on = panes.config().settings.cli_access;
            if last != Some(on) {
                panes.local().set_cli_access(on);
                if on {
                    let _ = panes.local().connect();
                }
                last = Some(on);
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

pub fn shared_mode(cfg: &wraithgrid_core::config::Config) -> SharedMode {
    match cfg.settings.shared_mode {
        wraithgrid_core::types::SharedMode::Overall => SharedMode::Overall,
        wraithgrid_core::types::SharedMode::PerAccount => SharedMode::PerAccount,
    }
}

pub fn mods_account(a: &wraithgrid_core::config::Account) -> wraithgrid_mods::Account {
    wraithgrid_mods::Account {
        id: a.id.clone(),
        name: a.name.clone(),
        cli: a.cli.as_str().into(),
        config_dir: a.config_dir.clone(),
    }
}

/// Links the provider's shared instructions and skills into an account before it starts,
/// when sharing is overall (`syncShared`). Problems are logged, never raised.
pub fn sync_shared(app: &AppHandle, cfg: &wraithgrid_core::config::Config, account_id: &str) {
    if let Some(a) = cfg.accounts.iter().find(|a| a.id == account_id) {
        app.state::<Shared>()
            .0
            .sync_shared(shared_mode(cfg), &mods_account(a), &|m| {
                eprintln!("[wraithgrid] {m}")
            });
    }
}

/// Resolves what to run on this computer (`resolveSpawn` with the user's overrides). Waits
/// for the login shell's PATH: call off the async workers.
pub fn resolve_local(
    cfg: &wraithgrid_core::config::Config,
    spec: &SpawnSpec,
) -> Result<wraithgrid_core::host::protocol::SpawnRequest, String> {
    let env = crate::sys::env::spawn_env();
    let home = home_dir();
    let s = &cfg.settings;
    resolve_spawn(
        spec,
        &ResolveContext {
            os: Os::current(),
            env: &env,
            home: &home,
            claude_path: &cfg.claude_path,
            cli_paths: &s.cli_paths,
            shell_path: &s.shell_path,
            shell_args: &s.shell_args,
            mod_dirs: &s.mod_dirs,
        },
    )
}

struct CreateArgs {
    pane_id: String,
    account_id: Option<String>,
    cwd: String,
    args: Vec<String>,
    shell: bool,
    resume: bool,
    attach: bool,
    cols: u16,
    rows: u16,
}

/// `ptyCreateArgs`.
fn create_args(p: &Value) -> Option<CreateArgs> {
    Some(CreateArgs {
        pane_id: args::id(p, "paneId")?,
        account_id: match p.get("accountId")? {
            Value::Null => None,
            _ => Some(args::id(p, "accountId")?),
        },
        cwd: args::string(p, "cwd", 1, 4096)?,
        args: args::arg_list(p.get("args")?)?,
        shell: p.get("shell")?.as_bool()?,
        resume: args::bool_or(p, "resume", false)?,
        attach: args::bool_or(p, "attach", false)?,
        cols: args::dim(p, "cols")?,
        rows: args::dim(p, "rows")?,
    })
}

fn create(app: &AppHandle, a: CreateArgs) -> Value {
    let panes = app.state::<Arc<Panes>>();
    if a.attach {
        if let Some(host) = panes.host_of(&a.pane_id) {
            if let Some(kept) = host.attach(&a.pane_id, a.cols, a.rows) {
                // Its screen came in ahead of the reply: it goes out before the answer.
                panes.flush_output();
                let exit = kept
                    .exit
                    .map(|e| json!({ "exitCode": e.exit_code, "signal": e.signal }));
                return json!({ "ok": true, "pid": kept.pid, "exit": exit });
            }
        }
    }
    let cfg = panes.config();
    let (mut config_dir, mut cli) = (None, AgentCli::Claude);
    if !a.shell {
        let Some(account) = a
            .account_id
            .as_deref()
            .and_then(|id| cfg.accounts.iter().find(|x| x.id == id))
        else {
            return fail("This pane has no account. Pick one in the New pane dialog.");
        };
        config_dir = Some(account.config_dir.clone());
        cli = account.cli;
    }
    let spec = SpawnSpec {
        pane_id: a.pane_id.clone(),
        cli,
        shell: a.shell,
        config_dir,
        cwd: a.cwd,
        args: a.args,
        resume: a.resume,
        cols: a.cols,
        rows: a.rows,
    };
    let started = match Panes::machine_id_of(&cfg, &a.pane_id) {
        // A remote project's panes are resolved and started on that machine (H5).
        Some(id) => match cfg.machines.iter().find(|m| m.id == id) {
            Some(m) => panes.remote_for(m).spawn(&spec),
            None => Err("This workspace’s machine was removed.".into()),
        },
        None => {
            if let (false, Some(account)) = (a.shell, a.account_id.as_deref()) {
                sync_shared(app, &cfg, account);
            }
            resolve_local(&cfg, &spec).and_then(|req| panes.local().create(req))
        }
    };
    match started {
        Ok(pid) => json!({ "ok": true, "pid": pid }),
        Err(e) => fail(e),
    }
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    Some(Ok(match channel {
        "pty:create" => {
            let Some(a) = create_args(&payload) else {
                eprintln!("[wraithgrid] refused pty:create: invalid arguments");
                return Some(Ok(fail("Invalid request")));
            };
            let app = app.clone();
            tauri::async_runtime::spawn_blocking(move || create(&app, a))
                .await
                .unwrap_or_else(|e| fail(e.to_string()))
        }
        // Fire and forget, in order: these are quick writes to the host's socket.
        "pty:write" => {
            let pane = args::id(&payload, "paneId");
            let data = payload
                .get("data")
                .and_then(Value::as_str)
                .filter(|d| d.encode_utf16().count() <= 1_000_000);
            if let (Some(pane), Some(data)) = (pane, data) {
                if let Some(host) = app.state::<Arc<Panes>>().host_of(&pane) {
                    host.write(&pane, data);
                }
            }
            Value::Null
        }
        "pty:resize" => {
            let pane = args::id(&payload, "paneId");
            let size = args::dim(&payload, "cols").zip(args::dim(&payload, "rows"));
            if let (Some(pane), Some((cols, rows))) = (pane, size) {
                if let Some(host) = app.state::<Arc<Panes>>().host_of(&pane) {
                    host.resize(&pane, cols, rows);
                }
            }
            Value::Null
        }
        "pty:kill" => {
            if let Some(pane) = args::id(&payload, "paneId") {
                if let Some(host) = app.state::<Arc<Panes>>().host_of(&pane) {
                    host.kill(&pane);
                }
            }
            Value::Null
        }
        _ => return None,
    }))
}
