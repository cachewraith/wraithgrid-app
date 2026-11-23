//! `git:*` and `project:suggestIcons` (src/main/ipc.ts + git.ts) over the wraithgrid-git
//! crate. Git runs in the pane's folder as this process's config has it, never a renderer
//! path; every git call is blocking, so it runs on the blocking pool. `git:clone` is the one
//! that takes a folder from the renderer: the parent the user picked for a new project.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use wraithgrid_core::config::Config;
use wraithgrid_core::paths::{
    contract_home, home_dir, is_strictly_inside, resolve_user_path, worktrees_root,
};
use wraithgrid_git::{
    add_worktree, check_worktree, clone_repo, commit_all, git_diff, git_log, git_show, git_status,
    list_branches, pull_branch, pull_request_link, push_branch, remove_worktree, switch_branch,
    GitCli, GitStatus, SwitchTarget, WorktreeFrom,
};

use super::fail;

pub fn setup(_app: &AppHandle) {}

fn config(app: &AppHandle) -> Config {
    app.state::<super::config::Store>()
        .0
        .lock()
        .unwrap()
        .get()
        .clone()
}

/// The pane's folder, resolved, from this process's own config.
fn pane_cwd(cfg: &Config, pane_id: &str) -> Option<PathBuf> {
    cfg.workspaces
        .iter()
        .flat_map(|w| w.panes.iter())
        .find(|p| p.id == pane_id)
        .map(|p| resolve_user_path(&p.cwd, &home_dir()))
}

fn machine_of_pane(cfg: &Config, pane_id: &str) -> Option<String> {
    cfg.workspaces
        .iter()
        .find(|w| w.panes.iter().any(|p| p.id == pane_id))
        .and_then(|w| w.machine_id.clone())
}

fn str_arg<'a>(payload: &'a Value, key: &str, max: usize) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= max)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .expect("git task panicked")
}

fn action(r: Result<String, String>) -> Value {
    match r {
        Ok(detail) => json!({ "ok": true, "detail": detail }),
        Err(e) => fail(e),
    }
}

/// A Wraithgrid worktree folder (real path under ~/.wraithgrid/worktrees), or None. The path
/// goes to git, so dunce keeps it plain on Windows (git cannot use std's \\?\C:\ form).
fn own_worktree(cwd: &str) -> Option<PathBuf> {
    let home = home_dir();
    let dir = dunce::canonicalize(resolve_user_path(cwd, &home)).ok()?;
    let root = dunce::canonicalize(worktrees_root(&home)).ok()?;
    is_strictly_inside(&dir, &root).then_some(dir)
}

