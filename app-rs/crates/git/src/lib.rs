//! Git for Wraithgrid's panes (port of `src/main/git.ts` and the renderer's `diff.ts` and
//! `commit-graph.ts`): status, diff, commit, push/pull, branches, worktrees and history, plus
//! the pure parsing the Changes panel draws from. No UI here; every git call is blocking, so
//! callers run it off the UI thread.

pub mod commit_graph;
pub mod diff;
mod git;
mod history;
mod runner;

pub use commit_graph::{layout_graph, ref_labels, Edge, GraphCommit, GraphRow, RefKind, RefLabel};
pub use diff::{parse_patch, same_diff, DiffFile, DiffLine, DiffLineKind, DiffStatus};
pub use git::*;
pub use history::{git_log, git_show, parse_log, GitCommit, GitLog, GitShow, LOG_PAGE};
pub use runner::{GitCli, GitError, GitRunner};
