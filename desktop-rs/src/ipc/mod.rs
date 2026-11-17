//! The IPC surface (src/shared/ipc-channels.ts), one module per channel group. Each module
//! has `setup` (its managed state, background work) and `handle` (its channels).

mod chat;
mod config;
mod files;
mod git;
mod misc;
mod mods;
mod preview;
mod pty;
mod themes;
mod update;
mod usage;
mod window;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

pub fn setup(app: &AppHandle) {
    config::setup(app);
    window::setup(app);
    pty::setup(app);
    chat::setup(app);
    files::setup(app);
    git::setup(app);
    usage::setup(app);
    update::setup(app);
    themes::setup(app);
    mods::setup(app);
    preview::setup(app);
    misc::setup(app);
}

pub async fn route(app: &AppHandle, channel: &str, payload: Value) -> Result<Value, String> {
    let group = channel.split(':').next().unwrap_or_default();
    let answer = match group {
        "config" => config::handle(app, channel, payload).await,
        "window" | "app" => window::handle(app, channel, payload).await,
        "shell" if channel == "shell:openExternal" => window::handle(app, channel, payload).await,
        "pty" => pty::handle(app, channel, payload).await,
        "chat" => chat::handle(app, channel, payload).await,
        "dialog" | "fs" => files::handle(app, channel, payload).await,
        "git" | "project" => git::handle(app, channel, payload).await,
        "usage" => usage::handle(app, channel, payload).await,
        "update" => update::handle(app, channel, payload).await,
        "themes" => themes::handle(app, channel, payload).await,
        "mods" => mods::handle(app, channel, payload).await,
        "preview" => preview::handle(app, channel, payload).await,
        "debug" => {
            eprintln!(
                "renderer: {}",
                payload.get("message").and_then(Value::as_str).unwrap_or("")
            );
            if let Some(stack) = payload.get("stack").and_then(Value::as_str) {
                eprintln!("{stack}");
            }
            Some(Ok(Value::Null))
        }
        // login, claude, account, shared, shell:list, clipboard, notify, pane, machine, cli
        _ => misc::handle(app, channel, payload).await,
    };
    match answer {
        Some(result) => result,
        None => {
            eprintln!("wraithgrid: channel not ported yet: {channel}");
            Ok(Value::Null)
        }
    }
}

/// Sends an event to the renderer under its IPC channel name (`subscribe` in bridge.ts).
pub fn emit(app: &AppHandle, channel: &str, payload: impl serde::Serialize + Clone) {
    let _ = app.emit(channel, payload);
}

/// `SimpleResult` of ipc-contract.ts.
pub fn ok() -> Value {
    json!({ "ok": true })
}

pub fn fail(error: impl Into<String>) -> Value {
    json!({ "ok": false, "error": error.into() })
}
