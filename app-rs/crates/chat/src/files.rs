//! The `@` file menu in a chat (#159), ported from `src/main/chat/files.ts`: the files and
//! folders under the pane's own folder, as paths relative to it. git's own list when the
//! folder is a repo (so .gitignore holds), a bounded walk otherwise; ranked here so the view
//! gets a few paths, never the whole tree.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::json::now_ms;
use crate::text::{locale_cmp, subsequence};

/// Paths sent to the menu per query.
pub const FILE_MENTIONS_MAX: usize = 50;
/// Entries a walk (no git) looks at, and how long it may take (ms).
pub const WALK_ENTRIES_MAX: usize = 20_000;
pub const WALK_MS: i64 = 1500;
/// How long one folder's list is reused while typing (ms).
pub const FILE_LIST_TTL_MS: i64 = 10_000;

const SKIP: [&str; 2] = [".git", "node_modules"];

/// The args the file list runs git with.
pub const GIT_LS_FILES: [&str; 5] = [
    "ls-files",
    "--cached",
    "--others",
    "--exclude-standard",
    "-z",
];

/// Why a git run gave no full list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitFailure {
    /// Not a repo, git missing, …: walk the folder instead.
    Failed,
    /// Past the runner's buffer: what git printed before that.
    Overflowed(String),
}

/// Runs `git <args>` in a folder and returns its stdout (the app passes its own git runner).
pub type GitRun<'a> = dyn Fn(&[&str], &Path) -> Result<String, GitFailure> + Send + Sync + 'a;

fn skipped(rel: &str) -> bool {
    rel.split('/').any(|part| SKIP.contains(&part))
}

/// Each file's folders (`src/`, `src/main/`) once, then the files; `/` separators.
pub fn with_folders(files: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut dirs = Vec::new();
    for f in files {
        for (i, _) in f.match_indices('/') {
            let d = &f[..=i];
            if seen.insert(d.to_string()) {
                dirs.push(d.to_string());
            }
        }
    }
    dirs.extend(files.iter().cloned());
    dirs
}

/// git's tracked and untracked-but-not-ignored files; None when `root` is not a repo.
fn git_files(git: &GitRun, root: &Path) -> Option<Vec<String>> {
    let out = match git(&GIT_LS_FILES, root) {
        Ok(out) => out,
        // Past the runner's buffer: what came first is still a list (cut at the last whole entry).
        Err(GitFailure::Overflowed(part)) => match part.rfind('\0') {
            Some(i) => part[..=i].to_string(),
            None => String::new(),
        },
        Err(GitFailure::Failed) => return None,
    };
    let mut seen = HashSet::new();
    Some(
        out.split('\0')
            .filter(|f| !f.is_empty() && !skipped(f) && seen.insert(*f))
            .map(String::from)
            .collect(),
    )
}

/// Breadth first, so a cut keeps the shallow paths; folders end with `/`.
fn walk(root: &Path, now: &dyn Fn() -> i64) -> Vec<String> {
    let mut out = Vec::new();
    let until = now() + WALK_MS;
    let mut queue = VecDeque::from([String::new()]);
    let mut seen = 0;
    while seen < WALK_ENTRIES_MAX && now() < until {
        let Some(rel) = queue.pop_front() else { break };
        let Ok(dir) = std::fs::read_dir(root.join(&rel)) else {
            continue;
        };
        let mut entries: Vec<(String, std::fs::FileType)> = dir
            .flatten()
            .filter_map(|d| Some((d.file_name().into_string().ok()?, d.file_type().ok()?)))
            .collect();
        entries.sort_by(|a, b| locale_cmp(&a.0, &b.0));
        for (name, ty) in entries {
            seen += 1;
            if seen > WALK_ENTRIES_MAX {
                break;
            }
            if SKIP.contains(&name.as_str()) {
                continue;
            }
            let p = format!("{rel}{name}");
            if ty.is_dir() {
                out.push(format!("{p}/"));
                queue.push_back(format!("{p}/"));
            } else if ty.is_file() || ty.is_symlink() {
                out.push(p);
            }
        }
    }
    out
}

/// Every path the menu can offer for `root`: folders (`src/`) and files.
pub fn list_project_paths(root: &Path, git: &GitRun, now: &dyn Fn() -> i64) -> Vec<String> {
    match git_files(git, root) {
        Some(files) => with_folders(&files),
        None => walk(root, now),
    }
}

