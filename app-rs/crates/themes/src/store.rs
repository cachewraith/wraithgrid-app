//! Theme files in ~/.wraithgrid/themes (src/main/themes.ts): listed, imported (validated, then
//! written back in a normal form, never copied byte for byte), saved from the editor,
//! deleted. Only files directly in that folder. Files are written exactly as the Electron
//! app writes them (`JSON.stringify(theme, null, 2)` and a newline).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::theme::{built_in_themes, theme_slug, AppTheme, ThemeFile, THEMES_FILE_MAX, THEMES_MAX};
use crate::validate::parse_theme_file;

/// A theme file that could not be loaded, and why (shown in Settings).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ThemeProblem {
    pub file: String,
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ThemeList {
    pub themes: Vec<AppTheme>,
    pub problems: Vec<ThemeProblem>,
}

/// `Ok(id)` the theme was written under, or `Err(why)` in words for the UI. Unexpected I/O
/// failures are the outer `anyhow` error (the TS throws; the app says "Could not …").
pub type ThemeImport = Result<String, String>;

/// Theme files people make or import: ~/.wraithgrid/themes/<id>.json
pub fn themes_root(home: &Path) -> PathBuf {
    home.join(".wraithgrid").join("themes")
}

/// `^[a-z0-9][a-z0-9-]{0,39}$`: a theme id, and so a file name.
pub(crate) fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=40).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

pub(crate) fn is_built_in(id: &str) -> bool {
    built_in_themes().iter().any(|t| t.id == id)
}

/// Node's wording for a failed file call (`ENOENT: no such file or directory, stat '…'`),
/// since the TS shows `err.message` as it is.
pub(crate) fn node_error(err: &io::Error, syscall: &str, path: &Path) -> String {
    let code = match err.kind() {
        io::ErrorKind::NotFound => Some(("ENOENT", "no such file or directory")),
        io::ErrorKind::PermissionDenied => Some(("EACCES", "permission denied")),
        io::ErrorKind::IsADirectory => Some(("EISDIR", "illegal operation on a directory")),
        io::ErrorKind::NotADirectory => Some(("ENOTDIR", "not a directory")),
        _ => None,
    };
    match code {
        Some((c, what)) => format!("{c}: {what}, {syscall} '{}'", path.display()),
        None => err.to_string(),
    }
}

/// The pretty JSON a theme file holds: two-space indent and a final newline.
pub fn theme_file_text(theme: &ThemeFile) -> String {
    pretty(theme)
}

pub(crate) fn pretty(v: &impl Serialize) -> String {
    // Serializing plain structs and maps cannot fail.
    let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
    s.push('\n');
    s
}

/// Reads and checks one theme file. `Err` is a failed `stat` (the TS throws there).
fn read_theme(file: &Path) -> io::Result<Result<ThemeFile, String>> {
    let st = fs::metadata(file)?;
    if !st.is_file() {
        return Ok(Err("not a file".into()));
    }
    if st.len() > THEMES_FILE_MAX as u64 {
        return Ok(Err("larger than 64 KB".into()));
    }
    let bytes = fs::read(file)?;
    let Ok(raw) = serde_json::from_str::<Value>(&String::from_utf8_lossy(&bytes)) else {
        return Ok(Err("not valid JSON".into()));
    };
    Ok(parse_theme_file(&raw))
}

/// File names in a folder; lossy for names that are not UTF-8 (as Node gives them).
fn dir_names(dir: &Path) -> io::Result<Vec<String>> {
    fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect()
}

