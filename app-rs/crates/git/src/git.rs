//! Git facts for a pane's folder (branch, changed files, diff, branches) and the few writes the
//! user asks for: a worktree for a new pane (and removing it), switching branch, commit, push,
//! pull and a pull-request link (port of `src/main/git.ts`; history is in history.rs). git
//! runs through an injected `GitRunner`, so parsing and argument building stay unit-testable.
//! Errors are the line the UI shows.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::runner::{GitError, GitRunner};

pub const COMMIT_MESSAGE_MAX: usize = 5000;

/// A diff bigger than this is cut, so one huge generated file can't stall the UI.
pub const DIFF_MAX_BYTES: usize = 1_000_000;

/// A folder's git state for the pane header and status polling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    /// False when the folder is not inside a git work tree (or git is missing).
    pub repo: bool,
    /// None on a detached HEAD.
    pub branch: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    /// Changed, staged and untracked paths.
    pub changed: usize,
}

/// The working tree against HEAD, for the Changes panel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiff {
    pub patch: String,
    pub truncated: bool,
    /// Untracked files (not in the patch), at most 500.
    pub untracked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalBranch {
    pub name: String,
    /// git's relative committer date (`2 hours ago`).
    pub when: String,
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteBranch {
    pub name: String,
    pub when: String,
}

/// A folder's branches, newest commit first; `remote` lists only ones not checked out locally.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranches {
    pub current: Option<String>,
    pub local: Vec<LocalBranch>,
    pub remote: Vec<RemoteBranch>,
}

/// What a new worktree checks out: a new branch, or one the repo has (local or remote, #162).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeFrom {
    #[default]
    New,
    Local,
    Remote,
}

/// The branch to switch a folder to, as `{ "kind": "local", "name": "main" }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "lowercase")]
pub enum SwitchTarget {
    Local(String),
    /// Becomes a new local branch tracking it.
    Remote(String),
    /// A new branch from HEAD.
    Create(String),
}

/// A linked worktree about to be removed: its branch and uncommitted files, so the user can
/// decide first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeCheck {
    pub branch: Option<String>,
    pub changed: usize,
    pub main_root: PathBuf,
}

/// Branch names we create: git's own rules are checked too, this keeps them simple and safe
/// (`[A-Za-z0-9._/-]{1,100}`, no leading `-`, no `..` or `//`, no `.lock`, `.` or `/` at the end).
pub fn valid_branch_name(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
        && !name.starts_with('-')
        && !name.contains("..")
        && !name.contains("//")
        && !name.ends_with(".lock")
        && !name.ends_with(['.', '/'])
}

static AHEAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ahead ([0-9]+)").unwrap());
static BEHIND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"behind ([0-9]+)").unwrap());

const STATUS_ARGS: [&str; 4] = [
    "status",
    "--porcelain=1",
    "--branch",
    "--untracked-files=normal",
];

/// Parses `git status --porcelain=1 --branch` output (`repo` is always true).
pub fn parse_status(out: &str) -> GitStatus {
    let mut lines: Vec<&str> = out.split('\n').filter(|l| !l.is_empty()).collect();
    let head = match lines.first() {
        Some(l) if l.starts_with("## ") => &lines.remove(0)[3..],
        _ => "",
    };
    let mut st = GitStatus {
        repo: true,
        ..GitStatus::default()
    };
    if !head.is_empty() {
        st.branch = if let Some(b) = head.strip_prefix("No commits yet on ") {
            Some(b.to_string())
        } else if head.starts_with("HEAD (no branch)") {
            None
        } else {
            let b = head.split("...").next().unwrap_or_default();
            Some(b.split(' ').next().unwrap_or_default().to_string())
        };
        let num = |re: &Regex| {
            re.captures(head)
                .and_then(|c| c[1].parse().ok())
                .unwrap_or(0)
        };
        st.ahead = num(&AHEAD);
        st.behind = num(&BEHIND);
    }
    st.changed = lines.len();
    st
}