/// A git runner on `git` from PATH, for an app without one of its own. Output past `max`
/// bytes counts as overflowed.
pub fn system_git(max: usize) -> impl Fn(&[&str], &Path) -> Result<String, GitFailure> {
    move |args, cwd| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|_| GitFailure::Failed)?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if text.len() > max {
            let cut = crate::text::floor_boundary(&text, max);
            return Err(GitFailure::Overflowed(text[..cut].to_string()));
        }
        if !out.status.success() {
            return Err(GitFailure::Failed);
        }
        Ok(text)
    }
}

/// A folder's list and when it was made (ms).
type Cached = (i64, Arc<Vec<String>>);

/// `list_project_paths` with each folder's list kept for a few seconds (one per keystroke
/// otherwise).
pub struct FileLister {
    git: Box<GitRun<'static>>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
    cache: Mutex<HashMap<PathBuf, Cached>>,
}

impl FileLister {
    pub fn new(
        git: impl Fn(&[&str], &Path) -> Result<String, GitFailure> + Send + Sync + 'static,
    ) -> Self {
        Self::with_clock(git, now_ms)
    }

    /// With its own clock (ms), for tests.
    pub fn with_clock(
        git: impl Fn(&[&str], &Path) -> Result<String, GitFailure> + Send + Sync + 'static,
        now: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        FileLister {
            git: Box::new(git),
            now: Box::new(now),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The folder's paths, from the cache while fresh.
    pub fn list(&self, root: &Path) -> Arc<Vec<String>> {
        let at = (self.now)();
        if let Some((when, paths)) = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(root)
        {
            if at - when < FILE_LIST_TTL_MS {
                return paths.clone();
            }
        }
        let paths = Arc::new(list_project_paths(root, &*self.git, &*self.now));
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(root.to_path_buf(), (at, paths.clone()));
        paths
    }
}

fn base_of(p: &str) -> &str {
    let s = p.strip_suffix('/').unwrap_or(p);
    match s.rfind('/') {
        Some(i) => &s[i + 1..],
        None => s,
    }
}

fn score(p: &str, q: &str) -> u32 {
    let full = p.to_lowercase();
    let base = base_of(&full);
    // The name before its extension(s): `app` of `app.ts` and `app.test.ts`.
    let stem = match base.find('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base,
    };
    if base == q || full == q {
        100
    } else if stem == q {
        90
    } else if base.starts_with(q) {
        80
    } else if full.starts_with(q) {
        70
    } else if base.contains(q) {
        60
    } else if full.contains(q) {
        40
    } else if subsequence(q, &full) {
        20
    } else {
        0
    }
}

fn depth(p: &str) -> usize {
    p.split('/').filter(|s| !s.is_empty()).count()
}

/// Folders first, then by name.
fn folders_first(a: &&String, b: &&String) -> std::cmp::Ordering {
    b.ends_with('/')
        .cmp(&a.ends_with('/'))
        .then_with(|| locale_cmp(a, b))
}

/// The paths matching `query`, best first: the file name before the rest of the path, then
/// shallower and shorter paths. A bare `@` lists the top level. `query` uses `/` like the
/// paths.
pub fn rank_paths(paths: &[String], query: &str, limit: usize) -> Vec<String> {
    let q = query.to_lowercase();
    if q.is_empty() || q.ends_with('/') {
        // Inside a folder (`src/`): that folder's own entries, the way a file browser opens it.
        let want = depth(&q) + 1;
        let mut hits: Vec<&String> = paths
            .iter()
            .filter(|p| depth(p) == want && (q.is_empty() || p.to_lowercase().starts_with(&q)))
            .collect();
        hits.sort_by(folders_first);
        return hits.into_iter().take(limit).cloned().collect();
    }
    let mut scored: Vec<(u32, &String)> = paths
        .iter()
        .map(|p| (score(p, &q), p))
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| {
        sb.cmp(sa)
            .then(depth(a).cmp(&depth(b)))
            .then(a.chars().count().cmp(&b.chars().count()))
            .then_with(|| locale_cmp(a, b))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, p)| p.clone())
        .collect()
}
