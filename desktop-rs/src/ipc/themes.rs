//! `themes:*` (src/main/ipc.ts, themes.ts, community-themes.ts) over wraithgrid-themes:
//! the user's theme files in ~/.wraithgrid/themes and the community gallery.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use wraithgrid_core::paths::home_dir;
use wraithgrid_themes::{
    delete_theme, import_theme, list_themes, parse_theme_file, repo_from_url, save_theme,
    theme_file_text, theme_slug, themes_root, CommunityThemes, CommunityThemesDeps,
    THEMES_FILE_MAX,
};

use super::{fail, ok};
use crate::panels::dialog::{self, Pick};

pub struct Community(Arc<RwLock<CommunityThemes>>);

fn dir() -> PathBuf {
    themes_root(&home_dir())
}

pub fn setup(app: &AppHandle) {
    let deps = CommunityThemesDeps::live(dir(), env!("CARGO_PKG_VERSION"));
    app.manage(Community(Arc::new(RwLock::new(CommunityThemes::new(deps)))));
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tauri::async_runtime::spawn_blocking(f).await.ok()
}

fn import_result(r: anyhow::Result<Result<String, String>>, rejected: &str, what: &str) -> Value {
    match r {
        Ok(Ok(id)) => {
            eprintln!("wraithgrid: {what} theme {id}");
            json!({ "ok": true, "id": id })
        }
        Ok(Err(e)) => fail(e),
        Err(e) => {
            eprintln!("wraithgrid: {rejected}: {e}");
            fail(rejected)
        }
    }
}

/// A theme id as the contract allows it: `^[a-z0-9][a-z0-9-]{0,39}$`.
fn theme_id(payload: &Value) -> Option<String> {
    let id = payload.get("id")?.as_str()?;
    let mut chars = id.chars();
    let first = chars.next()?;
    let ok = id.len() <= 40
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    ok.then(|| id.to_string())
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    let community = app.state::<Community>().0.clone();
    let invalid = |rejected: Value| {
        eprintln!("wraithgrid: refused {channel}: invalid arguments");
        Some(Ok(rejected))
    };
    Some(Ok(match channel {
        "themes:list" => blocking(|| list_themes(&dir()))
            .await
            .and_then(|l| serde_json::to_value(l).ok())
            .unwrap_or_else(|| json!({ "themes": [], "problems": [] })),
        "themes:import" => {
            let home = home_dir();
            let Some(file) = dialog::file(
                app,
                "Import theme",
                ("Wraithgrid theme", &["json"]),
                &home,
                Pick::Open,
            )
            .await
            else {
                return Some(Ok(Value::Null));
            };
            blocking(move || import_theme(&file, &dir()))
                .await
                .map(|r| import_result(r, "Could not import the theme", "imported"))
                .unwrap_or_else(|| fail("Could not import the theme"))
        }
        "themes:delete" => {
            let Some(id) = theme_id(&payload) else {
                return invalid(fail("Could not delete the theme"));
            };
            match blocking(move || delete_theme(&id, &dir())).await {
                Some(Ok(true)) => ok(),
                Some(Ok(false)) => fail("Not a theme of yours."),
                Some(Err(e)) => {
                    eprintln!("wraithgrid: themes:delete failed: {e}");
                    fail("Could not delete the theme")
                }
                None => fail("Could not delete the theme"),
            }
        }
        "themes:export" => {
            let theme = match payload.get("theme").map(parse_theme_file) {
                Some(Ok(t)) => t,
                _ => return invalid(fail("Could not export the theme")),
            };
            let home = home_dir();
            let name = format!("{}.json", theme_slug(&theme.name));
            let Some(file) = dialog::file(
                app,
                "Export theme",
                ("Wraithgrid theme", &["json"]),
                &home,
                Pick::Save { file_name: &name },
            )
            .await
            else {
                return Some(Ok(Value::Null));
            };
            let text = theme_file_text(&theme);
            if text.encode_utf16().count() > THEMES_FILE_MAX {
                return Some(Ok(fail("Theme is too large.")));
            }
            match blocking(move || wraithgrid_usage::write_file_atomic(&file, &text)).await {
                Some(Ok(())) => ok(),
                _ => fail("Could not export the theme"),
            }
        }
        "themes:save" => {
            let raw = payload.get("theme").cloned().unwrap_or(Value::Null);
            blocking(move || save_theme(&raw, &dir()))
                .await
                .map(|r| import_result(r, "Could not save the theme", "saved"))
                .unwrap_or_else(|| fail("Could not save the theme"))
        }
        "themes:community" => match blocking(move || community.write().unwrap().list()).await {
            Some(Ok(list)) => {
                let mut v = json!(list);
                v["ok"] = json!(true);
                v
            }
            Some(Err(e)) => fail(e.to_string()),
            None => fail("Could not read the community themes"),
        },
        "themes:communityShot" => {
            let Some(repo) = payload
                .get("repo")
                .and_then(Value::as_str)
                .filter(|r| (3..=300).contains(&r.len()))
            else {
                return invalid(Value::Null);
            };
            let repo = repo.to_string();
            blocking(move || community.read().unwrap().screenshot(&repo))
                .await
                .flatten()
                .map_or(Value::Null, Value::String)
        }
        "themes:communityInstall" => {
            let Some(raw) = payload
                .get("repo")
                .and_then(Value::as_str)
                .filter(|r| (3..=300).contains(&r.len()))
            else {
                return invalid(fail("Could not install the theme"));
            };
            let Some(repo) = repo_from_url(raw) else {
                return Some(Ok(fail(
                    "Give a GitHub repository: owner/repo or its github.com address.",
                )));
            };
            let r = repo.clone();
            match blocking(move || community.read().unwrap().install(&r)).await {
                Some(Ok(Ok(id))) => {
                    eprintln!("wraithgrid: installed community theme {id} from {repo}");
                    json!({ "ok": true, "id": id })
                }
                Some(Ok(Err(e))) => fail(e),
                Some(Err(e)) => {
                    eprintln!("wraithgrid: themes:communityInstall failed: {e}");
                    fail("Could not install the theme")
                }
                None => fail("Could not install the theme"),
            }
        }
        "themes:communityRemove" => {
            let Some(id) = theme_id(&payload) else {
                return invalid(fail("Could not remove the theme"));
            };
            match blocking(move || community.read().unwrap().uninstall(&id)).await {
                Some(Ok(true)) => ok(),
                Some(Ok(false)) => fail("Not a community theme."),
                _ => fail("Could not remove the theme"),
            }
        }
        "themes:openFolder" => {
            let d = dir();
            if std::fs::create_dir_all(&d).is_err() || !crate::panels::open_path(&d) {
                fail("Could not open the themes folder")
            } else {
                ok()
            }
        }
        _ => return None,
    }))
}