pub fn git_status(run: &dyn GitRunner, cwd: &Path) -> GitStatus {
    match run.run(&STATUS_ARGS, cwd, false) {
        Ok(out) => parse_status(&out),
        // Too many changed files to list is still a repo: count what was listed (#140).
        Err(e) => match e.overflow_output() {
            Some(part) => parse_status(&part[..part.rfind('\n').map_or(0, |i| i + 1)]),
            None => GitStatus::default(),
        },
    }
}

pub fn git_diff(run: &dyn GitRunner, cwd: &Path) -> Result<GitDiff, String> {
    let (patch, over) = match run.run(&["diff", "HEAD", "--no-color", "--no-ext-diff"], cwd, false)
    {
        Ok(p) => (p, false),
        // Bigger than the buffer: what came first, marked cut (#140), not the unstaged-only diff.
        Err(e) if e.overflowed => (e.stdout, true),
        // No commits yet: there is no HEAD to compare against.
        Err(_) => match run.run(&["diff", "--no-color", "--no-ext-diff"], cwd, false) {
            Ok(p) => (p, false),
            Err(e) if e.overflowed => (e.stdout, true),
            Err(e) => return Err(e.first_line()),
        },
    };
    // Listing untracked files is a nicety; the diff still stands without it.
    let untracked = run
        .run(&["ls-files", "--others", "--exclude-standard"], cwd, false)
        .map(|out| {
            out.split('\n')
                .filter(|l| !l.is_empty())
                .take(500)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let truncated = over || patch.len() > DIFF_MAX_BYTES;
    Ok(GitDiff {
        patch: if truncated {
            cut(&patch, DIFF_MAX_BYTES).to_string()
        } else {
            patch
        },
        truncated,
        untracked,
    })
}

/// The first `max` bytes of `s`, backed off to a char boundary.
pub(crate) fn cut(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `~/.wraithgrid/worktrees/<repo>/<branch>`, outside the repo so it never shows as untracked.
pub fn worktree_dir(worktrees_root: &Path, repo_root: &Path, branch: &str) -> PathBuf {
    worktrees_root
        .join(repo_root.file_name().unwrap_or_default())
        .join(branch.replace('/', "-"))
}

/// `path.resolve`: absolute, `.` and `..` folded, the platform's own separators (git prints
/// `C:/…` on Windows).
fn resolve(p: &str) -> PathBuf {
    let p = Path::new(p);
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
            c => out.push(c),
        }
    }
    out
}

/// The repo's main checkout, so a worktree made from a worktree pane goes with the repo's
/// others (#162); a checkout whose git dir is elsewhere (a submodule) uses its own top folder.
fn main_checkout(run: &dyn GitRunner, cwd: &Path) -> Result<PathBuf, GitError> {
    let top = run.run(&["rev-parse", "--show-toplevel"], cwd, false)?;
    // An old git without --path-format fails here: the top folder.
    if let Ok(common) = run.run(
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd,
        false,
    ) {
        let common = resolve(common.trim());
        if common.file_name().is_some_and(|n| n == ".git") {
            if let Some(parent) = common.parent() {
                return Ok(parent.to_path_buf());
            }
        }
    }
    Ok(PathBuf::from(top.trim()))
}

static CHECKED_OUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^'[^']+' is already (?:checked out|used by worktree) at '([^']+)'").unwrap()
});

/// git refusing a branch that another folder has checked out, said plainly (#162).
pub fn explain_worktree_error(line: &str, branch: &str) -> String {
    match CHECKED_OUT.captures(line) {
        Some(m) => format!(
            "{branch} is already checked out in {}. A branch can be open in one folder at a time: open that folder, or pick another branch.",
            &m[1]
        ),
        None => line.to_string(),
    }
}

const NOT_VALID: &str = "Not a valid branch name.";

fn gone(name: &str) -> String {
    format!("No branch {name} here any more.")
}