/// The git for a clone: a big repository takes far longer than a push, and ssh must fail
/// rather than wait for a host-key or passphrase answer nobody can type (unless the user set
/// their own ssh command).
fn clone_git() -> GitCli {
    let mut git = GitCli {
        slow_timeout: Duration::from_secs(30 * 60),
        ..GitCli::default()
    };
    if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none() {
        git.env
            .push(("GIT_SSH_COMMAND".into(), "ssh -o BatchMode=yes".into()));
    }
    git
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    let gone = || fail("That pane is gone.");
    let invalid = |rejected: Value| {
        eprintln!("wraithgrid: refused {channel}: invalid arguments");
        Some(Ok(rejected))
    };
    let answer = match channel {
        "project:suggestIcons" => {
            let Some(ws_id) = str_arg(&payload, "wsId", 200) else {
                return invalid(json!([]));
            };
            let cfg = config(app);
            let Some(ws) = cfg.workspaces.iter().find(|w| w.id == ws_id) else {
                return Some(Ok(Value::Null));
            };
            if ws.machine_id.is_some() {
                return Some(Ok(json!([])));
            }
            let dir = if ws.folder.is_empty() {
                ws.panes.first().map(|p| p.cwd.clone()).unwrap_or_default()
            } else {
                ws.folder.clone()
            };
            if dir.is_empty() {
                return Some(Ok(json!([])));
            }
            let root = resolve_user_path(&dir, &home_dir());
            let icons = blocking(move || {
                crate::panels::project_icons::suggest(&crate::panels::project_icons::read_markers(
                    &root,
                ))
            })
            .await;
            json!(icons)
        }
        "git:status" => {
            let Some(pane) = str_arg(&payload, "paneId", 200) else {
                return invalid(json!(GitStatus::default()));
            };
            match pane_cwd(&config(app), pane) {
                None => json!(GitStatus::default()),
                Some(cwd) => json!(blocking(move || git_status(&GitCli::default(), &cwd)).await),
            }
        }
        "git:diff" => {
            let Some(pane) = str_arg(&payload, "paneId", 200) else {
                return invalid(fail("Could not read the diff"));
            };
            match pane_cwd(&config(app), pane) {
                None => gone(),
                Some(cwd) => match blocking(move || git_diff(&GitCli::default(), &cwd)).await {
                    Ok(d) => {
                        json!({ "ok": true, "patch": d.patch, "truncated": d.truncated, "untracked": d.untracked })
                    }
                    Err(e) => fail(e),
                },
            }
        }
        "git:worktreeAdd" => {
            let (Some(cwd_arg), Some(branch)) = (
                str_arg(&payload, "cwd", 4096),
                str_arg(&payload, "branch", 100),
            ) else {
                return invalid(fail("Could not create the worktree"));
            };
            let reuse = payload
                .get("reuse")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let from = match payload.get("from").and_then(Value::as_str).unwrap_or("new") {
                "new" => WorktreeFrom::New,
                "local" => WorktreeFrom::Local,
                "remote" => WorktreeFrom::Remote,
                _ => return invalid(fail("Could not create the worktree")),
            };
            let home = home_dir();
            let cwd = resolve_user_path(cwd_arg, &home);
            match std::fs::metadata(&cwd) {
                Err(_) => fail(format!("Folder not found: {cwd_arg}")),
                Ok(m) if !m.is_dir() => fail(format!("Not a folder: {cwd_arg}")),
                Ok(_) => {
                    let branch = branch.to_string();
                    let b = branch.clone();
                    let root = worktrees_root(&home);
                    let r = blocking(move || {
                        add_worktree(&GitCli::default(), &cwd, &b, &root, reuse, from)
                    })
                    .await;
                    match r {
                        Ok(dir) => {
                            let dir =
                                contract_home(&dir.to_string_lossy(), &home.to_string_lossy());
                            let new = if from == WorktreeFrom::New {
                                "new "
                            } else {
                                ""
                            };
                            eprintln!("wraithgrid: created worktree {dir} on {new}branch {branch}");
                            json!({ "ok": true, "dir": dir })
                        }
                        Err(e) => fail(e),
                    }
                }
            }
        }
        "git:clone" => {
            let (Some(url), Some(parent_arg)) = (
                str_arg(&payload, "url", wraithgrid_git::CLONE_URL_MAX),
                str_arg(&payload, "parent", 4096),
            ) else {
                return invalid(fail("Could not clone the repository"));
            };
            let branch = match payload.get("branch") {
                None | Some(Value::Null) => None,
                Some(Value::String(b)) if b.is_empty() => None,
                Some(Value::String(b)) => Some(b.clone()),
                Some(_) => return invalid(fail("Could not clone the repository")),
            };
            let home = home_dir();
            let parent = resolve_user_path(parent_arg, &home);
            if let Err(e) = std::fs::create_dir_all(&parent) {
                return Some(Ok(fail(format!("Could not make {parent_arg}: {e}"))));
            }
            if !parent.is_dir() {
                return Some(Ok(fail(format!("Not a folder: {parent_arg}"))));
            }
            // Plain path for git on Windows (no \\?\ prefix).
            let parent = dunce::canonicalize(&parent).unwrap_or(parent);
            let url = url.to_string();
            match blocking(move || clone_repo(&clone_git(), &parent, &url, branch.as_deref())).await
            {
                Ok(c) => {
                    let dir = contract_home(&c.dir.to_string_lossy(), &home.to_string_lossy());
                    let how = if c.existing { "reused" } else { "cloned into" };
                    eprintln!("wraithgrid: {how} {dir}");
                    json!({ "ok": true, "dir": dir, "existing": c.existing })
                }
                Err(e) => fail(e),
            }
        }
        "git:worktreeCheck" => {
            let Some(cwd) = str_arg(&payload, "cwd", 4096) else {
                return invalid(fail("Could not check the worktree"));
            };
            let cwd = cwd.to_string();
            blocking(move || match own_worktree(&cwd) {
                None => fail("Not a Wraithgrid worktree."),
                Some(dir) => match check_worktree(&GitCli::default(), &dir) {
                    Ok(c) => json!({
                        "ok": true,
                        "branch": c.branch,
                        "changed": c.changed,
                        "mainRoot": c.main_root.to_string_lossy(),
                    }),
                    Err(e) => fail(e),
                },
            })
            .await
        }
        "git:worktreeRemove" => {
            let (Some(cwd), Some(force)) = (
                str_arg(&payload, "cwd", 4096),
                payload.get("force").and_then(Value::as_bool),
            ) else {
                return invalid(fail("Could not remove the worktree"));
            };
            let cwd = cwd.to_string();
            blocking(move || match own_worktree(&cwd) {
                None => fail("Not a Wraithgrid worktree."),
                Some(dir) => {
                    let r = remove_worktree(&GitCli::default(), &dir, force);
                    if r.is_ok() {
                        let shown =
                            contract_home(&dir.to_string_lossy(), &home_dir().to_string_lossy());
                        let forced = if force { " (forced)" } else { "" };
                        eprintln!("wraithgrid: removed worktree {shown}{forced}");
                    }
                    action(r)
                }
            })
            .await
        }
        "git:branches" | "git:switch" => {
            let Some(pane) = str_arg(&payload, "paneId", 200) else {
                return invalid(fail("Invalid request"));
            };
            let cfg = config(app);
            let Some(cwd) = pane_cwd(&cfg, pane) else {
                return Some(Ok(gone()));
            };
            if machine_of_pane(&cfg, pane).is_some() {
                return Some(Ok(fail(
                    "Branches of panes on another machine can\u{2019}t be switched from here yet.",
                )));
            }
            if channel == "git:branches" {
                match blocking(move || list_branches(&GitCli::default(), &cwd)).await {
                    Ok(b) => {
                        let mut v = json!(b);
                        v["ok"] = json!(true);
                        v
                    }
                    Err(e) => fail(e),
                }
            } else {
                let Some(name) = str_arg(&payload, "name", 255).map(String::from) else {
                    return invalid(fail("Invalid request"));
                };
                let to = match payload.get("kind").and_then(Value::as_str) {
                    Some("local") => SwitchTarget::Local(name),
                    Some("remote") => SwitchTarget::Remote(name),
                    Some("create") => SwitchTarget::Create(name),
                    _ => return invalid(fail("Invalid request")),
                };
                action(blocking(move || switch_branch(&GitCli::default(), &cwd, &to)).await)
            }
        }
        "git:commit" => {
            let (Some(pane), Some(message)) = (
                str_arg(&payload, "paneId", 200),
                str_arg(&payload, "message", 5000),
            ) else {
                return invalid(fail("Could not commit"));
            };
            let message = message.to_string();
            match pane_cwd(&config(app), pane) {
                None => gone(),
                Some(cwd) => {
                    action(blocking(move || commit_all(&GitCli::default(), &cwd, &message)).await)
                }
            }
        }
        "git:push" | "git:pull" | "git:openPr" => {
            let rejected = match channel {
                "git:push" => "Could not push",
                "git:pull" => "Could not pull",
                _ => "Could not open the pull request page",
            };
            let Some(pane) = str_arg(&payload, "paneId", 200) else {
                return invalid(fail(rejected));
            };
            let Some(cwd) = pane_cwd(&config(app), pane) else {
                return Some(Ok(gone()));
            };
            match channel {
                "git:push" => action(blocking(move || push_branch(&GitCli::default(), &cwd)).await),
                "git:pull" => action(blocking(move || pull_branch(&GitCli::default(), &cwd)).await),
                _ => match blocking(move || pull_request_link(&GitCli::default(), &cwd)).await {
                    Ok(url) => {
                        crate::panels::open_external(&url);
                        json!({ "ok": true, "detail": "Opened in your browser" })
                    }
                    Err(e) => fail(e),
                },
            }
        }
        "git:log" => {
            let pane = str_arg(&payload, "paneId", 200);
            let skip = payload
                .get("skip")
                .and_then(Value::as_u64)
                .filter(|s| *s <= 100_000);
            let (Some(pane), Some(skip)) = (pane, skip) else {
                return invalid(fail("Could not read the history"));
            };
            match pane_cwd(&config(app), pane) {
                None => gone(),
                Some(cwd) => {
                    match blocking(move || git_log(&GitCli::default(), &cwd, skip as usize)).await {
                        Ok(l) => json!({ "ok": true, "commits": l.commits, "more": l.more }),
                        Err(e) => fail(e),
                    }
                }
            }
        }
        "git:show" => {
            let pane = str_arg(&payload, "paneId", 200);
            let sha = str_arg(&payload, "sha", 40).filter(|s| {
                s.len() >= 7
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            });
            let (Some(pane), Some(sha)) = (pane, sha) else {
                return invalid(fail("Could not read the commit"));
            };
            let sha = sha.to_string();
            match pane_cwd(&config(app), pane) {
                None => gone(),
                Some(cwd) => match blocking(move || git_show(&GitCli::default(), &cwd, &sha)).await
                {
                    Ok(s) => json!({ "ok": true, "patch": s.patch, "truncated": s.truncated }),
                    Err(e) => fail(e),
                },
            }
        }
        _ => return None,
    };
    Some(Ok(answer))
}
