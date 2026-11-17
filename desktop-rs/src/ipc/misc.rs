//! The system channels of ipc.ts: sign-in (`login:*`), `claude:detect`, account config
//! dirs, `shared:apply`, `shell:list`, `clipboard:image`, `notify:pane`, machines (H5) and
//! the `wraithgrid` CLI's relayed commands (`cli:result`; `cli:action` goes out from
//! sys/panes.rs).

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use wraithgrid_core::agent_cli::{AgentCli, ImagePaste};
use wraithgrid_core::config::Machine;
use wraithgrid_core::login::login_flow;
use wraithgrid_core::pane_env::Os;
use wraithgrid_core::paths::{
    accounts_root, contract_home, home_dir, is_strictly_inside, resolve_user_path,
};
use wraithgrid_core::platform::{check_binary, find_on_path, list_shells};
use wraithgrid_core::spawn::SpawnSpec;
use wraithgrid_mods::SharedMode;

use super::pty::{mods_account, resolve_local, sync_shared, Shared};
use super::{fail, ok};
use crate::sys::args;
use crate::sys::login::{Launch, Logins};
use crate::sys::panes::Panes;

pub fn setup(app: &AppHandle) {
    app.manage(Arc::new(Logins::default()));
}

fn log(msg: &str) {
    eprintln!("[wraithgrid] {msg}");
}

/// Runs blocking work (processes, files, the PATH probe) off the async workers.
async fn blocking(f: impl FnOnce() -> Value + Send + 'static) -> Value {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| fail(e.to_string()))
}

fn env_lookup(
    env: &std::collections::BTreeMap<String, String>,
) -> impl Fn(&str) -> Option<String> + '_ {
    move |k: &str| env.get(k).cloned()
}

// ---- Sign-in -----------------------------------------------------------------------------

fn login_start(app: &AppHandle, account_id: &str) -> Value {
    let cfg = app.state::<Arc<Panes>>().config();
    let Some(account) = cfg.accounts.iter().find(|a| a.id == account_id) else {
        return fail("No such account.");
    };
    let Some(flow) = login_flow(account.cli) else {
        return fail(format!(
            "{} signs in from a terminal.",
            account.cli.as_str()
        ));
    };
    sync_shared(app, &cfg, &account.id);
    let launch = |args: &[&str]| {
        resolve_local(
            &cfg,
            &SpawnSpec {
                pane_id: format!("login-{}", account.id),
                cli: account.cli,
                shell: false,
                config_dir: Some(account.config_dir.clone()),
                cwd: "~".into(),
                args: args.iter().map(|a| a.to_string()).collect(),
                resume: false,
                cols: 80,
                rows: 24,
            },
        )
        .map(Launch::from)
    };
    let (login, status) = (launch(flow.login), launch(flow.status));
    let (login, status) = match (login, status) {
        (Ok(l), Ok(s)) => (l, s),
        (Err(e), _) | (_, Err(e)) => return fail(e),
    };
    app.state::<Arc<Logins>>()
        .start(app, &account.id, login, status, flow);
    ok()
}

/// `loginCodeArgs`: the code a sign-in page shows, printable with no spaces.
fn login_code(p: &Value) -> Option<(String, String)> {
    let code = p.get("code")?.as_str()?;
    let ok = (1..=2048).contains(&code.len()) && code.bytes().all(|b| (0x21..=0x7e).contains(&b));
    ok.then(|| Some((args::id(p, "accountId")?, code.to_string())))?
}

// ---- claude binary, account dirs, shared brains -----------------------------------------

fn claude_detect(app: &AppHandle, override_path: Option<String>) -> Value {
    let cfg = app.state::<Arc<Panes>>().config();
    let env = crate::sys::env::spawn_env();
    let over = override_path.unwrap_or(cfg.claude_path);
    let c = check_binary(
        "claude",
        &over,
        &home_dir(),
        &env_lookup(&env),
        Os::current(),
    );
    let source = match (&c.path, c.overridden) {
        (None, _) => Value::Null,
        (Some(_), true) => "override".into(),
        (Some(_), false) => "path".into(),
    };
    let mut r = json!({
        "path": c.path.map(|p| p.to_string_lossy().into_owned()),
        "source": source,
        "version": c.version,
        "ok": c.error.is_none(),
    });
    if let Some(e) = c.error {
        r["error"] = e.into();
    }
    r
}

fn create_dir(slug: &str) -> Value {
    let home = home_dir();
    let dir = accounts_root(&home).join(slug);
    let made = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(&dir)
    };
    match made {
        Ok(()) => json!({
            "ok": true,
            "dir": contract_home(&dir.to_string_lossy(), &home.to_string_lossy()),
        }),
        Err(e) => {
            log(&format!("account:createDir failed: {e}"));
            fail("Could not create the config dir")
        }
    }
}

