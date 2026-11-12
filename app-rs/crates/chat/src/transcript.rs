//! Moving a chat to another account (#29), ported from `src/main/chat/transcript.ts`. claude
//! keeps a conversation inside the signed-in account's own config dir
//! (`projects/<encoded folder>/<session id>.jsonl`), so another account can only resume it
//! once the file is in its dir too: this copies it (the original stays with the first
//! account), and the chat then restarts as the other account with `--resume <id>`. Checked
//! with claude 2.1.306: a copied transcript resumes and carries on. Only between Wraithgrid's
//! own account dirs; session ids are plain tokens.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::types::is_valid_session_id;

/// Ok, or the reason it was not copied (shown to the user as is).
pub type CopyResult = Result<(), String>;

const NOT_AN_ID: &str = "Not a conversation id.";
const OUTSIDE: &str = "Only accounts in Wraithgrid’s own folder can be switched.";
const SAME: &str = "This chat already runs as that account.";
const NOT_FOUND: &str = "This conversation’s file was not found.";

/// `path.resolve`: absolute, with `.` and `..` worked out lexically (no symlinks followed).
fn resolve(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `child` is inside `parent` and is not `parent` itself (`isStrictlyInside` of
/// `src/main/paths.ts`).
pub fn is_strictly_inside(child: &Path, parent: &Path) -> bool {
    let (child, parent) = (resolve(child), resolve(parent));
    child != parent && child.starts_with(&parent)
}

fn check(from: &Path, to: &Path, id: &str, root: &Path) -> CopyResult {
    if !is_valid_session_id(id) {
        return Err(NOT_AN_ID.into());
    }
    if !is_strictly_inside(from, root) || !is_strictly_inside(to, root) {
        return Err(OUTSIDE.into());
    }
    if resolve(from) == resolve(to) {
        return Err(SAME.into());
    }
    Ok(())
}

fn copy_failed(err: io::Error) -> String {
    format!("Could not copy the conversation: {err}")
}

/// Copies a folder's contents into `dst`, overwriting what is there (`fs.cp`, recursive).
fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

/// The names in a folder, sorted (so the first match does not depend on the file system).
fn sorted_entries(dir: &Path) -> Vec<fs::DirEntry> {
    let mut list: Vec<fs::DirEntry> = match fs::read_dir(dir) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => Vec::new(),
    };
    list.sort_by_key(|d| d.file_name());
    list
}

/// Copies session `session_id` from `from_dir` to the same project folder in `to_dir`
/// (overwriting an older copy from an earlier switch). Both dirs must be inside `root`.
pub fn copy_transcript(
    from_dir: &Path,
    to_dir: &Path,
    session_id: &str,
    root: &Path,
) -> CopyResult {
    check(from_dir, to_dir, session_id, root)?;
    // The folder name is claude's encoding of the working directory: find it rather than
    // re-derive it, so a change in claude's encoding can't send the copy elsewhere.
    let projects = from_dir.join("projects");
    let file_name = format!("{session_id}.jsonl");
    let found = sorted_entries(&projects)
        .into_iter()
        .find(|d| d.file_type().is_ok_and(|t| t.is_dir()) && d.path().join(&file_name).is_file());
    let Some(found) = found else {
        return Err(NOT_FOUND.into());
    };
    let src = found.path();
    let dst = to_dir.join("projects").join(found.file_name());
    let run = || -> io::Result<()> {
        fs::create_dir_all(&dst)?;
        fs::copy(src.join(&file_name), dst.join(&file_name))?;
        // Subagent transcripts and tool results, when claude keeps any for this session.
        let extra = src.join(session_id);
        if extra.is_dir() {
            copy_dir(&extra, &dst.join(session_id))?;
        }
        Ok(())
    };
    run().map_err(copy_failed)
}

/// codex (#60): a thread is `sessions/YYYY/MM/DD/rollout-<time>-<thread id>.jsonl` in the
/// account's CODEX_HOME. Copied to the same place in `to_dir`, `thread/resume` finds it there
/// (checked with codex-cli 0.164.0; its sqlite index isn't needed). Both dirs inside `root`.
pub fn copy_codex_rollout(
    from_dir: &Path,
    to_dir: &Path,
    thread_id: &str,
    root: &Path,
) -> CopyResult {
    check(from_dir, to_dir, thread_id, root)?;
    // Three folder levels (year, month, day), then the file; found, not derived from a date.
    let sessions = from_dir.join("sessions");
    let suffix = format!("-{thread_id}.jsonl");
    fn find(dir: &Path, depth: u8, suffix: &str) -> Option<PathBuf> {
        for d in sorted_entries(dir) {
            let Ok(ty) = d.file_type() else { continue };
            let name = d.file_name().to_string_lossy().into_owned();
            let dated = (1..=4).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_digit());
            if depth < 3 && ty.is_dir() && dated {
                if let Some(hit) = find(&d.path(), depth + 1, suffix) {
                    return Some(hit);
                }
            } else if depth == 3
                && ty.is_file()
                && name.starts_with("rollout-")
                && name.ends_with(suffix)
            {
                return Some(d.path());
            }
        }
        None
    }
    let Some(found) = find(&sessions, 0, &suffix) else {
        return Err(NOT_FOUND.into());
    };
    let rel = found.strip_prefix(&sessions).unwrap_or(&found);
    let dst = to_dir.join("sessions").join(rel);
    let run = || -> io::Result<()> {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&found, &dst)?;
        Ok(())
    };
    run().map_err(copy_failed)
}
