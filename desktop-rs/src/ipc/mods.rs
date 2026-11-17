//! `mods:*` (the mod handlers of src/main/ipc.ts, mods.ts, mod-community.ts) over the
//! wraithgrid-mods crate's `ModManager`: each claude account's own `claude plugin` command,
//! the claudemod.com community list, and downloaded mods in ~/.wraithgrid/mods.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use wraithgrid_core::config::Config;
use wraithgrid_core::paths::home_dir;
use wraithgrid_mods::catalog::{clone_mod, pull_mod};
use wraithgrid_mods::mods::ClaudeCli;
use wraithgrid_mods::{
    CatalogMod, CommunityCatalog, FailedAccount, ModChange, ModDeps, ModManager,
};

use super::{fail, ok};
use crate::panels::env;

pub struct Mods {
    manager: Arc<ModManager>,
    /// The last community list: a download must name one of its bare-plugin mods.
    community: Arc<Mutex<Vec<CatalogMod>>>,
}

pub fn setup(app: &AppHandle) {
    app.manage(Mods {
        manager: Arc::new(ModManager::new(
            home_dir(),
            CommunityCatalog::new(wraithgrid_mods::mod_community::ureq_fetch),
        )),
        community: Arc::default(),
    });
}

fn config(app: &AppHandle) -> Config {
    app.state::<super::config::Store>()
        .0
        .lock()
        .unwrap()
        .get()
        .clone()
}