fn delete_dir(app: &AppHandle, dir: &str) -> Value {
    let home = home_dir();
    let target = resolve_user_path(dir, &home);
    let cfg = app.state::<Arc<Panes>>().config();
    // Only a dir that a current account points at, and only inside the home dir.
    let Some(owner) = cfg
        .accounts
        .iter()
        .find(|a| resolve_user_path(&a.config_dir, &home) == target)
    else {
        return fail("That folder does not belong to an account");
    };
    if !is_strictly_inside(&target, &home) {
        return fail("Refusing to delete a folder outside your home directory");
    }
    // dunce: plain C:\ paths on Windows, so `unshare` matches the stored folder paths.
    let Ok(real) = dunce::canonicalize(&target) else {
        return ok(); // Already gone.
    };
    let real_home = dunce::canonicalize(&home).unwrap_or(home.clone());
    if !is_strictly_inside(&real, &real_home) {
        return fail("Refusing to delete a folder outside your home directory");
    }
    // Drop the shared links first so a recursive delete can never reach through them.
    app.state::<Shared>()
        .0
        .unshare(owner.cli.as_str(), &real.to_string_lossy());
    if let Err(e) = std::fs::remove_dir_all(&real) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log(&format!("account:deleteDir failed: {e}"));
            return fail("Could not delete the config dir");
        }
    }
    log(&format!(
        "deleted account config dir {} at the user's request",
        contract_home(&target.to_string_lossy(), &home.to_string_lossy())
    ));
    ok()
}

fn shared_apply(app: &AppHandle, mode: SharedMode) -> Value {
    let cfg = app.state::<Arc<Panes>>().config();
    let accounts: Vec<_> = cfg.accounts.iter().map(mods_account).collect();
    match app.state::<Shared>().0.apply(mode, &accounts, &log) {
        Ok(report) => json!({ "ok": true, "report": report }),
        Err(e) => fail(e),
    }
}

// ---- Shells, clipboard images ------------------------------------------------------------

fn shell_list() -> Value {
    let env = crate::sys::env::spawn_env();
    let get = env_lookup(&env);
    let os = Os::current();
    let shells = list_shells(
        os,
        &get,
        &|n| find_on_path(n, &get, os),
        &|p: &Path| p.exists(),
        &|p: &Path| std::fs::read_to_string(p).ok(),
    );
    Value::Array(
        shells
            .into_iter()
            .map(|s| json!({ "name": s.name, "path": s.path, "args": s.args }))
            .collect(),
    )
}

/// wl-paste's image, for images only some Wayland clipboards expose.
fn wl_paste_image() -> Option<(Vec<u8>, &'static str)> {
    let run = |args: &[&str]| {
        std::process::Command::new("wl-paste")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    };
    let listed = String::from_utf8(run(&["--list-types"])?).ok()?;
    let (mime, ext) = crate::sys::native::pick_image_type(listed.lines())?;
    let data = run(&["--no-newline", "--type", mime])?;
    (!data.is_empty()).then_some((data, ext))
}

/// Writes the image into the temp dir's `wraithgrid-paste` (0700), dropping pastes older
/// than a day; returns the file.
fn save_image(data: &[u8], ext: &str) -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("wraithgrid-paste");
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir)?;
    let now = std::time::SystemTime::now();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let old = e
            .metadata()
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age.as_secs() > 24 * 60 * 60);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let ms = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let tag = wraithgrid_core::ids::new_id("p");
    let file = dir.join(format!("paste-{ms}-{tag}.{ext}"));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    std::io::Write::write_all(&mut opts.open(&file)?, data)?;
    Ok(file)
}

/// How a pasted image path is typed into a pane: a plain path, or an `@path` reference.
fn image_paste_text(cli: AgentCli, file: &str) -> String {
    let quoted = if file.chars().any(char::is_whitespace) {
        format!("\"{file}\"")
    } else {
        file.to_string()
    };
    match cli.spec().image_paste {
        ImagePaste::Path => format!("{quoted} "),
        ImagePaste::AtPath => format!("@{quoted} "),
    }
}

fn clipboard_image(app: &AppHandle, pane_id: &str) -> Value {
    let cfg = app.state::<Arc<Panes>>().config();
    let pane = cfg
        .workspaces
        .iter()
        .flat_map(|w| &w.panes)
        .find(|p| p.id == pane_id);
    let account = pane
        .filter(|p| !p.shell)
        .and_then(|p| p.account_id.as_deref())
        .and_then(|id| cfg.accounts.iter().find(|a| a.id == id));
    let Some(account) = account else {
        return Value::Null;
    };
    let wayland =
        wraithgrid_core::platform::detect_desktop(Os::current(), |k| std::env::var(k).ok()).wayland;
    let image = crate::sys::native::clipboard_image(app).or_else(|| {
        if wayland {
            wl_paste_image()
        } else {
            None
        }
    });
    let Some((data, ext)) = image else {
        return Value::Null;
    };
    match save_image(&data, ext) {
        Ok(file) => image_paste_text(account.cli, &file.to_string_lossy()).into(),
        Err(e) => {
            log(&format!("clipboard:image failed: {e}"));
            Value::Null
        }
    }
}

