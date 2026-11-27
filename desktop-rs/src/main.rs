//! Wraithgrid on Tauri: the 1.26 React renderer (src/renderer) in a native webview. The
//! renderer talks to `window.wraith` (src/tauri/bridge.ts), which sends every IPC channel of
//! src/shared/ipc-channels.ts to the one `ipc` command here; events go back under the same
//! channel names. The work itself lives in the app-rs crates.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ipc;
mod panels;
mod sys;
mod voice;

use serde_json::Value;
use tauri::AppHandle;

/// Every renderer call: `invoke` (awaits the answer) and `send` (fire and forget) alike.
#[tauri::command]
async fn ipc(app: AppHandle, channel: String, payload: Option<Value>) -> Result<Value, String> {
    ipc::route(&app, &channel, payload.unwrap_or(Value::Null)).await
}

/// Calls that must finish before the renderer's next call starts (Electron handled IPC in
/// order; Tauri runs async commands concurrently): a sync command runs inline, before the
/// next message is read. Only for quick handlers that don't need the main thread.
#[tauri::command]
fn ipc_ordered(app: AppHandle, channel: String, payload: Option<Value>) -> Result<Value, String> {
    tauri::async_runtime::block_on(ipc::route(&app, &channel, payload.unwrap_or(Value::Null)))
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
        .invoke_handler(tauri::generate_handler![ipc, ipc_ordered])
        .build(tauri::generate_context!())
        .expect("error while running Wraithgrid")
        .run(|_app, _event| {
            // macOS hands `wraithgrid://` links (Info.plist CFBundleURLTypes) over as an event,
            // not argv, both at launch and while running.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = &_event {
                for url in urls {
                    ipc::links::deliver(_app, url.as_str());
                }
            }
        });
}
