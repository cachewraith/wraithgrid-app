//! Window controls, `app:info` and `shell:openExternal`.

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

pub fn setup(_app: &AppHandle) {}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    let window = app.get_webview_window("main")?;
    Some(match channel {
        "window:minimize" => window
            .minimize()
            .map(|_| Value::Null)
            .map_err(|e| e.to_string()),
        "window:maximize" => {
            let max = window.is_maximized().unwrap_or(false);
            if max {
                window.unmaximize()
            } else {
                window.maximize()
            }
            .map(|_| Value::Null)
            .map_err(|e| e.to_string())
        }
        "window:close" => window
            .close()
            .map(|_| Value::Null)
            .map_err(|e| e.to_string()),
        "app:info" => Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "platform": std::env::consts::OS.replace("macos", "darwin").replace("windows", "win32"),
        })),
        "shell:openExternal" => {
            let url = payload
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if url.starts_with("https://") || url.starts_with("http://") {
                Ok(json!({ "ok": open_url(url) }))
            } else {
                Ok(super::fail("Only web links open outside."))
            }
        }
        _ => return None,
    })
}

fn open_url(url: &str) -> bool {
    let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(windows) {
        ("cmd", vec!["/c", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };
    std::process::Command::new(cmd).args(args).spawn().is_ok()
}