/// Makes the worktree folder for `branch` and returns it. `reuse`: use the folder if it is
/// there, or check out the branch if only it exists (team, #79). `from` local/remote opens
/// that branch (a remote one as a new local branch tracking it).
pub fn add_worktree(
    run: &dyn GitRunner,
    cwd: &Path,
    branch: &str,
    worktrees_root: &Path,
    reuse: bool,
    from: WorktreeFrom,
) -> Result<PathBuf, String> {
    if !valid_branch_name(branch) {
        return Err(NOT_VALID.into());
    }
    let root = main_checkout(run, cwd)
        .and_then(|root| {
            run.run(&["check-ref-format", "--branch", branch], cwd, false)?;
            Ok(root)
        })
        .map_err(|e| e.first_line())?;
    // An existing branch must be one git lists, so nothing from the UI reaches git as an
    // option; a remote one gets the local name without its remote (`origin/feat/x` → `feat/x`).
    let mut local = branch;
    if from != WorktreeFrom::New {
        let list = list_branches(run, &root)?;
        let known = match from {
            WorktreeFrom::Local => list.local.iter().any(|b| b.name == branch),
            _ => list.remote.iter().any(|b| b.name == branch),
        };
        if !known {
            return Err(gone(branch));
        }
        if from == WorktreeFrom::Remote {
            local = &branch[branch.find('/').map_or(0, |i| i + 1)..];
        }
        if !valid_branch_name(local) {
            return Err(NOT_VALID.into());
        }
    }
    let dir = worktree_dir(worktrees_root, &root, local);
    let has_git = dir.join(".git").exists();
    if reuse && has_git {
        return Ok(dir);
    }
    // Its folder is there on that branch already: git would only say the folder exists.
    if from != WorktreeFrom::New && has_git {
        let there = current_branch(run, &dir).ok().flatten();
        if there.as_deref() == Some(local) {
            let line = format!("'{local}' is already checked out at '{}'", dir.display());
            return Err(explain_worktree_error(&line, local));
        }
    }
    let d = dir.to_string_lossy();
    let args: Vec<&str> = match from {
        WorktreeFrom::Local => vec!["worktree", "add", &d, branch],
        WorktreeFrom::Remote => vec!["worktree", "add", "--track", "-b", local, &d, branch],
        WorktreeFrom::New => vec!["worktree", "add", "-b", branch, &d],
    };
    // Slow: a big checkout and post-checkout hooks take longer than the status poll's timeout.
    if let Err(err) = run.run(&args, &root, true) {
        if !reuse || from != WorktreeFrom::New {
            return Err(explain_worktree_error(&err.first_line(), local));
        }
        if run
            .run(&["worktree", "add", &d, branch], &root, true)
            .is_err()
        {
            return Err(err.first_line());
        }
    }
    Ok(dir)
}

/// Stages everything in the pane's folder and commits it with the user's message.
pub fn commit_all(run: &dyn GitRunner, cwd: &Path, message: &str) -> Result<String, String> {
    let msg = message.trim();
    if msg.is_empty() {
        return Err("Write a commit message first.".into());
    }
    // UTF-16 units, as the UI counts them.
    if msg.encode_utf16().count() > COMMIT_MESSAGE_MAX {
        return Err("The message is too long.".into());
    }
    let commit = || -> Result<String, GitError> {
        run.run(&["add", "-A"], cwd, false)?;
        run.run(&["commit", "-m", msg], cwd, true)?;
        let hash = run.run(&["rev-parse", "--short", "HEAD"], cwd, false)?;
        Ok(format!("Committed {}", hash.trim()))
    };
    commit().map_err(|e| e.first_line())
}

const BRANCH_REF_FORMAT: &str =
    "--format=%(refname)%00%(committerdate:relative)%00%(upstream:short)";

