//! `update:*` (src/main/update-check.ts, update-install.ts, release-notes.ts): GitHub releases
//! of cachewraith/wraithgrid through core::update (package kind, SHA256SUMS check, install),
//! and About's release notes through core::release_notes with docs/WHATS-NEW.md bundled.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use wraithgrid_core::paths::current_user_data_dir;
use wraithgrid_core::release_notes::{self, parse_whats_new, ReleaseNote};
use wraithgrid_core::update::{self, CheckResult, PackageKind, ReleaseInfo};

const WHATS_NEW: &str = include_str!("../../../docs/WHATS-NEW.md");
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Unauthenticated GitHub API calls are limited to 60 an hour: repeated checks reuse the last.
const MIN_CHECK_MS: i64 = 30_000;
const RECHECK: Duration = Duration::from_secs(6 * 60 * 60);
const PACKAGED: bool = !cfg!(debug_assertions);

#[derive(Default)]
struct State {
    last: Option<(Value, i64, Option<ReleaseInfo>)>,
    /// The downloaded release and its file, waiting for Restart now.
    ready: Option<(ReleaseInfo, PathBuf)>,
    downloading: bool,
}

pub struct Updates {
    state: Mutex<State>,
    cancel: Arc<AtomicBool>,
    /// Serializes checks and downloads (a second click joins by waiting).
    busy: tauri::async_runtime::Mutex<()>,
    updated_from: Option<String>,
}

fn notes_file() -> PathBuf {
    current_user_data_dir().join("release-notes.json")
}

fn now_ms() -> i64 {
    wraithgrid_usage::now_ms()
}

fn user_agent() -> String {
    format!("Wraithgrid/{VERSION}")
}

fn note_json(n: &ReleaseNote) -> Value {
    json!({ "version": n.version, "title": n.title, "date": n.date, "notes": n.notes })
}

fn release_json(r: &ReleaseInfo) -> Value {
    json!({ "version": r.version, "url": r.url, "notes": r.notes, "publishedAt": r.published_at })
}

fn progress(app: &AppHandle, p: Value) {
    super::emit(app, "update:progress", p);
}

pub fn setup(app: &AppHandle) {
    let updated_from = release_notes::start(&notes_file(), VERSION);
    app.manage(Updates {
        state: Mutex::default(),
        cancel: Arc::default(),
        busy: tauri::async_runtime::Mutex::new(()),
        updated_from,
    });
    if PACKAGED {
        let app = app.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(RECHECK);
            let on = app
                .state::<super::config::Store>()
                .0
                .lock()
                .unwrap()
                .get()
                .settings
                .check_updates_on_launch;
            if on {
                tauri::async_runtime::block_on(check(&app, true));
            }
        });
    }
}

/// One check at a time, at most one request every 30 s; keeps the latest notes for after the
/// update and, when `notify`, announces a newer release once per version.
async fn check(app: &AppHandle, notify: bool) -> Value {
    let updates = app.state::<Updates>();
    let _busy = updates.busy.lock().await;
    if let Some((answer, at, _)) = &updates.state.lock().unwrap().last {
        if now_ms() - at < MIN_CHECK_MS {
            return answer.clone();
        }
    }
    let result = tauri::async_runtime::spawn_blocking(|| {
        let ua = user_agent();
        update::check(VERSION, &|url| update::fetch(url, &ua))
    })
    .await
    .unwrap_or(CheckResult::Error {
        current: VERSION.into(),
        error: "The update check failed.".into(),
    });
    let checked_at = now_ms();
    let (answer, latest) = match &result {
        CheckResult::Available { current, latest } => (
            json!({ "status": "available", "current": current, "latest": release_json(latest), "checkedAt": checked_at }),
            Some(latest.clone()),
        ),
        CheckResult::Current { current, latest } => (
            json!({ "status": "current", "current": current, "latest": release_json(latest), "checkedAt": checked_at }),
            Some(latest.clone()),
        ),
        CheckResult::Error { current, error } => {
            eprintln!("wraithgrid: update check failed: {error}");
            (
                json!({ "status": "error", "current": current, "error": error, "checkedAt": checked_at }),
                None,
            )
        }
    };
    if let Some(latest) = &latest {
        release_notes::remember(&notes_file(), latest);
        if notify && matches!(result, CheckResult::Available { .. }) {
            let state = current_user_data_dir().join("update-notified.json");
            if let Some((title, body)) = release_notes::notice_once(&state, VERSION, latest) {
                notify_os(&title, &body);
            }
        }
    }
    updates.state.lock().unwrap().last = Some((answer.clone(), checked_at, latest));
    answer
}

/// The new-release notice through the desktop's notifier (no click action here).
fn notify_os(title: &str, body: &str) {
    if cfg!(target_os = "linux") {
        let _ = std::process::Command::new("notify-send")
            .args(["--app-name=Wraithgrid", title, body])
            .spawn();
    } else {
        eprintln!("wraithgrid: {title}: {body}");
    }
}

fn install_fail(error: &str, manual: bool) -> Value {
    json!({ "ok": false, "error": error, "manual": manual })
}

