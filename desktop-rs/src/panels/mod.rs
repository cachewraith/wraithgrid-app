//! Helpers for the panel channels (git, usage, update, themes, mods, preview).

pub mod dialog;
pub mod env;
pub mod project_icons;

/// Opens a web link in the user's browser.
pub fn open_external(url: &str) -> bool {
    let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(windows) {
        ("cmd", vec!["/c", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };
    std::process::Command::new(cmd).args(args).spawn().is_ok()
}

/// Opens a folder in the file manager.
pub fn open_path(path: &std::path::Path) -> bool {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd).arg(path).spawn().is_ok()
}
