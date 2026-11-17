//! `usage:get` and the `usage:limits` event: usage.json (wraithgrid-usage's store) and the
//! live claude limits poll every 2 minutes (src/main/usage-poll.ts, app.ts).
//!
//! The chat module feeds turns and CLI-reported limits in through [`record`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Listener, Manager};
use wraithgrid_core::paths::{current_user_data_dir, home_dir, resolve_user_path};
use wraithgrid_usage::model::{AccountLimits, TurnUsage, UsageWindow};
use wraithgrid_usage::poll::{UsagePollDeps, UsagePoller, POLL_MS};
use wraithgrid_usage::store::{PaneOwner, UsageEvent, UsageStore, UsageStoreDeps};
use wraithgrid_usage::UsageAccount;

pub struct Usage(pub Arc<Mutex<UsageStore>>);

fn owner_of(app: &AppHandle, pane_id: &str) -> Option<PaneOwner> {
    let store = app.state::<super::config::Store>();
    let cfg = store.0.lock().unwrap();
    cfg.get().workspaces.iter().find_map(|w| {
        w.panes.iter().find(|p| p.id == pane_id).map(|p| PaneOwner {
            account_id: p.account_id.clone(),
            workspace_id: Some(w.id.clone()),
            title: p.title.clone(),
        })
    })
}

pub fn setup(app: &AppHandle) {
    let owner_app = app.clone();
    let store = UsageStore::new(UsageStoreDeps::new(
        current_user_data_dir().join("usage.json"),
        Box::new(move |pane| owner_of(&owner_app, pane)),
    ));
    let mut store = store;
    store.load();
    let store = Arc::new(Mutex::new(store));
    app.manage(Usage(store.clone()));

    // Polls each signed-in claude account's limits every 2 minutes.
    let poll_app = app.clone();
    let poll_store = store.clone();
    std::thread::Builder::new()
        .name("usage-poll".into())
        .spawn(move || {
            let mut poller = UsagePoller::new(UsagePollDeps::live());
            loop {
                let accounts = accounts(&poll_app);
                for (account_id, limits) in poller.poll_once(&accounts) {
                    poll_store.lock().unwrap().set_limits(&account_id, limits);
                    super::emit(
                        &poll_app,
                        "usage:limits",
                        json!({ "accountId": account_id, "limits": limits }),
                    );
                }
                poll_store.lock().unwrap().flush();
                std::thread::sleep(Duration::from_millis(POLL_MS as u64));
            }
        })
        .expect("usage poll thread");

    // What chat turns used and the limits the CLI reported (usage-store.ts onChatEvent).
    let chat_app = app.clone();
    app.listen("chat:event", move |e| {
        let Ok(msg) = serde_json::from_str::<Value>(e.payload()) else {
            return;
        };
        let (Some(pane_id), Some(event)) = (msg["paneId"].as_str(), usage_event(&msg["event"]))
        else {
            return;
        };
        record(&chat_app, pane_id, &event);
    });

    // The store's delayed save (1.5 s after a change).
    std::thread::Builder::new()
        .name("usage-save".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            store.lock().unwrap().tick();
        })
        .expect("usage save thread");
}

fn accounts(app: &AppHandle) -> Vec<UsageAccount> {
    let store = app.state::<super::config::Store>();
    let cfg = store.0.lock().unwrap();
    let home = home_dir();
    cfg.get()
        .accounts
        .iter()
        .map(|a| UsageAccount {
            id: a.id.clone(),
            name: a.name.clone(),
            cli: a.cli.as_str().into(),
            signed_in: a.signed_in,
            config_dir: resolve_user_path(&a.config_dir, &home),
        })
        .collect()
}

fn window(v: &Value) -> Option<UsageWindow> {
    Some(UsageWindow {
        utilization: v.get("utilization")?.as_f64().filter(|n| n.is_finite())?,
        resets_at: v.get("resetsAt")?.as_f64().filter(|n| n.is_finite())? as i64,
    })
}

/// A ChatEvent (src/shared/chat.ts) usage cares about: `limits` and `turnEnd`.
fn usage_event(e: &Value) -> Option<UsageEvent> {
    let num = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0)
    };
    match e.get("t")?.as_str()? {
        "limits" => {
            let l = e.get("limits")?;
            Some(UsageEvent::Limits(AccountLimits {
                five_hour: l.get("fiveHour").and_then(window),
                seven_day: l.get("sevenDay").and_then(window),
                at: l.get("at")?.as_f64()? as i64,
            }))
        }
        "turnEnd" => Some(UsageEvent::TurnEnd(
            e.get("usage").filter(|u| u.is_object()).map(|u| TurnUsage {
                model: u.get("model").and_then(Value::as_str).map(String::from),
                cost_usd: num(u, "costUsd"),
                input: num(u, "input").unwrap_or(0.0) as u64,
                output: num(u, "output").unwrap_or(0.0) as u64,
                cache_read: num(u, "cacheRead").unwrap_or(0.0) as u64,
                cache_write: num(u, "cacheWrite").unwrap_or(0.0) as u64,
            }),
        )),
        _ => None,
    }
}

/// A chat pane's `limits` or `turnEnd` event into the usage store (also called from the
/// `chat:event` listener above).
pub fn record(app: &AppHandle, pane_id: &str, event: &UsageEvent) {
    if let Some(usage) = app.try_state::<Usage>() {
        usage.0.lock().unwrap().on_chat_event(pane_id, event);
    }
}

/// Writes a pending save now (quit).
#[allow(dead_code)]
pub fn flush(app: &AppHandle) {
    if let Some(usage) = app.try_state::<Usage>() {
        usage.0.lock().unwrap().flush();
    }
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    _payload: Value,
) -> Option<Result<Value, String>> {
    match channel {
        "usage:get" => {
            let data = app.state::<Usage>().0.lock().unwrap().get();
            Some(serde_json::to_value(data).map_err(|e| e.to_string()))
        }
        _ => None,
    }
}
