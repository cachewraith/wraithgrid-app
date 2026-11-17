//! Desktop notifications (`showPaneNotice` in app.ts): a pane finished or wants approval;
//! clicking the notification brings the window back at that pane (`pane:reveal`).

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

/// The app icon as a file, for the notification server (written once per version).
fn icon_file() -> Option<PathBuf> {
    const PNG: &[u8] = include_bytes!("../../icons/icon.png");
    let dir = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| wraithgrid_core::paths::home_dir().join(".cache"))
        .join("wraithgrid");
    let file = dir.join(format!("icon-{}.png", env!("CARGO_PKG_VERSION")));
    if !file.is_file() {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&file, PNG).ok()?;
    }
    Some(file)
}

/// Shows the window again, in front.
pub fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn pane_notice(app: &AppHandle, pane_id: String, title: String, body: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.appname("Wraithgrid").summary(&title).body(&body);
        if let Some(icon) = icon_file() {
            n.icon(&icon.to_string_lossy());
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            n.action("default", "Show");
            match n.show() {
                // Blocks this thread until the notification is clicked or closed.
                Ok(handle) => handle.wait_for_action(|action| {
                    if action == "default" {
                        show_window(&app);
                        crate::ipc::emit(&app, "pane:reveal", pane_id.clone());
                    }
                }),
                Err(e) => eprintln!("[wraithgrid] notification: {e}"),
            }
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            let _ = (&app, &pane_id);
            if let Err(e) = n.show() {
                eprintln!("[wraithgrid] notification: {e}");
            }
        }
    });
}
