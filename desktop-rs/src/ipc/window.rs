//! The window (app.ts's createWindow and shutdown), its controls, `app:info` and
//! `shell:openExternal`. One app per user-data dir: a second launch shows the first one's
//! window. Closing the window decides what happens to running agents (settings.onWindowClose:
//! ask, keep them running in the background with a tray icon, or stop them and quit).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, WebviewWindowBuilder, WindowEvent};
use wraithgrid_core::pane_env::Os;
use wraithgrid_core::paths::{config_file_path, contract_home, current_user_data_dir, home_dir};
use wraithgrid_core::platform::{detect_desktop, Chrome};
use wraithgrid_core::types::CloseMode;

use super::config;
use crate::sys::native::{message_box, MessageBox};
use crate::sys::notify::show_window;
use crate::sys::panes::Panes;
use crate::sys::tray::{self, Tray, TrayCommand};

/// Window state for the close flow.
#[derive(Default)]
pub struct Closing {
    /// A close is being decided (the dialog is up): further close requests wait.
    deciding: AtomicBool,
    /// Quitting: the window may go.
    quitting: AtomicBool,
    tray: tray::Slot,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    Stop,
    Keep,
}

pub fn setup(app: &AppHandle) {
    single_instance(app);
    app.manage(Arc::new(Closing::default()));
    let Some(conf) = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .cloned()
    else {
        eprintln!("wraithgrid: no main window in tauri.conf.json");
        return;
    };
    // The window follows the page's title (TitleBar sets it), as Electron's does.
    let built = WebviewWindowBuilder::from_config(app, &conf).and_then(|b| {
        b.inner_size(1440.0, 900.0)
            .min_inner_size(640.0, 420.0)
            .on_document_title_changed(|w, title| {
                let _ = w.set_title(&title);
            })
            .build()
    });
    let window = match built {
        Ok(w) => w,
        Err(e) => {
            eprintln!("wraithgrid: could not open the window: {e}");
            return;
        }
    };
    let handle = app.clone();
    window.on_window_event(move |e| {
        if let WindowEvent::CloseRequested { api, .. } = e {
            let closing = handle.state::<Arc<Closing>>();
            if closing.quitting.load(Ordering::SeqCst) {
                return;
            }
            api.prevent_close();
            if closing.deciding.swap(true, Ordering::SeqCst) {
                return;
            }
            let app = handle.clone();
            std::thread::spawn(move || {
                let choice = choose_on_close(&app);
                app.state::<Arc<Closing>>()
                    .deciding
                    .store(false, Ordering::SeqCst);
                match choice {
                    None => {}
                    Some(Intent::Keep) => keep_in_tray(&app),
                    Some(Intent::Stop) => quit(&app, Intent::Stop),
                }
            });
        }
    });
}

/// One app per user-data dir (`requestSingleInstanceLock`): a second launch asks the first
/// to show its window over a socket in the user-data dir and exits. The GPUI app uses the
/// same socket, so it and this one never run on the same config either. A launch carrying a
/// `wraithgrid://` link (links.rs) hands it over as `link <url>` instead of `open`.
#[cfg(unix)]
fn single_instance(app: &AppHandle) {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    let path = current_user_data_dir().join("wraithgrid-window.sock");
    let hello = match super::links::from_args(std::env::args()) {
        Some(link) => format!("link {link}\n"),
        None => "open\n".into(),
    };
    if let Ok(mut s) = UnixStream::connect(&path) {
        if s.write_all(hello.as_bytes()).is_ok() {
            eprintln!("wraithgrid: already running; showing its window");
            std::process::exit(0);
        }
    }
    // Left by an app that ended without cleaning up.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::create_dir_all(current_user_data_dir());
    let Ok(listener) = UnixListener::bind(&path) else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_err() {
                continue;
            }
            let line = line.trim();
            if line == "open" {
                reopen_window(&app);
            } else if let Some(link) = line.strip_prefix("link ") {
                super::links::deliver(&app, link);
            }
        }
    });
}

#[cfg(not(unix))]
fn single_instance(_app: &AppHandle) {}

fn config_of(app: &AppHandle) -> wraithgrid_core::config::Config {
    app.state::<config::Store>().0.lock().unwrap().get().clone()
}

/// Terminal panes still running, plus chats mid-turn (`runningAgents`).
fn running_agents(app: &AppHandle) -> usize {
    app.state::<Arc<Panes>>().local().live_count() + super::chat::busy_count(app)
}

/// Asks what to do with running agents, unless there are none or the choice is saved.
fn choose_on_close(app: &AppHandle) -> Option<Intent> {
    let live = running_agents(app);
    let cfg = config_of(app);
    // No agents, but scheduled jobs: the tray icon stays so they still run (#77).
    if live == 0 {
        return Some(if cfg.schedules.iter().any(|j| j.enabled) {
            Intent::Keep
        } else {
            Intent::Stop
        });
    }
    match cfg.settings.on_window_close {
        CloseMode::Keep => return Some(Intent::Keep),
        CloseMode::Stop => return Some(Intent::Stop),
        CloseMode::Ask => {}
    }
    let message = format!(
        "{live} agent{} running",
        if live == 1 { " is" } else { "s are" }
    );
    let a = message_box(
        app,
        MessageBox {
            message: &message,
            detail: "Keep them running in the background, and Wraithgrid shows them again, screen and all, next time it opens. Or stop them now.",
            buttons: &["Keep running", "Stop agents and quit", "Cancel"],
            default: 0,
            cancel: 2,
            checkbox: Some("Remember my choice (Settings → When the window closes)"),
        },
    );
    let choice = match a.button {
        0 => Intent::Keep,
        1 => Intent::Stop,
        _ => return None,
    };
    if a.checked {
        let (mode, name) = match choice {
            Intent::Keep => (CloseMode::Keep, "keep"),
            Intent::Stop => (CloseMode::Stop, "stop"),
        };
        {
            let store = app.state::<config::Store>();
            let mut store = store.0.lock().unwrap();
            store.update(|c| c.settings.on_window_close = mode);
            store.save();
        }
        super::emit(app, "window:closeMode", name);
    }
    Some(choice)
}

