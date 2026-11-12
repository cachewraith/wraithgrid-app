//! The commit history for the Changes panel's History tab (port of the history half of
//! `src/main/git.ts`): every branch and tag a page at a time, and one commit's diff.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::git::{cut, DIFF_MAX_BYTES};
use crate::runner::GitRunner;

/// Commits per page of the history graph.
pub const LOG_PAGE: usize = 100;

/// sha, parents, author, committer time, decoration, subject: unit-separated, record-ended.
const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ct%x1f%D%x1f%s%x1e";

/// One commit in the history graph; `refs` as git decorates it (`HEAD -> main`, `tag: v1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    /// Committer time in ms.
    pub time: i64,
    pub refs: Vec<String>,
    pub subject: String,
}

/// A page of history; `more` when there is a next page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLog {
    pub commits: Vec<GitCommit>,
    pub more: bool,
}

/// One commit's patch, cut like the working-tree diff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitShow {
    pub patch: String,
    pub truncated: bool,
}

/// A full or abbreviated commit id (`[0-9a-f]{7,40}`), so nothing else reaches git as one.
fn is_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Parses `git log` output in `LOG_FORMAT`; rows without a commit id are dropped.
pub fn parse_log(out: &str) -> Vec<GitCommit> {
    out.split('\x1e')
        .map(|r| r.strip_prefix('\n').unwrap_or(r))
        .filter(|r| !r.is_empty())
        .filter_map(|r| {
            let mut f = r.split('\x1f');
            let sha = f.next().filter(|s| is_sha(s))?;
            let parents = f.next().unwrap_or_default();
            let author = f.next().unwrap_or_default();
            let time = f.next().unwrap_or_default();
            let refs = f.next().unwrap_or_default();
            let subject = f.next().unwrap_or_default();
            Some(GitCommit {
                sha: sha.to_string(),
                parents: parents
                    .split(' ')
                    .filter(|p| is_sha(p))
                    .map(String::from)
                    .collect(),
                author: author.to_string(),
                time: time.trim().parse::<i64>().map_or(0, |t| t * 1000),
                refs: refs
                    .split(", ")
                    .map(str::trim)
                    .filter(|x| !x.is_empty() && *x != "HEAD")
                    .map(String::from)
                    .collect(),
                subject: subject.chars().take(300).collect(),
            })
        })
        .collect()
}

static NO_HISTORY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("does not have any commits|bad default revision|unknown revision").unwrap()
});

/// A page of history for every branch and tag, newest first (topological within a date).
pub fn git_log(run: &dyn GitRunner, cwd: &Path, skip: usize) -> Result<GitLog, String> {
    let max = format!("--max-count={}", LOG_PAGE + 1);
    let skip = format!("--skip={skip}");
    let args = [
        "log",
        "--branches",
        "--remotes",
        "--tags",
        "HEAD",
        "--date-order",
        &max,
        &skip,
        LOG_FORMAT,
        "--no-color",
    ];
    match run.run(&args, cwd, false) {
        Ok(out) => {
            let mut commits = parse_log(&out);
            let more = commits.len() > LOG_PAGE;
            commits.truncate(LOG_PAGE);
            Ok(GitLog { commits, more })
        }
        Err(e) => {
            let msg = e.first_line();
            // A repository with no commits has no history, which is not an error.
            if NO_HISTORY.is_match(&msg) {
                Ok(GitLog::default())
            } else {
                Err(msg)
            }
        }
    }
}

/// One commit's changes as a patch (cut like the working-tree diff); a merge shows what it
/// brought in against its first parent.
pub fn git_show(run: &dyn GitRunner, cwd: &Path, sha: &str) -> Result<GitShow, String> {
    if !is_sha(sha) {
        return Err("Not a commit id.".into());
    }
    let args = [
        "show",
        "--format=",
        "--no-color",
        "--no-ext-diff",
        "--first-parent",
        "-m",
        sha,
        "--",
    ];
    match run.run(&args, cwd, false) {
        Ok(patch) => {
            let truncated = patch.len() > DIFF_MAX_BYTES;
            Ok(GitShow {
                patch: if truncated {
                    cut(&patch, DIFF_MAX_BYTES).to_string()
                } else {
                    patch
                },
                truncated,
            })
        }
        Err(e) if e.overflowed => Err("This commit is too large to show.".into()),
        Err(e) => Err(e.first_line()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::GitError;

    #[test]
    fn parse_log_reads_the_fields_and_drops_bad_rows() {
        let sha = "a".repeat(40);
        let p1 = "b".repeat(40);
        let p2 = "c".repeat(40);
        let parents = format!("{p1} {p2}");
        let good = [
            sha.as_str(),
            &parents,
            "Ada",
            "1700000000",
            "HEAD -> main, tag: v1",
            "Merge feat",
        ];
        let out =
            good.join("\x1f") + "\x1e\n" + &["nope", "", "", "", "", "bad"].join("\x1f") + "\x1e\n";
        assert_eq!(
            parse_log(&out),
            vec![GitCommit {
                sha,
                parents: vec![p1, p2],
                author: "Ada".into(),
                time: 1_700_000_000_000,
                refs: vec!["HEAD -> main".into(), "tag: v1".into()],
                subject: "Merge feat".into(),
            }]
        );
    }

    #[test]
    fn git_log_treats_an_empty_repository_as_no_history() {
        let run = |_: &[&str], _: &Path, _: bool| -> Result<String, GitError> {
            Err(GitError {
                stderr: "fatal: your current branch 'main' does not have any commits yet\n".into(),
                ..GitError::default()
            })
        };
        assert_eq!(git_log(&run, Path::new("/r"), 0), Ok(GitLog::default()));
    }

    #[test]
    fn git_show_refuses_anything_but_a_commit_id() {
        let run = |_: &[&str], _: &Path, _: bool| -> Result<String, GitError> { panic!("git ran") };
        assert_eq!(
            git_show(&run, Path::new("/r"), "--all"),
            Err("Not a commit id.".into())
        );
    }
}