/// Parses `git for-each-ref` over refs/heads and refs/remotes (newest commit first).
pub fn parse_branches(out: &str, current: Option<String>) -> GitBranches {
    let mut local = Vec::new();
    let mut remote_refs = Vec::new();
    for line in out.split('\n') {
        let mut f = line.split('\0');
        let r = f.next().unwrap_or_default();
        let when = f.next().unwrap_or_default().to_string();
        let upstream = f.next().unwrap_or_default();
        if r.is_empty() {
            continue;
        }
        if let Some(name) = r.strip_prefix("refs/heads/") {
            local.push(LocalBranch {
                name: name.to_string(),
                when,
                upstream: (!upstream.is_empty()).then(|| upstream.to_string()),
            });
        } else if let Some(name) = r.strip_prefix("refs/remotes/") {
            if !r.ends_with("/HEAD") {
                remote_refs.push(RemoteBranch {
                    name: name.to_string(),
                    when,
                });
            }
        }
    }
    // A remote branch someone already has locally (as its upstream or by name) is not offered twice.
    let covered: HashSet<&str> = local
        .iter()
        .flat_map(|b| b.upstream.as_deref().into_iter().chain([b.name.as_str()]))
        .collect();
    let remote = remote_refs
        .into_iter()
        .filter(|r| {
            let short = match r.name.find('/') {
                Some(i) if i > 0 => &r.name[i + 1..],
                _ => &r.name,
            };
            !covered.contains(r.name.as_str()) && !covered.contains(short)
        })
        .collect();
    GitBranches {
        current,
        local,
        remote,
    }
}

pub fn list_branches(run: &dyn GitRunner, cwd: &Path) -> Result<GitBranches, String> {
    let list = || -> Result<GitBranches, GitError> {
        let out = run.run(
            &[
                "for-each-ref",
                "--sort=-committerdate",
                BRANCH_REF_FORMAT,
                "refs/heads",
                "refs/remotes",
            ],
            cwd,
            false,
        )?;
        Ok(parse_branches(&out, current_branch(run, cwd)?))
    };
    list().map_err(|e| e.first_line())
}

/// Checks out a local branch, a remote one (as a new local branch tracking it) or a new branch
/// from HEAD. A name must be one git listed (or, for a new one, a valid name), so nothing from
/// the UI reaches git as an option. Uncommitted changes go along; git refuses when they would
/// be overwritten, and says which files.
pub fn switch_branch(run: &dyn GitRunner, cwd: &Path, to: &SwitchTarget) -> Result<String, String> {
    let (args, name): (Vec<&str>, &str) = match to {
        SwitchTarget::Create(name) => {
            if !valid_branch_name(name) {
                return Err(NOT_VALID.into());
            }
            (vec!["switch", "-c", name], name)
        }
        SwitchTarget::Local(name) | SwitchTarget::Remote(name) => {
            let list = list_branches(run, cwd)?;
            let local = matches!(to, SwitchTarget::Local(_));
            let known = if local {
                list.local.iter().any(|b| &b.name == name)
            } else {
                list.remote.iter().any(|b| &b.name == name)
            };
            if !known || name.starts_with('-') {
                return Err(gone(name));
            }
            if local && list.current.as_ref() == Some(name) {
                return Ok(format!("Already on {name}"));
            }
            if local {
                (vec!["switch", name], name)
            } else {
                (vec!["switch", "--track", name], name)
            }
        }
    };
    let switch = || -> Result<String, GitError> {
        if matches!(to, SwitchTarget::Create(_)) {
            run.run(&["check-ref-format", "--branch", name], cwd, false)?;
        }
        // Slow: post-checkout hooks and big trees take longer than the status poll's timeout.
        run.run(&args, cwd, true)?;
        let now = current_branch(run, cwd)?;
        Ok(format!("Switched to {}", now.as_deref().unwrap_or(name)))
    };
    switch().map_err(|e| e.first_line())
}

