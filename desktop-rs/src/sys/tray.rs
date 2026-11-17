//! The tray icon (`keepInTray` in app.ts): shown while the window is closed but agents (or
//! scheduled jobs) keep the app running. Its menu says how many agents run and opens the
//! window again or quits. Tauri's tray: StatusNotifierItem/AppIndicator on Linux.

use std::sync::{Arc, Mutex};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Wry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Open,
    StopAndQuit,
    QuitKeep,
}

pub struct Tray {
    _icon: TrayIcon<Wry>,
    count: MenuItem<Wry>,
}

fn count_text(live: usize) -> String {
    format!("{live} agent{} running", if live == 1 { "" } else { "s" })
}

impl Tray {
    pub fn show(app: &AppHandle, on: Arc<dyn Fn(TrayCommand) + Send + Sync>) -> Option<Tray> {
        let build = || -> tauri::Result<Tray> {
            let count = MenuItem::with_id(app, "count", count_text(0), false, None::<&str>)?;
            let open = MenuItem::with_id(app, "open", "Open Wraithgrid", true, None::<&str>)?;
            let stop =
                MenuItem::with_id(app, "stop", "Stop all agents and quit", true, None::<&str>)?;
            let keep =
                MenuItem::with_id(app, "keep", "Quit, keep agents running", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&count, &sep, &open, &stop, &keep])?;
            let (on_menu, on_click) = (on.clone(), on.clone());
            let mut builder = TrayIconBuilder::with_id("wraithgrid")
                .tooltip("Wraithgrid: agents running")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |_, e| match e.id().as_ref() {
                    "open" => on_menu(TrayCommand::Open),
                    "stop" => on_menu(TrayCommand::StopAndQuit),
                    "keep" => on_menu(TrayCommand::QuitKeep),
                    _ => {}
                })
                .on_tray_icon_event(move |_, e| {
                    if let TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = e
                    {
                        on_click(TrayCommand::Open);
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                builder = builder.icon(icon.clone());
            }
            Ok(Tray {
                _icon: builder.build(app)?,
                count,
            })
        };
        match build() {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("[wraithgrid] tray: {e}");
                None
            }
        }
    }

    pub fn set_live(&self, live: usize) {
        let _ = self.count.set_text(count_text(live));
    }
}

/// The tray while it is up; dropping it removes the icon.
pub type Slot = Mutex<Option<Tray>>;

pub fn hide(app: &AppHandle, slot: &Slot) {
    slot.lock().unwrap().take();
    let _ = app.remove_tray_by_id("wraithgrid");
}
