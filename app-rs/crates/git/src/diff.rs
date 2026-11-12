//! Splits `git diff` output into files and typed lines for the Changes panel (port of
//! `src/renderer/lib/diff.ts`). Pure, so the panel only renders; a diff it can't make sense of
//! still shows as plain context lines.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::git::GitDiff;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffLineKind {
    Add,
    Del,
    Ctx,
    Hunk,
    Meta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub path: String,
    /// Set when the file was renamed.
    pub old_path: Option<String>,
    pub status: DiffStatus,
    pub binary: bool,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<DiffLine>,
}

/// JavaScript's `/\t.*$/` without the `m` flag: from a tab to the end, where `.` stops at
/// `\r`, U+2028 and U+2029 (so a later tab may be the one that matches).
fn cut_tab_suffix(n: &str) -> &str {
    let stops = |s: &str| s.contains(['\r', '\u{2028}', '\u{2029}']);
    n.match_indices('\t')
        .find(|(i, _)| !stops(&n[*i..]))
        .map_or(n, |(i, _)| &n[..i])
}

/// `a/src/x.ts` → `src/x.ts`; `/dev/null` → None. Quoted names keep their escapes.
fn strip_prefix(name: &str) -> Option<String> {
    let n = name.strip_prefix('"').unwrap_or(name);
    let n = n.strip_suffix('"').unwrap_or(n);
    let n = cut_tab_suffix(n);
    if n == "/dev/null" {
        return None;
    }
    let n = n
        .strip_prefix("a/")
        .or_else(|| n.strip_prefix("b/"))
        .unwrap_or(n);
    Some(n.to_string())
}

/// `a/my file.png b/my file.png`: unquoted paths with spaces can't be split on a space, but
/// both sides name the same file unless renamed (then `rename to` sets it), so cut in the middle.
fn both_sides(rest: &str) -> &str {
    let len = rest.len();
    // An odd length has a middle byte; it must be the space between the sides.
    if len % 2 == 1 && rest.as_bytes()[len / 2] == b' ' && rest.starts_with("a/") {
        let b = &rest[len / 2 + 1..];
        if b.starts_with("b/") {
            return b;
        }
    }
    rest
}

static DIFF_GIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^diff --git (\S+|"[^"]*") (\S+|"[^"]*")$"#).unwrap());

/// A line without its first character (the `+`, `-` or ` ` marker).
fn body(line: &str) -> String {
    let mut chars = line.chars();
    chars.next();
    chars.as_str().to_string()
}

pub fn parse_patch(patch: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut in_hunk = false;

    for line in patch.split('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let name = match DIFF_GIT.captures(line) {
                Some(m) => m.get(2).map_or("", |g| g.as_str()),
                None => both_sides(rest),
            };
            files.push(DiffFile {
                path: strip_prefix(name).unwrap_or_default(),
                old_path: None,
                status: DiffStatus::Modified,
                binary: false,
                added: 0,
                removed: 0,
                lines: Vec::new(),
            });
            in_hunk = false;
            continue;
        }
        let Some(cur) = files.last_mut() else {
            continue;
        };
        if !in_hunk {
            if line.starts_with("new file mode") {
                cur.status = DiffStatus::Added;
            } else if line.starts_with("deleted file mode") {
                cur.status = DiffStatus::Deleted;
            } else if let Some(from) = line.strip_prefix("rename from ") {
                cur.status = DiffStatus::Renamed;
                cur.old_path = Some(from.to_string());
            } else if let Some(to) = line.strip_prefix("rename to ") {
                cur.path = to.to_string();
            } else if line.starts_with("Binary files ") {
                cur.binary = true;
            } else if let Some(name) = line.strip_prefix("+++ ") {
                if let Some(p) = strip_prefix(name).filter(|p| !p.is_empty()) {
                    cur.path = p;
                }
            } else if line.starts_with("@@") {
                in_hunk = true;
                cur.lines.push(DiffLine {
                    kind: DiffLineKind::Hunk,
                    text: line.to_string(),
                });
            }
            continue;
        }
        let (kind, text) = if line.starts_with("@@") {
            (DiffLineKind::Hunk, line.to_string())
        } else if line.starts_with('+') {
            cur.added += 1;
            (DiffLineKind::Add, body(line))
        } else if line.starts_with('-') {
            cur.removed += 1;
            (DiffLineKind::Del, body(line))
        } else if line.starts_with('\\') {
            (DiffLineKind::Meta, line.to_string())
        } else if !line.is_empty() || !cur.lines.is_empty() {
            (DiffLineKind::Ctx, body(line))
        } else {
            continue;
        };
        cur.lines.push(DiffLine { kind, text });
    }
    // A trailing newline in the patch leaves one empty context line behind.
    for f in &mut files {
        if f.lines
            .last()
            .is_some_and(|l| l.kind == DiffLineKind::Ctx && l.text.is_empty())
        {
            f.lines.pop();
        }
    }
    files
}