/// The user's themes in `dir`, and the files that could not be loaded. A missing folder
/// lists nothing.
pub fn list_themes(dir: &Path) -> ThemeList {
    let Ok(names) = dir_names(dir) else {
        return ThemeList::default();
    };
    let mut out = ThemeList::default();
    // Sorted by id: `ocean` before `ocean-2` (by file name `-` sorts before `.`). In UTF-16
    // order, as JS sorts.
    let mut ids: Vec<String> = names
        .iter()
        .filter_map(|n| n.strip_suffix(".json").map(str::to_owned))
        .collect();
    ids.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    ids.truncate(THEMES_MAX);
    for id in ids {
        let name = format!("{id}.json");
        if !valid_id(&id) {
            out.problems.push(ThemeProblem {
                file: name,
                error: "file name must be lowercase letters, digits and -".into(),
            });
            continue;
        }
        if is_built_in(&id) {
            out.problems.push(ThemeProblem {
                file: name,
                error: "a built-in theme has this name".into(),
            });
            continue;
        }
        let path = dir.join(&name);
        match read_theme(&path) {
            Ok(Ok(theme)) => out.themes.push(AppTheme {
                id,
                built_in: false,
                theme,
            }),
            Ok(Err(error)) => out.problems.push(ThemeProblem { file: name, error }),
            Err(e) => out.problems.push(ThemeProblem {
                file: name,
                error: node_error(&e, "stat", &path),
            }),
        }
    }
    out
}

/// Validates `src` and writes it into `dir` under a free id made from its name.
pub fn import_theme(src: &Path, dir: &Path) -> anyhow::Result<ThemeImport> {
    let r = match read_theme(src) {
        Ok(r) => r,
        Err(e) => return Ok(Err(node_error(&e, "stat", src))),
    };
    match r {
        Ok(theme) => write_new_theme(&theme, dir),
        Err(error) => {
            let base = src
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(Err(format!("{base}: {error}")))
        }
    }
}

/// Saves a theme made in the editor: checked like a file, then written under a free id.
pub fn save_theme(raw: &Value, dir: &Path) -> anyhow::Result<ThemeImport> {
    let theme = match parse_theme_file(raw) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let text = serde_json::to_string(&theme)?;
    if text.encode_utf16().count() > THEMES_FILE_MAX {
        return Ok(Err("larger than 64 KB".into()));
    }
    write_new_theme(&theme, dir)
}

fn write_new_theme(theme: &ThemeFile, dir: &Path) -> anyhow::Result<ThemeImport> {
    fs::create_dir_all(dir)?;
    let mut taken: Vec<String> = built_in_themes().iter().map(|t| t.id.clone()).collect();
    for n in dir_names(dir)? {
        taken.push(n.strip_suffix(".json").map(str::to_owned).unwrap_or(n));
    }
    let slug: String = theme_slug(&theme.name).chars().take(36).collect();
    let mut id = slug.clone();
    let mut i = 2;
    while taken.contains(&id) {
        id = format!("{slug}-{i}");
        i += 1;
    }
    fs::write(dir.join(format!("{id}.json")), pretty(theme))?;
    Ok(Ok(id))
}

/// Deletes a user theme. False for a built-in or a name that is not a theme id.
pub fn delete_theme(id: &str, dir: &Path) -> anyhow::Result<bool> {
    // The id pattern allows no `/`, `\` or `..`, so the file stays inside `dir`.
    if !valid_id(id) || is_built_in(id) {
        return Ok(false);
    }
    let file = dir.join(format!("{id}.json"));
    fs::remove_file(&file).map_err(|e| anyhow::anyhow!(node_error(&e, "unlink", &file)))?;
    Ok(true)
}

/// Writes a checked theme under a given id (a community theme's install or update).
pub fn write_theme_as(theme: &ThemeFile, dir: &Path, id: &str) -> anyhow::Result<()> {
    write_value_as(&serde_json::to_value(theme)?, dir, id)
}

/// `write_theme_as` for a theme already turned into JSON (to keep a key order).
pub(crate) fn write_value_as(theme: &Value, dir: &Path, id: &str) -> anyhow::Result<()> {
    if !valid_id(id) || is_built_in(id) {
        anyhow::bail!("Not a theme id: {id}");
    }
    fs::create_dir_all(dir)?;
    fs::write(dir.join(format!("{id}.json")), pretty(theme))?;
    Ok(())
}
