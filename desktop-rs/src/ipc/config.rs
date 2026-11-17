//! `config:get` / `config:set`: the same config.json the Electron and GPUI apps use.

use std::sync::Mutex;

use serde_json::Value;
use tauri::{AppHandle, Manager};
use wraithgrid_core::config::parse_config;
use wraithgrid_core::paths::{config_file_path, current_user_data_dir};
use wraithgrid_core::store::ConfigStore;

pub struct Store(pub Mutex<ConfigStore>);

pub fn setup(app: &AppHandle) {
    let mut store = ConfigStore::new(config_file_path(&current_user_data_dir()));
    store.load();
    app.manage(Store(Mutex::new(store)));
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    let store = app.state::<Store>();
    Some(match channel {
        "config:get" => {
            let store = store.0.lock().unwrap();
            serde_json::to_value(store.get()).map_err(|e| e.to_string())
        }
        "config:set" => {
            let parsed = match parse_config(&payload, false) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("wraithgrid: refused config:set: {e}");
                    return Some(Ok(super::fail(e)));
                }
            };
            let mut store = store.0.lock().unwrap();
            store.set(parsed);
            store.save();
            Ok(super::ok())
        }
        _ => return None,
    })
}
