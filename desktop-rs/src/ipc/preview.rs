//! `preview:*` (src/main/preview.ts and its ipc.ts handlers) over core::preview: detect a
//! preview pane's dev command, run it in the project folder and report its log, URL and exit
//! as `preview:event`. The page itself is an iframe in Tauri (PreviewView.tsx), so there is no
//! web contents to screenshot or watch: `preview:capture` fails cleanly and
//! `preview:requestFailed` is never sent.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use wraithgrid_core::config::Config;
use wraithgrid_core::paths::{contract_home, home_dir, resolve_user_path};
use wraithgrid_core::preview::{
    detect_dev_command, read_project_files, shell_launch, DevServer, PreviewEvent,
};
use wraithgrid_core::types::PaneView;

use super::{fail, ok};

#[derive(Default)]
pub struct DevServers(Mutex<HashMap<String, DevServer>>);

pub fn setup(app: &AppHandle) {
    app.manage(DevServers::default());
    // Dev servers end with the window (src/main/app.ts stopAll on quit).
    if let Some(win) = app.get_webview_window("main") {
        let app = app.clone();
        win.on_window_event(move |e| {
            if matches!(e, tauri::WindowEvent::Destroyed) {
                stop_all(&app);
            }
        });
    }
}

/// Stops every dev server (quit).
pub fn stop_all(app: &AppHandle) {
    if let Some(s) = app.try_state::<DevServers>() {
        s.0.lock().unwrap().clear();
    }
}

/// A preview pane on this computer: its project's command and its folder.
fn preview_pane(cfg: &Config, pane_id: &str) -> Option<(String, String)> {
    for ws in &cfg.workspaces {
        if let Some(p) = ws.panes.iter().find(|p| p.id == pane_id) {
            return (p.view == PaneView::Preview && ws.machine_id.is_none())
                .then(|| (ws.preview.command.trim().to_string(), p.cwd.clone()));
        }
    }
    None
}

fn event_json(e: PreviewEvent) -> Value {
    match e {
        PreviewEvent::Log(line) => json!({ "kind": "log", "line": line }),
        PreviewEvent::Url(url) => json!({ "kind": "url", "url": url }),
        PreviewEvent::Exit {
            code,
            error: Some(error),
        } => json!({ "kind": "exit", "code": code, "error": error }),
        PreviewEvent::Exit { code, error: None } => json!({ "kind": "exit", "code": code }),
    }
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    if !channel.starts_with("preview:") {
        return None;
    }
    let pane_id = payload
        .get("paneId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 200)
        .map(String::from);
    let cfg = || {
        app.state::<super::config::Store>()
            .0
            .lock()
            .unwrap()
            .get()
            .clone()
    };
    Some(Ok(match channel {
        "preview:detect" => {
            let Some(found) = pane_id.and_then(|p| preview_pane(&cfg(), &p)) else {
                return Some(Ok(Value::Null));
            };
            let dir = resolve_user_path(&found.1, &home_dir());
            tauri::async_runtime::spawn_blocking(move || {
                detect_dev_command(&read_project_files(&dir)).map_or(
                    Value::Null,
                    |(command, source)| json!({ "command": command, "source": source }),
                )
            })
            .await
            .unwrap_or(Value::Null)
        }
        "preview:start" => {
            let Some(pane_id) = pane_id else {
                eprintln!("wraithgrid: refused preview:start: invalid arguments");
                return Some(Ok(fail("Could not start the dev server")));
            };
            // The renderer saves a new command with config:set just before this call; Tauri
            // runs invokes concurrently, so give that save a moment to land.
            let mut found = preview_pane(&cfg(), &pane_id);
            for _ in 0..10 {
                if found.as_ref().is_none_or(|(c, _)| !c.is_empty()) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                found = preview_pane(&cfg(), &pane_id);
            }
            let Some((command, cwd)) = found else {
                return Some(Ok(fail("Not a preview on this computer.")));
            };
            if command.is_empty() {
                return Some(Ok(fail("Set the command that starts the dev server.")));
            }
            let mut env = crate::panels::env::spawn_env();
            // The server is shown here: tools that open a browser tab themselves shouldn't.
            env.insert("BROWSER".into(), "none".into());
            let (file, args) = shell_launch(
                &command,
                cfg!(windows),
                env.get("ComSpec").map(String::as_str),
            );
            let home = home_dir();
            let dir = resolve_user_path(&cwd, &home);
            eprintln!(
                "wraithgrid: preview {pane_id}: {command} in {}",
                contract_home(&dir.to_string_lossy(), &home.to_string_lossy())
            );
            let servers = app.state::<DevServers>();
            let mut servers = servers.0.lock().unwrap();
            // A restart stops the old server first (quietly).
            servers.remove(&pane_id);
            let sink_app = app.clone();
            let sink_pane = pane_id.clone();
            let sink = move |e: PreviewEvent| {
                super::emit(
                    &sink_app,
                    "preview:event",
                    json!({ "paneId": sink_pane, "event": event_json(e) }),
                );
            };
            match DevServer::start(&file, &args, &dir, &env, sink) {
                Ok(server) => {
                    servers.insert(pane_id, server);
                }
                Err(error) => super::emit(
                    app,
                    "preview:event",
                    json!({ "paneId": pane_id, "event": { "kind": "exit", "code": null, "error": error } }),
                ),
            }
            ok()
        }
        "preview:stop" => {
            if let Some(pane_id) = pane_id {
                app.state::<DevServers>().0.lock().unwrap().remove(&pane_id);
            }
            Value::Null
        }
        // The page is an iframe: no web contents of its own to capture.
        "preview:capture" => {
            fail("Screenshots of the preview aren\u{2019}t available in this version.")
        }
        _ => return None,
    }))
}
