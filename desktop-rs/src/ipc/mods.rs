//! Channels of this group (src/shared/ipc-channels.ts); not ported yet.

use serde_json::Value;
use tauri::AppHandle;

pub fn setup(_app: &AppHandle) {}

pub async fn handle(
    _app: &AppHandle,
    _channel: &str,
    _payload: Value,
) -> Option<Result<Value, String>> {
    None
}
