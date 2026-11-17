//! `dialog:pickFolder`, `dialog:pickFile`, `fs:listFolder`, `fs:reveal` (the dialog and folder
//! handlers of `src/main/ipc.ts`). Native dialogs come from tauri-plugin-dialog with its GTK
//! backend on Linux (the xdg portal has been crashing on Hyprland); a dialog that fails to open
//! answers like a cancelled one, and the renderer's in-app folder picker still works.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_dialog::{DialogExt, FilePath};
use wraithgrid_core::folders::list_folder;
use wraithgrid_core::paths::{home_dir, resolve_user_path};

use super::{fail, ok};

pub fn setup(app: &AppHandle) {
    // Another group may have registered it already (themes import/export use dialogs too).
    if app
        .try_state::<tauri_plugin_dialog::Dialog<Wry>>()
        .is_none()
    {
        if let Err(e) = app.plugin(tauri_plugin_dialog::init()) {
            eprintln!("wraithgrid: no native dialogs: {e}");
        }
    }
}

/// What a native open dialog picks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Folder,
    File,
    Files,
}

/// Shows a native open dialog over the main window; the picked paths, or None when cancelled
/// (or when no dialog could be shown).
pub async fn pick(
    app: &AppHandle,
    kind: Pick,
    title: Option<&str>,
    dir: &Path,
) -> Option<Vec<PathBuf>> {
    let mut builder = app.dialog().file().set_directory(dir);
    if let Some(title) = title {
        builder = builder.set_title(title);
    }
    if let Some(win) = app.get_webview_window("main") {
        builder = builder.set_parent(&win);
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<Vec<FilePath>>>();
    match kind {
        Pick::Folder => builder
            .set_can_create_directories(true)
            .pick_folder(move |p| {
                let _ = tx.send(p.map(|p| vec![p]));
            }),
        Pick::File => builder.pick_file(move |p| {
            let _ = tx.send(p.map(|p| vec![p]));
        }),
        Pick::Files => builder.pick_files(move |p| {
            let _ = tx.send(p);
        }),
    }
    // A dialog that never came up drops the sender: the same as a cancel.
    let picked = rx.await.ok().flatten()?;
    let paths: Vec<PathBuf> = picked
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect();
    (!paths.is_empty()).then_some(paths)
}

fn str_arg<'a>(payload: &'a Value, key: &str, max: usize) -> Result<Option<&'a str>, ()> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.chars().count() <= max => Ok(Some(s)),
        _ => Err(()),
    }
}

/// `FolderListResult` of ipc-contract.ts.
fn folder_list(path: &str) -> Value {
    let list = list_folder(path, &home_dir());
    match list.result {
        Ok(listed) => json!({
            "ok": true,
            "path": list.path,
            "parent": list.parent,
            "entries": listed
                .entries
                .iter()
                .map(|e| json!({ "name": e.name, "hidden": e.hidden, "git": e.git }))
                .collect::<Vec<_>>(),
            "truncated": listed.truncated,
            "branch": listed.branch,
        }),
        Err(error) => {
            json!({ "ok": false, "path": list.path, "parent": list.parent, "error": error })
        }
    }
}

/// Opens a folder in the file manager (Electron's `shell.openPath`): its error, if any.
fn open_path(dir: &Path) -> Result<(), String> {
    let mut cmd = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        Command::new("explorer")
    } else {
        Command::new("xdg-open")
    };
    let status = cmd
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Could not open the folder: {e}"))?;
    // explorer.exe answers 1 even when it opened the folder.
    if status.success() || cfg!(windows) {
        Ok(())
    } else {
        Err("No application is set to open folders.".into())
    }
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    Some(Ok(match channel {
        "dialog:pickFolder" | "dialog:pickFile" => {
            let Ok(default) = str_arg(&payload, "defaultPath", 4096) else {
                return Some(Ok(Value::Null));
            };
            let home = home_dir();
            let dir = default.map_or(home.clone(), |d| resolve_user_path(d, &home));
            let kind = if channel == "dialog:pickFolder" {
                Pick::Folder
            } else {
                Pick::File
            };
            match pick(app, kind, None, &dir).await {
                Some(paths) => Value::from(paths[0].to_string_lossy().into_owned()),
                None => Value::Null,
            }
        }
        "fs:listFolder" => match str_arg(&payload, "path", 4096) {
            Ok(Some(path)) => {
                let path = path.to_string();
                tauri::async_runtime::spawn_blocking(move || folder_list(&path))
                    .await
                    .unwrap_or_else(|_| {
                        json!({ "ok": false, "path": "~", "parent": null, "error": "This folder can’t be read." })
                    })
            }
            _ => {
                json!({ "ok": false, "path": "~", "parent": null, "error": "This folder can’t be read." })
            }
        },
        "fs:reveal" => match str_arg(&payload, "path", 4096) {
            Ok(Some(path)) => {
                let raw = path.trim();
                let dir = resolve_user_path(if raw.is_empty() { "~" } else { raw }, &home_dir());
                if !dir.is_dir() {
                    fail("That folder does not exist.")
                } else {
                    match tauri::async_runtime::spawn_blocking(move || open_path(&dir)).await {
                        Ok(Ok(())) => ok(),
                        Ok(Err(e)) => fail(e),
                        Err(_) => fail("Could not open the folder"),
                    }
                }
            }
            _ => fail("Could not open the folder"),
        },
        _ => return None,
    }))
}
