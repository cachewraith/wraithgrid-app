//! The folder picker's backend (port of `src/main/folders.ts` and `lib/folder-path.ts`): one
//! folder's subfolders (names and kind only, never file contents), `~` and relative paths
//! resolved from home, capped per folder; plus the pure helpers the picker draws paths with.

use std::cmp::Ordering;
use std::io::ErrorKind;
use std::path::Path;

use crate::paths::{contract_home, resolve_user_path};

/// Folders listed per folder; the rest are cut (the picker says so).
pub const FOLDER_ENTRIES_MAX: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEntry {
    pub name: String,
    pub hidden: bool,
    pub git: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderList {
    /// Where we are, `~`-contracted.
    pub path: String,
    pub parent: Option<String>,
    /// Err: why it can't be listed, in words.
    pub result: Result<Listed, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub entries: Vec<FolderEntry>,
    pub truncated: bool,
    pub branch: Option<String>,
}

impl FolderList {
    pub fn ok(&self) -> bool {
        self.result.is_ok()
    }
}

/// The branch checked out in `dir` from `.git/HEAD` (no git process); None when not a repo.
pub fn git_branch_of(dir: &Path) -> Option<String> {
    let mut git_dir = dir.join(".git");
    if std::fs::metadata(&git_dir).ok()?.is_file() {
        // A worktree: `.git` is a file pointing at the real git dir.
        let text = std::fs::read_to_string(&git_dir).ok()?;
        let target = text
            .lines()
            .find_map(|l| l.strip_prefix("gitdir:"))?
            .trim()
            .to_string();
        git_dir = dir.join(target);
    }
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(r) => r.trim().strip_prefix("refs/heads/").map(str::to_string),
        None => Some(head.chars().take(7).collect()),
    }
}

fn why(err: &std::io::Error) -> String {
    match err.kind() {
        ErrorKind::NotFound => "This folder does not exist.",
        ErrorKind::NotADirectory => "That is a file, not a folder.",
        ErrorKind::PermissionDenied => "You don’t have permission to open this folder.",
        _ => "This folder can’t be read.",
    }
    .into()
}