async fn download(app: &AppHandle) -> Value {
    let updates = app.state::<Updates>();
    if let Some((r, _)) = &updates.state.lock().unwrap().ready {
        progress(app, json!({ "phase": "ready", "version": r.version }));
        return json!({ "ok": true });
    }
    let _busy = updates.busy.lock().await;
    if let Some((r, _)) = &updates.state.lock().unwrap().ready {
        progress(app, json!({ "phase": "ready", "version": r.version }));
        return json!({ "ok": true });
    }
    let fail = |error: &str, manual: bool| {
        progress(app, json!({ "phase": "idle" }));
        install_fail(error, manual)
    };
    progress(app, json!({ "phase": "checking" }));
    let exe = std::env::current_exe().unwrap_or_default();
    let kind = update::package_kind(&exe, |k| std::env::var(k).ok());
    if kind == PackageKind::Manual {
        return fail(update::MANUAL, true);
    }
    let checked = tauri::async_runtime::spawn_blocking(|| {
        let ua = user_agent();
        update::check(VERSION, &|url| update::fetch(url, &ua))
    })
    .await;
    let release = match checked {
        Ok(CheckResult::Available { latest, .. }) => latest,
        Ok(CheckResult::Current { .. }) => {
            return fail("You already have the latest version.", false)
        }
        Ok(CheckResult::Error { error, .. }) => {
            eprintln!("wraithgrid: update download failed: {error}");
            return fail("Could not reach GitHub to fetch the update.", false);
        }
        Err(_) => return fail("Could not reach GitHub to fetch the update.", false),
    };
    let version = release.version.clone();
    progress(
        app,
        json!({ "phase": "downloading", "version": version, "percent": 0, "transferred": null, "total": null }),
    );
    updates.cancel.store(false, Ordering::Relaxed);
    updates.state.lock().unwrap().downloading = true;
    let cancel = updates.cancel.clone();
    let job_app = app.clone();
    let job_release = release.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let dir = std::env::temp_dir().join("wraithgrid-update");
        let mut last = 0u8;
        let v = job_release.version.clone();
        update::fetch_update(
            &job_release,
            kind,
            &dir,
            &user_agent(),
            &mut |p| {
                if p != last {
                    last = p;
                    progress(
                        &job_app,
                        json!({ "phase": "downloading", "version": v, "percent": p, "transferred": null, "total": null }),
                    );
                }
            },
            &|| cancel.load(Ordering::Relaxed),
        )
    })
    .await
    .unwrap_or_else(|_| Err("The download failed or did not match its checksum.".into()));
    updates.state.lock().unwrap().downloading = false;
    match result {
        Ok(file) => {
            updates.state.lock().unwrap().ready = Some((release, file));
            progress(app, json!({ "phase": "ready", "version": version }));
            json!({ "ok": true })
        }
        Err(_) if updates.cancel.load(Ordering::Relaxed) => {
            progress(app, json!({ "phase": "idle" }));
            json!({ "ok": false, "cancelled": true })
        }
        Err(e) => {
            eprintln!("wraithgrid: update download failed: {e}");
            let shown = if e.starts_with("This release has no") {
                e.as_str()
            } else {
                "The download failed or did not match its checksum."
            };
            fail(shown, false)
        }
    }
}

async fn restart(app: &AppHandle) -> Value {
    let updates = app.state::<Updates>();
    let Some((release, file)) = updates.state.lock().unwrap().ready.clone() else {
        return install_fail("No update has been downloaded yet.", false);
    };
    let version = release.version.clone();
    progress(app, json!({ "phase": "installing", "version": version }));
    let exe = std::env::current_exe().unwrap_or_default();
    let kind = update::package_kind(&exe, |k| std::env::var(k).ok());
    let installed = tauri::async_runtime::spawn_blocking(move || {
        let appimage = std::env::var("APPIMAGE").ok().map(PathBuf::from);
        let r = match (kind, &appimage) {
            (PackageKind::AppImage, Some(target)) => {
                update::install_appimage(&file, target).map_err(|e| e.to_string())
            }
            _ => match update::install_command(kind, &file) {
                Some((cmd, args)) => match std::process::Command::new(cmd).args(args).status() {
                    Ok(s) if s.success() || kind == PackageKind::Nsis => Ok(()),
                    Ok(s) => Err(format!("installer exited with {s}")),
                    Err(e) => Err(e.to_string()),
                },
                None => Err(update::MANUAL.to_string()),
            },
        };
        if r.is_ok() && kind != PackageKind::Nsis {
            let relaunch = appimage.unwrap_or(exe);
            let _ = std::process::Command::new(relaunch).spawn();
        }
        r
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    match installed {
        Ok(()) => {
            super::usage::flush(app);
            app.exit(0);
            json!({ "ok": true })
        }
        Err(e) => {
            eprintln!("wraithgrid: update install failed: {e}");
            progress(app, json!({ "phase": "ready", "version": version }));
            install_fail("The update was cancelled or could not be installed.", false)
        }
    }
}

fn notes(app: &AppHandle) -> Value {
    let bundled = parse_whats_new(WHATS_NEW);
    let view = release_notes::view(&notes_file(), VERSION, &bundled);
    json!({
        "current": VERSION,
        "updatedFrom": app.state::<Updates>().updated_from,
        "installed": view.installed.as_ref().map(note_json),
        "earlier": view.earlier.iter().map(note_json).collect::<Vec<_>>(),
    })
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    Some(Ok(match channel {
        "update:check" => match payload.get("reason").and_then(Value::as_str) {
            Some("launch") if !PACKAGED => json!({ "status": "skipped", "current": VERSION }),
            Some(reason @ ("launch" | "manual")) => check(app, reason == "launch").await,
            _ => {
                eprintln!("wraithgrid: refused update:check: invalid arguments");
                json!({ "status": "error", "current": VERSION, "error": "The update check failed.", "checkedAt": now_ms() })
            }
        },
        "update:download" => download(app).await,
        "update:restart" => restart(app).await,
        "update:cancel" => {
            let updates = app.state::<Updates>();
            if updates.state.lock().unwrap().downloading {
                updates.cancel.store(true, Ordering::Relaxed);
            }
            Value::Null
        }
        "update:notes" => notes(app),
        _ => return None,
    }))
}