/// The branch checked out in `cwd`, or None on a detached HEAD.
fn current_branch(run: &dyn GitRunner, cwd: &Path) -> Result<Option<String>, GitError> {
    let out = run.run(&["rev-parse", "--abbrev-ref", "HEAD"], cwd, false)?;
    let b = out.trim();
    Ok((!b.is_empty() && b != "HEAD").then(|| b.to_string()))
}

/// `origin` when there is one, else the first remote.
fn pick_remote(run: &dyn GitRunner, cwd: &Path) -> Result<Option<String>, GitError> {
    let out = run.run(&["remote"], cwd, false)?;
    let remotes: Vec<&str> = out
        .split('\n')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .collect();
    Ok(if remotes.contains(&"origin") {
        Some("origin".into())
    } else {
        remotes.first().map(|r| r.to_string())
    })
}

const DETACHED: &str = "Not on a branch (detached HEAD).";
const UPSTREAM_ARGS: [&str; 4] = ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"];

/// Pushes the current branch, setting its upstream on the first push.
pub fn push_branch(run: &dyn GitRunner, cwd: &Path) -> Result<String, String> {
    let push = || -> Result<Result<String, String>, GitError> {
        let Some(branch) = current_branch(run, cwd)? else {
            return Ok(Err(DETACHED.into()));
        };
        if run.run(&UPSTREAM_ARGS, cwd, false).is_ok() {
            run.run(&["push"], cwd, true)?;
            return Ok(Ok(format!("Pushed {branch}")));
        }
        let Some(remote) = pick_remote(run, cwd)? else {
            return Ok(Err("This repository has no remote to push to.".into()));
        };
        run.run(&["push", "--set-upstream", &remote, &branch], cwd, true)?;
        Ok(Ok(format!("Pushed {branch} to {remote}")))
    };
    push().unwrap_or_else(|e| Err(e.first_line()))
}

/// `(ahead, behind)` of HEAD against its upstream, from `git rev-list --left-right --count`
/// (which prints `behind ahead`).
fn ahead_behind(run: &dyn GitRunner, cwd: &Path) -> Result<(usize, usize), GitError> {
    let out = run.run(
        &["rev-list", "--left-right", "--count", "@{u}...HEAD"],
        cwd,
        false,
    )?;
    let mut n = out
        .split_whitespace()
        .map(|x| x.parse::<usize>().unwrap_or(0));
    let behind = n.next().unwrap_or(0);
    let ahead = n.next().unwrap_or(0);
    Ok((ahead, behind))
}

/// `1 commit`, `2 commits`.
fn plural(k: usize, one: &str) -> String {
    format!("{k} {one}{}", if k == 1 { "" } else { "s" })
}

/// Pulls the current branch from its upstream (git's own pull settings: merge or rebase),
/// never forced. Says what happened: up to date, how many commits came in, or git's error
/// (conflicts, local changes in the way, diverged branches).
pub fn pull_branch(run: &dyn GitRunner, cwd: &Path) -> Result<String, String> {
    let pull = || -> Result<Result<String, String>, GitError> {
        let Some(branch) = current_branch(run, cwd)? else {
            return Ok(Err(DETACHED.into()));
        };
        if run.run(&UPSTREAM_ARGS, cwd, false).is_err() {
            return Ok(Err(format!(
                "{branch} has no upstream branch to pull from. Push it first."
            )));
        }
        run.run(&["fetch", "--quiet"], cwd, true)?;
        let (ahead, behind) = ahead_behind(run, cwd)?;
        if behind == 0 {
            return Ok(Ok(if ahead > 0 {
                format!("Up to date · {} to push", plural(ahead, "commit"))
            } else {
                "Up to date".into()
            }));
        }
        run.run(&["pull", "--no-edit"], cwd, true)?;
        let (after, _) = ahead_behind(run, cwd)?;
        let to_push = if after > 0 {
            format!(" · {} to push", plural(after, "commit"))
        } else {
            String::new()
        };
        Ok(Ok(format!("Pulled {}{to_push}", plural(behind, "commit"))))
    };
    pull().unwrap_or_else(|e| Err(e.first_line()))
}