/// Hides the window but keeps this process (and a tray icon) while agents run. The page
/// stays loaded, so its panes and chats are as they were when it comes back.
fn keep_in_tray(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    let closing = app.state::<Arc<Closing>>().inner().clone();
    {
        let mut slot = closing.tray.lock().unwrap();
        if slot.is_none() {
            let handle = app.clone();
            *slot = Tray::show(
                app,
                Arc::new(move |c| {
                    let app = handle.clone();
                    std::thread::spawn(move || match c {
                        TrayCommand::Open => reopen_window(&app),
                        TrayCommand::StopAndQuit => quit(&app, Intent::Stop),
                        TrayCommand::QuitKeep => quit(&app, Intent::Keep),
                    });
                }),
            );
            if slot.is_none() {
                // No tray to come back through: leave with the agents running.
                drop(slot);
                return quit(app, Intent::Keep);
            }
        } else {
            return;
        }
    }
    let app = app.clone();
    std::thread::spawn(move || loop {
        let live = running_agents(&app);
        {
            let slot = closing.tray.lock().unwrap();
            let Some(t) = slot.as_ref() else { return };
            t.set_live(live);
        }
        let shown = app
            .get_webview_window("main")
            .is_some_and(|w| w.is_visible().unwrap_or(false));
        let jobs = config_of(&app).schedules.iter().any(|j| j.enabled);
        if live == 0 && !shown && !jobs {
            // Nothing left to keep running: leave quietly.
            return quit(&app, Intent::Stop);
        }
        std::thread::sleep(Duration::from_secs(5));
    });
}

pub(super) fn reopen_window(app: &AppHandle) {
    let closing = app.state::<Arc<Closing>>();
    tray::hide(app, &closing.tray);
    show_window(app);
}

/// Stops (or leaves running) the agents, then quits (`cleanup` + app.quit). Runs on a plain
/// thread (it blocks on the host and on the chat views' saves), never on the async runtime.
fn quit(app: &AppHandle, intent: Intent) {
    let closing = app.state::<Arc<Closing>>();
    if closing.quitting.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::block_on(super::chat::shutdown(app));
    let panes = app.state::<Arc<Panes>>();
    match intent {
        Intent::Stop => panes.local().stop_all(),
        Intent::Keep => panes.local().detach(),
    }
    panes.detach_remotes();
    app.state::<Arc<crate::sys::login::Logins>>().stop_all();
    {
        let store = app.state::<config::Store>();
        let mut store = store.0.lock().unwrap();
        if store.is_dirty() {
            store.save();
        }
    }
    tray::hide(app, &closing.tray);
    app.exit(0);
}

/// What the page should draw. This window is frameless on every OS (`decorations: false`),
/// so unlike Electron's (Windows overlay, macOS traffic lights) no OS buttons exist: Windows
/// and macOS get our own minimize/maximize/close. Tiling compositors keep close only.
fn chrome_name(c: Chrome) -> &'static str {
    match c {
        Chrome::Custom | Chrome::Overlay | Chrome::Mac => "custom",
        Chrome::Tiling => "tiling",
    }
}

fn app_info() -> Value {
    let home = home_dir().to_string_lossy().into_owned();
    let desktop = detect_desktop(Os::current(), |k| std::env::var(k).ok());
    let config = config_file_path(&current_user_data_dir());
    json!({
        "homeDir": home,
        "platform": std::env::consts::OS.replace("macos", "darwin").replace("windows", "win32"),
        "configPath": contract_home(&config.to_string_lossy(), &home),
        "version": env!("CARGO_PKG_VERSION"),
        "chrome": chrome_name(desktop.chrome),
        "desktop": desktop.desktop,
    })
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    if channel == "app:info" {
        return Some(Ok(app_info()));
    }
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
        // Goes through CloseRequested: the same choice as the window manager's close.
        "window:close" => window
            .close()
            .map(|_| Value::Null)
            .map_err(|e| e.to_string()),
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

pub(crate) fn open_url(url: &str) -> bool {
    let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(windows) {
        ("cmd", vec!["/c", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };
    std::process::Command::new(cmd).args(args).spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frameless_window_draws_its_own_buttons_on_every_os() {
        assert_eq!(chrome_name(Chrome::Overlay), "custom");
        assert_eq!(chrome_name(Chrome::Mac), "custom");
        assert_eq!(chrome_name(Chrome::Custom), "custom");
        assert_eq!(chrome_name(Chrome::Tiling), "tiling");
    }
}
