//! Where each chat pane's conversation view is saved (`<chat dir>/<pane id>.json`), ported
//! from the chat load/save/stop handlers of `src/main/ipc.ts`. The view is the thread as
//! [`crate::thread::ChatThread::serialize`] wrote it; the conversation itself stays in the
//! CLI's own transcript and is resumed by its session id.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::types::CHAT_SAVE_MAX;

/// A pane id is a plain token (`idSchema`): it names a file.
fn valid_pane_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The pane's view file, or None for an id that is not a plain token.
pub fn view_file(dir: &Path, pane_id: &str) -> Option<PathBuf> {
    valid_pane_id(pane_id).then(|| dir.join(format!("{pane_id}.json")))
}

/// The saved view, or None when there is none (or it can't be read).
pub fn load_view(dir: &Path, pane_id: &str) -> Option<String> {
    fs::read_to_string(view_file(dir, pane_id)?).ok()
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

/// Saves the view atomically (a temp file renamed over it) in a folder only the user reads.
pub fn save_view(dir: &Path, pane_id: &str, data: &str) -> io::Result<()> {
    let file = view_file(dir, pane_id).ok_or_else(|| invalid("not a pane id"))?;
    if data.len() > CHAT_SAVE_MAX {
        return Err(invalid("the view is too large to save"));
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    let tmp = dir.join(format!(".{pane_id}.json.{}.tmp", std::process::id()));
    fs::write(&tmp, data)?;
    fs::rename(&tmp, &file).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Deletes the saved view (a closed pane that is not kept); a missing file is fine.
pub fn delete_view(dir: &Path, pane_id: &str) -> io::Result<()> {
    let Some(file) = view_file(dir, pane_id) else {
        return Ok(());
    };
    match fs::remove_file(file) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
