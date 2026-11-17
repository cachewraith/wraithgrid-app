//! Native open/save dialogs for themes import/export, through tauri-plugin-dialog (registered
//! by the files group), parented to the main window.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_dialog::{DialogExt, FilePath};

/// What the dialog asks for.
pub enum Pick<'a> {
    Open,
    Save { file_name: &'a str },
}

/// Shows the dialog and waits for the answer; None when cancelled or no dialog came up.
pub async fn file(
    app: &AppHandle,
    title: &str,
    filter: (&str, &[&str]),
    dir: &Path,
    pick: Pick<'_>,
) -> Option<PathBuf> {
    if app.try_state::<tauri_plugin_dialog::Dialog<Wry>>().is_none() {
        let _ = app.plugin(tauri_plugin_dialog::init());
    }
    let mut builder = app
        .dialog()
        .file()
        .set_title(title)
        .set_directory(dir)
        .add_filter(filter.0, filter.1);
    if let Some(win) = app.get_webview_window("main") {
        builder = builder.set_parent(&win);
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<FilePath>>();
    match pick {
        Pick::Open => builder.pick_file(move |p| {
            let _ = tx.send(p);
        }),
        Pick::Save { file_name } => builder.set_file_name(file_name).save_file(move |p| {
            let _ = tx.send(p);
        }),
    }
    rx.await.ok().flatten()?.into_path().ok()
}
