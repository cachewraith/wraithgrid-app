//! Wraithgrid on GPUI (#166, ADR 0005): the same app as the Electron one, reading the same
//! config and account folders and attaching to the same background host, so updating from
//! the Electron build needs no setup.
// A GUI-subsystem binary on Windows, as the Electron one (no console window beside the app);
// debug builds keep the console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod a11y;
mod about_view;
mod accounts_view;
mod branch_picker;
mod changes;
mod chat_view;
mod cli_tty;
mod dialogs;
mod folder_picker;
mod keys;
mod login_dialog;
mod machines_row;
mod mods_glue;
mod mods_view;
mod preview_view;
mod project_view;
mod runtime;
mod scheduled_view;
mod settings;
mod shell;
mod shortcuts_sheet;
mod team_section;
mod terminal;
mod theme;
mod theme_editor;
mod themes_gallery;
mod tray;
mod usage_view;

use std::sync::Arc;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use wraithgrid_core::paths::{config_file_path, current_user_data_dir};
use wraithgrid_core::platform::MIN_WINDOW;
use wraithgrid_core::store::ConfigStore;

use wraithgrid_core::cli::{is_cli_command, run_cli, CliDeps};
use wraithgrid_core::config::parse_config;
use wraithgrid_core::host::client::connect_socket;
use wraithgrid_core::host::paths::current_host_paths;

use runtime::{electron_running, pane_host, resolve_path_in_background, Router};
use shell::Shell;

fn main() {
    runtime::force_gdk_x11();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if is_cli_command(argv.first().map(String::as_str)) {
        std::process::exit(run_command_line(&argv));
    }
    let user_data = current_user_data_dir();
    let blocked = electron_running(&user_data).map(|pid| {
        format!(
            "The Electron version of Wraithgrid is running (pid {pid}) on {}. Quit it first: both apps write the same config.",
            user_data.display()
        )
    });
    let mut second_launches = None;
    let mut updated = false;
    if blocked.is_none() {
        match runtime::single_instance(&user_data) {
            runtime::Instance::Second => return,
            runtime::Instance::First(rx) => second_launches = rx,
        }
        resolve_path_in_background();
        updated = about_view::record_launch().is_some();
    }
    let mut store = ConfigStore::new(config_file_path(&user_data));
    // Beside a running Electron app nothing is read: a corrupt file would be moved aside.
    if blocked.is_none() {
        store.load();
    }
    let router = Arc::new(Router::default());
    let (host, _paths) = pane_host(&user_data, router.clone());
    if blocked.is_none() {
        // Join (or start) the host now, so `wraithgrid` commands work before any pane exists.
        let host = host.clone();
        std::thread::spawn(move || {
            if let Err(err) = host.connect() {
                eprintln!("[wraithgrid] host: {err}");
            }
        });
    }

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        cx.set_app_identity("dev.cachewraith.wraithgrid", "Wraithgrid");
        Theme::change(ThemeMode::Dark, None, cx);
        let mono = terminal::resolve_mono(store.get().settings.font_family.as_str(), cx);
        cx.set_global(terminal::MonoFont(mono));
        let shell = cx.new(|cx| {
            let mut shell = Shell::new(store, host, router, blocked);
            if updated {
                shell.updated_to = Some(runtime::APP_VERSION.to_string());
            }
            shell.listen_for_commands(cx);
            shell.check_updates_on_launch(cx);
            shell.start_usage(cx);
            if let Some(rx) = second_launches {
                shell.listen_for_second_launch(rx, cx);
            }
            shell
        });
        // The window may close while the app keeps running in the tray (`keepInTray`).
        cx.set_quit_mode(QuitMode::Explicit);
        cx.on_window_closed({
            let shell = shell.clone();
            move |cx, _| {
                if !shell.read(cx).in_tray() {
                    shell.update(cx, |s, cx| {
                        s.flush();
                        s.flush_chats(cx);
                    });
                    cx.quit();
                }
            }
        })
        .detach();
        open_main_window(shell, cx);
        cx.activate(true);
    });
}

/// The window, around the shell: at launch, and again when the tray opens it.
pub fn open_main_window(shell: Entity<Shell>, cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1280.), px(800.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Wraithgrid".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(11.))),
        }),
        // The Electron app's Wayland app id, so existing window rules keep working.
        app_id: Some("wraithgrid".into()),
        window_min_size: Some(size(px(MIN_WINDOW.0), px(MIN_WINDOW.1))),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        shell.update(cx, |s, cx| s.attach_window(window, cx));
        // A close from the compositor (WM close, Alt+F4) asks like the close button does;
        // the shell quits (or goes to the tray) once the user chose.
        let asks = shell.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            asks.update(cx, |s, cx| s.request_close(window, cx))
                .is_err()
        });
        shell
    })
    .expect("open window");
    cx.activate(true);
}

/// `wraithgrid <command>`: talks to the host and exits, without opening a window.
fn run_command_line(argv: &[String]) -> i32 {
    let user_data = current_user_data_dir();
    let paths = current_host_paths(&user_data);
    let (socket, token) = (paths.socket.clone(), paths.token.clone());
    let config_file = config_file_path(&user_data);
    run_cli(
        argv,
        CliDeps {
            connect: Box::new(move || connect_socket(&socket)),
            read_token: Box::new(move || std::fs::read_to_string(&token)),
            read_config: Box::new(move || {
                let text = std::fs::read_to_string(&config_file).ok()?;
                parse_config(&serde_json::from_str(&text).ok()?, true).ok()
            }),
            app_version: runtime::APP_VERSION.into(),
            cwd: std::env::current_dir().unwrap_or_default(),
            out: Box::new(|t| println!("{t}")),
            err: Box::new(|t| eprintln!("{t}")),
            sleep: Box::new(std::thread::sleep),
            tty: cli_tty::local_tty(),
        },
    )
}
