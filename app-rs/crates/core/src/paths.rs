//! Where Wraithgrid keeps things on disk, and the small path helpers every module uses
//! (ports of `src/main/paths.ts` and `src/shared/paths.ts`). The locations are the Electron
//! app's, so both read the same files.

use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "config.json";

/// The user's home directory.
pub fn home_dir() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Electron's `appData`: `%APPDATA%`, `~/Library/Application Support`, or `$XDG_CONFIG_HOME`
/// (else `~/.config`).
pub fn app_data_dir(home: &Path, env: impl Fn(&str) -> Option<String>) -> PathBuf {
    if cfg!(windows) {
        if let Some(appdata) = env("APPDATA").filter(|v| !v.is_empty()) {
            return PathBuf::from(appdata);
        }
        return home.join("AppData").join("Roaming");
    }
    if cfg!(target_os = "macos") {
        return home.join("Library").join("Application Support");
    }
    match env("XDG_CONFIG_HOME").filter(|v| v.starts_with('/')) {
        Some(dir) => PathBuf::from(dir),
        None => home.join(".config"),
    }
}

/// Requirements §9 names ~/.config/wraithgrid. `WRAITHGRID_USER_DATA_DIR` lets tests run
/// against a throwaway config dir.
pub fn user_data_dir(app_data: &Path, env: impl Fn(&str) -> Option<String>) -> PathBuf {
    match env("WRAITHGRID_USER_DATA_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => app_data.join("wraithgrid"),
    }
}

/// This process's user-data dir, from the real environment.
pub fn current_user_data_dir() -> PathBuf {
    let env = |name: &str| std::env::var(name).ok();
    user_data_dir(&app_data_dir(&home_dir(), env), env)
}

pub fn config_file_path(user_data: &Path) -> PathBuf {
    user_data.join(CONFIG_FILE_NAME)
}

/// Where new accounts get their config dir: ~/.wraithgrid/accounts
pub fn accounts_root(home: &Path) -> PathBuf {
    home.join(".wraithgrid").join("accounts")
}

/// Where git worktrees created for new panes live, one folder per repo.
pub fn worktrees_root(home: &Path) -> PathBuf {
    home.join(".wraithgrid").join("worktrees")
}

/// Theme files people make or import: ~/.wraithgrid/themes/<id>.json
pub fn themes_root(home: &Path) -> PathBuf {
    home.join(".wraithgrid").join("themes")
}

fn trim_trailing_sep(p: &str) -> &str {
    if p.len() > 1 {
        p.trim_end_matches(['/', '\\'])
    } else {
        p
    }
}

/// `~` and `~/x` become absolute under `home`. Other paths are returned unchanged.
pub fn expand_home(p: &str, home: &str) -> String {
    if p == "~" {
        return home.to_string();
    }
    if p.starts_with("~/") || p.starts_with("~\\") {
        return format!("{}{}", trim_trailing_sep(home), &p[1..]);
    }
    p.to_string()
}

/// The inverse of `expand_home`, for display.
pub fn contract_home(p: &str, home: &str) -> String {
    let h = trim_trailing_sep(home);
    if h.is_empty() {
        return p.to_string();
    }
    if p == h {
        return "~".into();
    }
    match p.strip_prefix(h) {
        Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => format!("~{rest}"),
        _ => p.to_string(),
    }
}

pub fn base_name(p: &str) -> String {
    p.split(['/', '\\'])
        .rfind(|s| !s.is_empty())
        .unwrap_or(p)
        .to_string()
}

/// Lowercase, filesystem-safe slug. Returns '' when nothing usable is left.
pub fn slugify(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let mut out = String::new();
    let mut in_bad = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            out.push(c);
            in_bad = false;
        } else if !in_bad {
            out.push('-');
            in_bad = true;
        }
    }
    let out = out.trim_start_matches(['-', '_']).trim_end_matches('-');
    out.chars().take(48).collect()
}

/// Expands `~` and makes the path absolute (without touching the filesystem).
pub fn resolve_user_path(p: &str, home: &Path) -> PathBuf {
    let expanded = PathBuf::from(expand_home(p, &home.to_string_lossy()));
    let abs = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir().unwrap_or_default().join(expanded)
    };
    normalize(&abs)
}

/// Lexically removes `.` and `..` like Node's `path.resolve`.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in p.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// True when `child` is strictly inside `parent` (not equal to it).
pub fn is_strictly_inside(child: &Path, parent: &Path) -> bool {
    let (c, p) = (normalize(child), normalize(parent));
    c != p && c.starts_with(&p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_expansion() {
        assert_eq!(expand_home("~", "/home/u"), "/home/u");
        assert_eq!(expand_home("~/x", "/home/u/"), "/home/u/x");
        assert_eq!(expand_home("/a/~", "/home/u"), "/a/~");
        assert_eq!(contract_home("/home/u/x", "/home/u"), "~/x");
        assert_eq!(contract_home("/home/user2", "/home/u"), "/home/user2");
        assert_eq!(contract_home("/home/u", "/home/u/"), "~");
    }

    #[test]
    fn names() {
        assert_eq!(base_name("/a/b/"), "b");
        assert_eq!(base_name("C:\\x\\y"), "y");
        assert_eq!(slugify("  My Project!! v2 "), "my-project-v2");
        assert_eq!(slugify("__x"), "x");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn user_data() {
        let env = |k: &str| (k == "WRAITHGRID_USER_DATA_DIR").then(|| "/tmp/wg".to_string());
        assert_eq!(
            user_data_dir(Path::new("/x"), env),
            PathBuf::from("/tmp/wg")
        );
        assert_eq!(
            user_data_dir(Path::new("/home/u/.config"), |_| None),
            PathBuf::from("/home/u/.config/wraithgrid")
        );
    }

    #[test]
    fn inside() {
        assert!(is_strictly_inside(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(!is_strictly_inside(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!is_strictly_inside(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!is_strictly_inside(
            Path::new("/a/b/../c"),
            Path::new("/a/b")
        ));
    }
}