/// Runs `f` with the mods crate's view of the app: accounts, claude, each account's env.
fn with_deps<R>(config: &Config, f: impl FnOnce(&ModDeps<'_>) -> R) -> R {
    let base = env::spawn_env();
    let claude = env::claude_binary(config, &base).map(ClaudeCli::new);
    let accounts = env::accounts(config);
    let env_for = |a: &wraithgrid_mods::Account| env::account_env(config, &base, a);
    let log = |m: &str| eprintln!("wraithgrid: {m}");
    let deps = ModDeps {
        accounts: &accounts,
        claude: claude.as_ref().map(|c| c as _),
        env_for: &env_for,
        log: &log,
    };
    f(&deps)
}

fn failed(r: Result<Vec<FailedAccount>, String>) -> Value {
    match r {
        Ok(failed) => json!({ "ok": true, "failed": failed }),
        Err(e) => fail(e),
    }
}

fn str_arg(payload: &Value, key: &str, max: usize) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= max)
        .map(String::from)
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    if !channel.starts_with("mods:") {
        return None;
    }
    let state = app.state::<Mods>();
    let manager = state.manager.clone();
    let community = state.community.clone();
    let cfg = config(app);
    let rejected = match channel {
        "mods:list" => json!([]),
        "mods:setEnabled" | "mods:change" => fail("Could not change the mod"),
        "mods:catalog" => fail("Could not read the mod catalog"),
        "mods:community" => fail("Could not read claudemod.com"),
        "mods:install" => fail("Could not install the mod"),
        "mods:clone" => fail("Could not download the mod"),
        "mods:pull" => fail("Could not update the mod"),
        "mods:marketplaces" => fail("Could not list the marketplaces"),
        "mods:marketplaceAdd" => fail("Could not add the marketplace"),
        "mods:marketplaceRemove" => fail("Could not remove the marketplace"),
        _ => return None,
    };
    let invalid = || {
        eprintln!("wraithgrid: refused {channel}: invalid arguments");
        Some(Ok(rejected.clone()))
    };
    let job: Box<dyn FnOnce() -> Value + Send> = match channel {
        "mods:list" => Box::new(move || {
            with_deps(&cfg, |d| {
                let rows: Vec<Value> = manager
                    .list(d)
                    .into_iter()
                    .map(|a| match a.mods {
                        Ok(mods) => json!({ "accountId": a.account_id, "ok": true, "mods": mods }),
                        Err(e) => json!({ "accountId": a.account_id, "ok": false, "error": e }),
                    })
                    .collect();
                json!(rows)
            })
        }),
        "mods:setEnabled" => {
            let (Some(account), Some(id), Some(scope), Some(enabled)) = (
                str_arg(&payload, "accountId", 200),
                str_arg(&payload, "id", 200),
                str_arg(&payload, "scope", 32),
                payload.get("enabled").and_then(Value::as_bool),
            ) else {
                return invalid();
            };
            let project = match payload.get("projectPath") {
                Some(Value::String(p)) if p.len() <= 4096 => Some(p.clone()),
                Some(Value::Null) => None,
                _ => return invalid(),
            };
            Box::new(move || {
                with_deps(&cfg, |d| {
                    match manager.set_enabled(d, &account, &id, &scope, project.as_deref(), enabled)
                    {
                        Ok(()) => ok(),
                        Err(e) => fail(e),
                    }
                })
            })
        }
        "mods:catalog" => Box::new(move || {
            with_deps(&cfg, |d| match manager.catalog(d) {
                Ok(mods) => json!({ "ok": true, "mods": mods }),
                Err(e) => fail(e),
            })
        }),
        "mods:community" => Box::new(move || match manager.community() {
            Ok(mods) => {
                *community.lock().unwrap() = mods.clone();
                json!({ "ok": true, "mods": mods })
            }
            Err(e) => fail(e),
        }),
        "mods:install" => {
            let Some(id) = str_arg(&payload, "id", 200) else {
                return invalid();
            };
            Box::new(move || with_deps(&cfg, |d| failed(manager.install(d, &id))))
        }
        "mods:change" => {
            let id = str_arg(&payload, "id", 200);
            let change = match payload.get("change").and_then(Value::as_str) {
                Some("uninstall") => Some(ModChange::Uninstall),
                Some("update") => Some(ModChange::Update),
                _ => None,
            };
            let (Some(id), Some(change)) = (id, change) else {
                return invalid();
            };
            Box::new(move || with_deps(&cfg, |d| failed(manager.change(d, &id, change))))
        }
        "mods:marketplaces" => Box::new(move || {
            with_deps(&cfg, |d| match manager.marketplaces(d) {
                Ok(list) => {
                    json!({ "ok": true, "marketplaces": list.marketplaces, "total": list.total })
                }
                Err(e) => fail(e),
            })
        }),
        "mods:marketplaceAdd" => {
            let Some(source) = str_arg(&payload, "source", 500) else {
                return invalid();
            };
            Box::new(move || with_deps(&cfg, |d| failed(manager.add_marketplace(d, &source))))
        }
        "mods:marketplaceRemove" => {
            let Some(name) = str_arg(&payload, "name", 120) else {
                return invalid();
            };
            Box::new(move || with_deps(&cfg, |d| failed(manager.remove_marketplace(d, &name))))
        }
        "mods:clone" => {
            let Some(id) = str_arg(&payload, "id", 200) else {
                return invalid();
            };
            Box::new(move || {
                let offered = community
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|m| m.id == id && m.clone.is_some())
                    .cloned();
                let Some(m) = offered else {
                    return fail("That mod is no longer offered. Refresh the list.");
                };
                match clone_mod(&home_dir(), &m, &env::spawn_env().into_iter().collect()) {
                    Ok(dir) => {
                        eprintln!(
                            "wraithgrid: downloaded mod {} from {}",
                            m.name,
                            m.clone.as_deref().unwrap_or_default()
                        );
                        json!({ "ok": true, "dir": dir })
                    }
                    Err(e) => fail(e),
                }
            })
        }
        "mods:pull" => {
            let Some(name) = str_arg(&payload, "name", 120) else {
                return invalid();
            };
            Box::new(move || {
                match pull_mod(&home_dir(), &name, &env::spawn_env().into_iter().collect()) {
                    Ok(changed) => {
                        if changed {
                            eprintln!("wraithgrid: updated mod {name}");
                        }
                        json!({ "ok": true, "changed": changed })
                    }
                    Err(e) => fail(e),
                }
            })
        }
        _ => return None,
    };
    Some(Ok(tauri::async_runtime::spawn_blocking(job)
        .await
        .unwrap_or(rejected)))
}