/// Lists `input`'s subfolders, sorted by name; hidden ones (`.x`) are marked, not dropped.
pub fn list_folder(input: &str, home: &Path) -> FolderList {
    let raw = match input.trim() {
        "" => "~",
        t => t,
    };
    let home_s = home.to_string_lossy();
    // Relative paths are taken from home, like a new terminal.
    let abs = if raw == "~"
        || raw.starts_with("~/")
        || raw.starts_with("~\\")
        || Path::new(raw).is_absolute()
    {
        resolve_user_path(raw, home)
    } else {
        resolve_user_path(&home.join(raw).to_string_lossy(), home)
    };
    let path = contract_home(&abs.to_string_lossy(), &home_s);
    let parent = abs
        .parent()
        .map(|p| contract_home(&p.to_string_lossy(), &home_s));
    let read = match std::fs::read_dir(&abs) {
        Ok(r) => r,
        Err(e) => {
            return FolderList {
                path,
                parent,
                result: Err(why(&e)),
            }
        }
    };
    let mut dirs: Vec<String> = read
        .flatten()
        .filter(|d| match d.file_type() {
            Ok(t) if t.is_dir() => true,
            // A link to a folder counts as one.
            Ok(t) if t.is_symlink() => std::fs::metadata(d.path()).is_ok_and(|m| m.is_dir()),
            _ => false,
        })
        .map(|d| d.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort_by(|a, b| natural_cmp(a, b));
    let truncated = dirs.len() > FOLDER_ENTRIES_MAX;
    dirs.truncate(FOLDER_ENTRIES_MAX);
    let entries = dirs
        .into_iter()
        .map(|name| FolderEntry {
            hidden: name.starts_with('.'),
            git: abs.join(&name).join(".git").exists(),
            name,
        })
        .collect();
    FolderList {
        path,
        parent,
        result: Ok(Listed {
            entries,
            truncated,
            branch: git_branch_of(&abs),
        }),
    }
}

/// Case-insensitive, numbers by value (`a2` before `a10`), like `localeCompare(…, {numeric})`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (n, m) = (take(&mut x), take(&mut y));
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let o = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// Typed text that is a path (jump there) rather than a filter.
pub fn looks_like_path(text: &str) -> bool {
    let t = text.trim();
    t == "~"
        || t.starts_with("~/")
        || t.starts_with("~\\")
        || t.starts_with('/')
        || is_drive_root(t)
}

fn is_drive_root(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

fn sep_of(p: &str) -> char {
    if p.contains('\\') && !p.contains('/') {
        '\\'
    } else {
        '/'
    }
}

/// `dir` + `name`, with the separator `dir` already uses.
pub fn join_path(dir: &str, name: &str) -> String {
    let sep = sep_of(dir);
    if dir.ends_with(sep) {
        format!("{dir}{name}")
    } else {
        format!("{dir}{sep}{name}")
    }
}

/// Breadcrumb parts `(label, path up to it)`: `~/a/b` → `~`, `~/a`, `~/a/b`.
pub fn crumbs(p: &str) -> Vec<(String, String)> {
    let sep = sep_of(p);
    let mut out = vec![];
    let mut acc = String::new();
    let mut rest = p;
    let b = p.as_bytes();
    if let Some(r) = p.strip_prefix('/') {
        out.push(("/".to_string(), "/".to_string()));
        acc = "/".into();
        rest = r;
    } else if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        let root = format!("{}{sep}", &p[..2]);
        out.push((p[..2].to_string(), root.clone()));
        acc = root;
        rest = p.get(3..).unwrap_or("");
    }
    for part in rest.split(['/', '\\']).filter(|s| !s.is_empty()) {
        if acc.is_empty() {
            acc = part.to_string();
        } else if acc.ends_with(sep) {
            acc.push_str(part);
        } else {
            acc.push(sep);
            acc.push_str(part);
        }
        out.push((part.to_string(), acc.clone()));
    }
    out
}

/// The folder's name and where it is: `~/code/api` → (`api`, `~/code`).
pub fn split_folder(p: &str) -> (String, String) {
    let c = crumbs(p);
    match c.len() {
        0 => (p.to_string(), String::new()),
        1 => (c[0].0.clone(), String::new()),
        n => (c[n - 1].0.clone(), c[n - 2].1.clone()),
    }
}

/// Long paths cut in the middle, so both ends stay readable.
pub fn middle_truncate(p: &str, max: usize) -> String {
    let chars: Vec<char> = p.chars().collect();
    if chars.len() <= max {
        return p.to_string();
    }
    let keep = max.saturating_sub(1);
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut s: String = chars[..head].iter().collect();
    s.push('…');
    s.extend(&chars[chars.len() - tail..]);
    s
}

/// Pins `p` on top, or unpins it; at most 20 kept.
pub fn toggle_pin(pinned: &[String], p: &str) -> Vec<String> {
    if pinned.iter().any(|x| x == p) {
        pinned.iter().filter(|x| *x != p).cloned().collect()
    } else {
        std::iter::once(p.to_string())
            .chain(pinned.iter().cloned())
            .take(20)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_crumbs() {
        assert!(looks_like_path("~"));
        assert!(looks_like_path(" ~/code "));
        assert!(looks_like_path("/etc"));
        assert!(looks_like_path("C:\\work"));
        assert!(!looks_like_path("code"));
        assert!(!looks_like_path("~code"));
        assert_eq!(join_path("~/a", "b"), "~/a/b");
        assert_eq!(join_path("/", "etc"), "/etc");
        assert_eq!(join_path("C:\\w", "x"), "C:\\w\\x");
        let c: Vec<String> = crumbs("~/a/b").into_iter().map(|c| c.1).collect();
        assert_eq!(c, ["~", "~/a", "~/a/b"]);
        let c: Vec<String> = crumbs("/etc/x").into_iter().map(|c| c.1).collect();
        assert_eq!(c, ["/", "/etc", "/etc/x"]);
        let c: Vec<String> = crumbs("C:\\w\\x").into_iter().map(|c| c.1).collect();
        assert_eq!(c, ["C:\\", "C:\\w", "C:\\w\\x"]);
        assert_eq!(split_folder("~/code/api"), ("api".into(), "~/code".into()));
        assert_eq!(split_folder("~"), ("~".into(), "".into()));
        assert_eq!(middle_truncate("abcdefghij", 5), "ab…ij");
        assert_eq!(middle_truncate("abc", 5), "abc");
    }

    #[test]
    fn sorts_naturally() {
        let mut v = vec!["b", "a10", "A2", "a1"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a1", "A2", "a10", "b"]);
    }

    #[test]
    fn pins() {
        let p = toggle_pin(&[], "~/a");
        assert_eq!(p, ["~/a"]);
        assert_eq!(toggle_pin(&p, "~/b"), ["~/b", "~/a"]);
        assert!(toggle_pin(&p, "~/a").is_empty());
    }

    #[test]
    fn lists_a_folder() {
        let home = std::env::temp_dir().join(format!("wg-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("proj/.git")).unwrap();
        std::fs::write(home.join("proj/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(home.join(".hid")).unwrap();
        std::fs::create_dir_all(home.join("b10")).unwrap();
        std::fs::create_dir_all(home.join("b9")).unwrap();
        std::fs::write(home.join("file.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(home.join("b9"), home.join("link")).unwrap();

        let l = list_folder("", &home);
        assert_eq!(l.path, "~");
        let ok = l.result.as_ref().unwrap();
        #[cfg(unix)]
        {
            let names: Vec<&str> = ok.entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, [".hid", "b9", "b10", "link", "proj"]);
        }
        assert!(ok.entries[0].hidden);
        assert!(ok.entries.iter().find(|e| e.name == "proj").unwrap().git);
        assert_eq!(ok.branch, None);

        let p = list_folder("proj", &home);
        assert_eq!(p.path, format!("~{}proj", std::path::MAIN_SEPARATOR));
        assert_eq!(p.parent.as_deref(), Some("~"));
        assert_eq!(p.result.unwrap().branch.as_deref(), Some("main"));

        assert_eq!(
            list_folder("~/nope", &home).result,
            Err("This folder does not exist.".into())
        );
        assert_eq!(
            list_folder("~/file.txt", &home).result,
            Err("That is a file, not a folder.".into())
        );
        std::fs::remove_dir_all(&home).unwrap();
    }
}