static REMOTE_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:https://(?:[^@/]+@)?|ssh://git@|git@)(github\.com|gitlab\.com)[:/]([A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)+)/?$",
    )
    .unwrap()
});

/// The web page that opens a pull request (GitHub) or merge request (GitLab) for `branch`,
/// from a remote URL in https or scp-like ssh form; None for other hosts.
pub fn pull_request_url(remote_url: &str, branch: &str) -> Option<String> {
    let m = REMOTE_URL.captures(remote_url.trim())?;
    let repo = &m[2];
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let b = encode_uri_component(branch);
    Some(if &m[1] == "github.com" {
        format!("https://github.com/{repo}/compare/{b}?expand=1")
    } else {
        format!(
            "https://gitlab.com/{repo}/-/merge_requests/new?merge_request%5Bsource_branch%5D={b}"
        )
    })
}

/// JavaScript's `encodeURIComponent`: everything but `A-Za-z0-9-_.!~*'()` as `%XX` UTF-8.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The pull-request page for the pane's branch on its remote.
pub fn pull_request_link(run: &dyn GitRunner, cwd: &Path) -> Result<String, String> {
    let link = || -> Result<Result<String, String>, GitError> {
        let Some(branch) = current_branch(run, cwd)? else {
            return Ok(Err(DETACHED.into()));
        };
        let Some(remote) = pick_remote(run, cwd)? else {
            return Ok(Err("This repository has no remote.".into()));
        };
        let url = run.run(&["remote", "get-url", &remote], cwd, false)?;
        Ok(pull_request_url(&url, &branch)
            .ok_or_else(|| "Pull requests open for GitHub and GitLab remotes only.".into()))
    };
    link().unwrap_or_else(|e| Err(e.first_line()))
}

/// Whether `dir` is a linked worktree (not a repo's main checkout), its branch and how many
/// files in it are uncommitted, so the user can decide before it is removed.
pub fn check_worktree(run: &dyn GitRunner, dir: &Path) -> Result<WorktreeCheck, String> {
    let check = || -> Result<Result<WorktreeCheck, String>, GitError> {
        let out = run.run(
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-dir",
                "--git-common-dir",
            ],
            dir,
            false,
        )?;
        let mut lines = out.split('\n').map(str::trim);
        let git_dir = lines.next().unwrap_or_default();
        let common_dir = lines.next().unwrap_or_default();
        if git_dir.is_empty() || common_dir.is_empty() || resolve(git_dir) == resolve(common_dir) {
            return Ok(Err("Not a linked worktree.".into()));
        }
        let st = parse_status(&run.run(&STATUS_ARGS, dir, false)?);
        let common = resolve(common_dir);
        let main_root = common.parent().map(Path::to_path_buf).unwrap_or(common);
        Ok(Ok(WorktreeCheck {
            branch: st.branch,
            changed: st.changed,
            main_root,
        }))
    };
    check().unwrap_or_else(|e| Err(e.first_line()))
}

