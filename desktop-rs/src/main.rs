//! Wraithgrid on Tauri: the 1.26 React renderer (src/renderer) in a native webview. The
//! renderer talks to `window.wraith` (src/tauri/bridge.ts), which sends every IPC channel of
//! src/shared/ipc-channels.ts to the one `ipc` command here; events go back under the same
//! channel names. The work itself lives in the app-rs crates.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ipc;

use serde_json::Value;
use tauri::AppHandle;

/// Every renderer call: `invoke` (awaits the answer) and `send` (fire and forget) alike.
#[tauri::command]
async fn ipc(app: AppHandle, channel: String, payload: Option<Value>) -> Result<Value, String> {
    ipc::route(&app, &channel, payload.unwrap_or(Value::Null)).await
}

fn main() {
    // WebKitGTK's DMA-BUF renderer draws blank windows on some Wayland + NVIDIA setups.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: first thing in main, before any thread starts.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    tauri::Builder::default()
        .setup(|app| {
            ipc::setup(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![ipc])
        .run(tauri::generate_context!())
        .expect("error while running Wraithgrid");
}