/// Two fetches of a folder's changes say the same: the panel keeps what it shows (#139).
/// Errors are the same when their messages are.
pub fn same_diff(a: &Result<GitDiff, String>, b: &Result<GitDiff, String>) -> bool {
    match (a, b) {
        (Ok(a), Ok(b)) => {
            a.patch == b.patch && a.truncated == b.truncated && a.untracked == b.untracked
        }
        (Err(a), Err(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_files_and_counts_lines_with_renames_new_files_and_binaries() {
        let patch = [
            "diff --git a/src/x.ts b/src/x.ts",
            "index 1..2 100644",
            "--- a/src/x.ts",
            "+++ b/src/x.ts",
            "@@ -1,2 +1,2 @@",
            " keep",
            "-old",
            "+new",
            "\\ No newline at end of file",
            "diff --git a/n.md b/n.md",
            "new file mode 100644",
            "--- /dev/null",
            "+++ b/n.md",
            "@@ -0,0 +1 @@",
            "+hi",
            "diff --git a/old.ts b/new.ts",
            "similarity index 100%",
            "rename from old.ts",
            "rename to new.ts",
            "diff --git a/img.png b/img.png",
            "Binary files a/img.png and b/img.png differ",
            "",
        ]
        .join("\n");
        let files = parse_patch(&patch);
        let summary: Vec<_> = files
            .iter()
            .map(|f| (f.path.as_str(), f.status, f.added, f.removed, f.binary))
            .collect();
        assert_eq!(
            summary,
            [
                ("src/x.ts", DiffStatus::Modified, 1, 1, false),
                ("n.md", DiffStatus::Added, 1, 0, false),
                ("new.ts", DiffStatus::Renamed, 0, 0, false),
                ("img.png", DiffStatus::Modified, 0, 0, true),
            ]
        );
        let kinds: Vec<_> = files[0].lines.iter().map(|l| l.kind).collect();
        use DiffLineKind::*;
        assert_eq!(kinds, [Hunk, Ctx, Del, Add, Meta]);
        assert_eq!(files[0].lines[1].text, "keep");
        assert_eq!(files[2].old_path.as_deref(), Some("old.ts"));
    }

    #[test]
    fn returns_nothing_for_an_empty_diff() {
        assert_eq!(parse_patch(""), []);
    }

    #[test]
    fn names_files_with_spaces_quoted_names_and_deletions() {
        let patch = [
            "diff --git a/my file.png b/my file.png",
            "Binary files a/my file.png and b/my file.png differ",
            "diff --git \"a/t\\303\\251.txt\" \"b/t\\303\\251.txt\"",
            "deleted file mode 100644",
            "--- \"a/t\\303\\251.txt\"\t",
            "+++ /dev/null",
            "@@ -1 +0,0 @@",
            "-gone",
        ]
        .join("\n");
        let files = parse_patch(&patch);
        assert_eq!(files[0].path, "my file.png");
        assert!(files[0].binary);
        // `+++ /dev/null` keeps the name from the header line.
        assert_eq!(files[1].path, "t\\303\\251.txt");
        assert_eq!(files[1].status, DiffStatus::Deleted);
        assert_eq!(files[1].removed, 1);
    }

    #[test]
    fn keeps_the_ts_json_shape() {
        let files = parse_patch("diff --git a/x b/x\n@@ -1 +1 @@\n+y\n");
        assert_eq!(
            serde_json::to_string(&files).unwrap(),
            r#"[{"path":"x","oldPath":null,"status":"modified","binary":false,"added":1,"removed":0,"lines":[{"kind":"hunk","text":"@@ -1 +1 @@"},{"kind":"add","text":"y"}]}]"#
        );
    }

    #[test]
    fn same_diff_tells_a_changed_patch_or_untracked_list_apart_and_errors_by_message() {
        let ok = |patch: &str, untracked: &[&str]| -> Result<GitDiff, String> {
            Ok(GitDiff {
                patch: patch.into(),
                truncated: false,
                untracked: untracked.iter().map(|f| f.to_string()).collect(),
            })
        };
        let err = |e: &str| -> Result<GitDiff, String> { Err(e.into()) };
        assert!(same_diff(&ok("a", &[]), &ok("a", &[])));
        assert!(!same_diff(&ok("a", &[]), &ok("a2", &[])));
        assert!(!same_diff(&ok("a", &["x"]), &ok("a", &["y"])));
        assert!(same_diff(&err("e"), &err("e")));
        assert!(!same_diff(&err("e"), &ok("", &[])));
    }
}