/// Removes a linked worktree's folder; its branch and commits stay in the repo. Uncommitted
/// files are only thrown away with `force`. Runs from the main checkout: git won't remove
/// the worktree it is running in.
pub fn remove_worktree(run: &dyn GitRunner, dir: &Path, force: bool) -> Result<String, String> {
    let check = check_worktree(run, dir)?;
    if check.changed > 0 && !force {
        return Err(format!(
            "{} uncommitted files; remove anyway to lose them.",
            check.changed
        ));
    }
    let d = dir.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&d);
    run.run(&args, &check.main_root, false)
        .map_err(|e| e.first_line())?;
    Ok(match check.branch {
        Some(b) => format!("Kept branch {b}"),
        None => "Removed".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    type Out = Result<String, GitError>;

    #[test]
    fn parse_status_reads_the_branch_ahead_behind_and_the_change_count() {
        let st = parse_status("## main...origin/main [ahead 2, behind 1]\n M a.ts\n?? b.ts\n");
        assert_eq!(
            st,
            GitStatus {
                repo: true,
                branch: Some("main".into()),
                ahead: 2,
                behind: 1,
                changed: 2
            }
        );
        let st = parse_status("## feat/x\n");
        assert_eq!(st.branch.as_deref(), Some("feat/x"));
        assert_eq!((st.ahead, st.behind, st.changed), (0, 0, 0));
        assert_eq!(
            parse_status("## No commits yet on main\n?? a\n")
                .branch
                .as_deref(),
            Some("main")
        );
        assert_eq!(parse_status("## HEAD (no branch)\n").branch, None);
    }

    #[test]
    fn git_status_reports_a_non_repo_when_git_fails() {
        let failing =
            |_: &[&str], _: &Path, _: bool| -> Out { Err(GitError::new("not a git repository")) };
        let st = git_status(&failing, Path::new("/x"));
        assert!(!st.repo);
        assert_eq!(st.changed, 0);
    }

    #[test]
    fn git_status_counts_what_was_listed_past_the_buffer() {
        let over = |_: &[&str], _: &Path, _: bool| -> Out {
            Err(GitError {
                stdout: "## main\n M a\n M b\n M c".into(),
                overflowed: true,
                ..GitError::default()
            })
        };
        let st = git_status(&over, Path::new("/x"));
        assert!(st.repo);
        // The cut last line is not counted.
        assert_eq!(st.changed, 2);
    }

    #[test]
    fn branch_names_accept_plain_names_and_refuse_option_like_or_unsafe_ones() {
        for ok in ["feat/rate-limit", "fix_1", "v1.2"] {
            assert!(valid_branch_name(ok), "{ok}");
        }
        for bad in [
            "-b", "--force", "a..b", "a b", "x.lock", "x/", "a//b", "", "a;rm",
        ] {
            assert!(!valid_branch_name(bad), "{bad}");
        }
        assert!(valid_branch_name(&"a".repeat(100)));
        assert!(!valid_branch_name(&"a".repeat(101)));
    }

    #[test]
    fn never_runs_git_for_a_refused_name() {
        let called = Cell::new(false);
        let spy = |_: &[&str], _: &Path, _: bool| -> Out {
            called.set(true);
            Ok(String::new())
        };
        let r = add_worktree(
            &spy,
            Path::new("/r"),
            "--upload-pack=x",
            Path::new("/w"),
            false,
            WorktreeFrom::New,
        );
        assert!(r.is_err());
        assert!(!called.get());
    }

    #[test]
    fn says_plainly_when_a_branch_is_checked_out_in_another_folder() {
        for line in [
            "'feat/x' is already checked out at '/w/api/feat-x'",
            "'feat/x' is already used by worktree at '/w/api/feat-x'",
        ] {
            assert_eq!(
                explain_worktree_error(line, "feat/x"),
                "feat/x is already checked out in /w/api/feat-x. A branch can be open in one folder at a time: open that folder, or pick another branch."
            );
        }
        assert_eq!(
            explain_worktree_error("something else", "x"),
            "something else"
        );
    }

    #[test]
    fn opens_only_a_branch_git_lists_never_a_name_from_outside_as_an_option() {
        let calls = RefCell::new(Vec::<Vec<String>>::new());
        let run = |args: &[&str], _: &Path, _: bool| -> Out {
            calls
                .borrow_mut()
                .push(args.iter().map(|a| a.to_string()).collect());
            Ok(match args[0] {
                "rev-parse" => "/r\n".into(),
                "for-each-ref" => "refs/heads/main\0now\0\n".into(),
                _ => String::new(),
            })
        };
        let r = add_worktree(
            &run,
            Path::new("/r"),
            "nope",
            Path::new("/w"),
            false,
            WorktreeFrom::Local,
        );
        assert_eq!(r, Err("No branch nope here any more.".into()));
        assert!(!calls.borrow().iter().any(|a| a[0] == "worktree"));
    }

    #[test]
    fn puts_worktrees_under_the_root_one_folder_per_repo_slashes_flattened() {
        assert_eq!(
            worktree_dir(Path::new("/h/wt"), Path::new("/code/api"), "feat/x"),
            Path::new("/h/wt").join("api").join("feat-x")
        );
    }

    #[test]
    fn git_diff_cuts_a_huge_diff_and_says_so() {
        let big = "x".repeat(DIFF_MAX_BYTES + 10);
        let run = |args: &[&str], _: &Path, _: bool| -> Out {
            Ok(if args[0] == "diff" {
                big.clone()
            } else {
                String::new()
            })
        };
        let d = git_diff(&run, Path::new("/r")).unwrap();
        assert!(d.truncated);
        assert_eq!(d.patch.len(), DIFF_MAX_BYTES);
    }

    #[test]
    fn cut_backs_off_to_a_char_boundary() {
        assert_eq!(cut("aé", 2), "a");
        assert_eq!(cut("ab", 5), "ab");
    }

    #[test]
    fn parse_branches_skips_remote_head_and_remote_branches_checked_out_locally() {
        let out = [
            "refs/heads/main\x002 hours ago\x00origin/main",
            "refs/heads/fix\x003 days ago\x00",
            "refs/remotes/origin/HEAD\x002 hours ago\x00",
            "refs/remotes/origin/main\x002 hours ago\x00",
            "refs/remotes/origin/fix\x003 days ago\x00",
            "refs/remotes/origin/new\x001 hour ago\x00",
        ]
        .join("\n");
        let b = parse_branches(&out, Some("main".into()));
        assert_eq!(
            b,
            GitBranches {
                current: Some("main".into()),
                local: vec![
                    LocalBranch {
                        name: "main".into(),
                        when: "2 hours ago".into(),
                        upstream: Some("origin/main".into())
                    },
                    LocalBranch {
                        name: "fix".into(),
                        when: "3 days ago".into(),
                        upstream: None
                    },
                ],
                remote: vec![RemoteBranch {
                    name: "origin/new".into(),
                    when: "1 hour ago".into()
                }],
            }
        );
    }

    #[test]
    fn pull_request_url_builds_github_and_gitlab_pages_from_https_and_ssh_remotes() {
        let url = |u: &str, b: &str| pull_request_url(u, b);
        assert_eq!(
            url("git@github.com:acme/app.git", "feat/x").as_deref(),
            Some("https://github.com/acme/app/compare/feat%2Fx?expand=1")
        );
        assert_eq!(
            url("https://github.com/acme/app", "b").as_deref(),
            Some("https://github.com/acme/app/compare/b?expand=1")
        );
        assert_eq!(
            url("https://token@github.com/acme/app.git\n", "b").as_deref(),
            Some("https://github.com/acme/app/compare/b?expand=1")
        );
        assert_eq!(
            url("ssh://git@gitlab.com/group/sub/app.git", "b").as_deref(),
            Some(
                "https://gitlab.com/group/sub/app/-/merge_requests/new?merge_request%5Bsource_branch%5D=b"
            )
        );
        assert_eq!(url("https://example.com/acme/app.git", "b"), None);
        assert_eq!(url("https://github.com.evil.io/acme/app", "b"), None);
    }

    #[test]
    fn switch_targets_and_worktree_checks_keep_the_ts_json_shape() {
        let t: SwitchTarget =
            serde_json::from_str(r#"{"kind":"remote","name":"origin/x"}"#).unwrap();
        assert_eq!(t, SwitchTarget::Remote("origin/x".into()));
        let from: WorktreeFrom = serde_json::from_str(r#""local""#).unwrap();
        assert_eq!(from, WorktreeFrom::Local);
        let c = WorktreeCheck {
            branch: None,
            changed: 1,
            main_root: PathBuf::from("/r"),
        };
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"branch":null,"changed":1,"mainRoot":"/r"}"#
        );
    }
}