// ---- Machines (H5) -------------------------------------------------------------------------

fn ssh_hosts() -> Value {
    let file = home_dir().join(".ssh").join("config");
    let hosts = std::fs::read_to_string(file)
        .map(|t| wraithgrid_remote::ssh::ssh_config_hosts(&t))
        .unwrap_or_default();
    json!(hosts)
}

fn machine_test(app: &AppHandle, name: String, ssh_host: String) -> Value {
    let m = Machine {
        id: "probe".into(),
        name,
        ssh_host,
    };
    match app.state::<Arc<Panes>>().probe(&m) {
        Ok(p) => json!({ "ok": true, "appVersion": p.app_version, "live": p.live }),
        Err(e) => fail(e),
    }
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    let app2 = app.clone();
    let p = payload;
    Some(Ok(match channel {
        "login:start" => match args::id(&p, "accountId") {
            Some(id) => blocking(move || login_start(&app2, &id)).await,
            None => fail("Could not start the sign-in"),
        },
        "login:code" => {
            if let Some((id, code)) = login_code(&p) {
                app.state::<Arc<Logins>>().code(&id, &code);
            }
            Value::Null
        }
        "login:cancel" => {
            if let Some(id) = args::id(&p, "accountId") {
                app.state::<Arc<Logins>>().cancel(&id);
            }
            Value::Null
        }
        "claude:detect" => match args::opt_string(&p, "override", 0, 4096) {
            Some(over) => blocking(move || claude_detect(&app2, over)).await,
            None => {
                json!({ "path": null, "source": null, "version": null, "ok": false, "error": "Detection failed" })
            }
        },
        "account:createDir" => {
            let slug = p.get("slug").and_then(Value::as_str).filter(|s| {
                let b = s.as_bytes();
                (1..=48).contains(&b.len())
                    && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
                    && b.iter().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-'
                    })
            });
            match slug {
                Some(slug) => {
                    let slug = slug.to_string();
                    blocking(move || create_dir(&slug)).await
                }
                None => fail("Could not create the config dir"),
            }
        }
        "account:deleteDir" => {
            let dir = args::string(&p, "dir", 1, 4096);
            match (dir, p.get("confirm").and_then(Value::as_bool)) {
                (Some(dir), Some(true)) => blocking(move || delete_dir(&app2, &dir)).await,
                _ => fail("Could not delete the config dir"),
            }
        }
        "shared:apply" => {
            let mode = match p.get("mode").and_then(Value::as_str) {
                Some("overall") => Some(SharedMode::Overall),
                Some("per-account") => Some(SharedMode::PerAccount),
                _ => None,
            };
            match mode {
                Some(mode) => blocking(move || shared_apply(&app2, mode)).await,
                None => fail("Could not update the shared CLAUDE.md and skills"),
            }
        }
        "shell:list" => blocking(shell_list).await,
        "clipboard:image" => match args::id(&p, "paneId") {
            Some(id) => blocking(move || clipboard_image(&app2, &id)).await,
            None => Value::Null,
        },
        "notify:pane" => {
            let pane = args::id(&p, "paneId");
            let title = args::string(&p, "title", 1, 120);
            let body = args::string(&p, "body", 0, 300);
            if let (Some(pane), Some(title), Some(body)) = (pane, title, body) {
                let cfg = app.state::<Arc<Panes>>().config();
                if cfg
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.panes)
                    .any(|x| x.id == pane)
                {
                    crate::sys::notify::pane_notice(app, pane, title, body);
                }
            }
            Value::Null
        }
        "machine:sshHosts" => blocking(ssh_hosts).await,
        "machine:test" => {
            let name = args::string(&p, "name", 1, 64);
            let host = p
                .get("sshHost")
                .and_then(Value::as_str)
                .filter(|h| args::is_ssh_host(h));
            match (name, host) {
                (Some(name), Some(host)) => {
                    let host = host.to_string();
                    blocking(move || machine_test(&app2, name, host)).await
                }
                _ => fail("Invalid machine"),
            }
        }
        "cli:result" => {
            if let Some((id, result)) = crate::sys::panes::parse_cli_result(&p) {
                app.state::<Arc<Panes>>().cli_result(id, result);
            }
            Value::Null
        }
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_text() {
        assert_eq!(image_paste_text(AgentCli::Claude, "/t/a.png"), "/t/a.png ");
        assert_eq!(
            image_paste_text(AgentCli::Gemini, "/t/a b.png"),
            "@\"/t/a b.png\" "
        );
    }
}
