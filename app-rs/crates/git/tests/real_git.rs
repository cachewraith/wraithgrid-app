//! The git functions against real repositories (port of the real-git suites in
//! `tests/unit/git.test.ts`): worktrees, commit/push/pull, branches and history, through
//! `GitCli` with an empty global config so the user's own config can't change results.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::{NamedTempFile, TempDir};
use wraithgrid_git::*;

/// A real git with a fixed identity and an empty global config. Not /dev/null for the config:
/// on Windows that is `\\.\nul`, which git refuses to open. Timeouts are long: these spawn
/// many git processes and Windows runners can be slow (see docs/CONTEXT.md).
struct Git {
    cli: GitCli,
    _config: NamedTempFile,
}

impl Git {
    fn new() -> Self {
        let config = NamedTempFile::new().unwrap();
        let env = [
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@t"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@t"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .chain([(
            "GIT_CONFIG_GLOBAL".to_string(),
            config.path().to_string_lossy().into_owned(),
        )])
        .collect();
        let cli = GitCli {
            timeout: Duration::from_secs(30),
            slow_timeout: Duration::from_secs(60),
            env,
            ..GitCli::default()
        };
        Self {
            cli,
            _config: config,
        }
    }

    /// Runs git for the test's own setup; a failure fails the test.
    fn run(&self, args: &[&str], cwd: &Path) -> String {
        self.cli
            .run(args, cwd, true)
            .unwrap_or_else(|e| panic!("git {args:?}: {}", e.first_line()))
    }
}

/// A temp folder by its real path (macOS /var → /private/var), as git prints it.
fn temp() -> (TempDir, PathBuf) {
    let t = TempDir::new().unwrap();
    let p = fs::canonicalize(t.path()).unwrap();
    (t, p)
}

fn name(p: &Path) -> &std::ffi::OsStr {
    p.file_name().unwrap()
}

/// A repo on `main` with one commit of a.txt, and an empty folder for worktrees.
struct Repo {
    git: Git,
    dir: PathBuf,
    wt: PathBuf,
    _tmp: (TempDir, TempDir),
}

fn repo() -> Repo {
    let git = Git::new();
    let (t1, dir) = temp();
    let (t2, wt) = temp();
    git.run(&["init", "-q", "-b", "main"], &dir);
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    git.run(&["add", "."], &dir);
    git.run(&["commit", "-qm", "init"], &dir);
    Repo {
        git,
        dir,
        wt,
        _tmp: (t1, t2),
    }
}

#[test]
fn reads_status_and_diff_and_adds_a_worktree_on_a_new_branch() {
    let Repo { git, dir, wt, .. } = &repo();
    let run = &git.cli;
    fs::write(dir.join("a.txt"), "two\n").unwrap();
    fs::write(dir.join("new.txt"), "n\n").unwrap();
    assert_eq!(
        git_status(run, dir),
        GitStatus {
            repo: true,
            branch: Some("main".into()),
            ahead: 0,
            behind: 0,
            changed: 2
        }
    );
    let d = git_diff(run, dir).unwrap();
    assert_eq!(d.untracked, ["new.txt"]);
    let files = parse_patch(&d.patch);
    assert_eq!(files.len(), 1);
    assert_eq!(
        (files[0].path.as_str(), files[0].added, files[0].removed),
        ("a.txt", 1, 1)
    );

    let want = wt.join(name(dir)).join("feat-x");
    let r = add_worktree(run, dir, "feat/x", wt, false, WorktreeFrom::New);
    assert_eq!(r, Ok(want.clone()));
    assert_eq!(git_status(run, &want).branch.as_deref(), Some("feat/x"));
    // The same branch twice is git's error, passed on.
    let again = add_worktree(run, dir, "feat/x", wt, false, WorktreeFrom::New).unwrap_err();
    assert!(again.contains("already exists"), "{again}");
    // A team agent (#79) takes the same folder again, or its branch once the folder is gone.
    assert_eq!(
        add_worktree(run, dir, "feat/x", wt, true, WorktreeFrom::New),
        Ok(want.clone())
    );
    fs::remove_dir_all(&want).unwrap();
    git.run(&["worktree", "prune"], dir);
    assert_eq!(
        add_worktree(run, dir, "feat/x", wt, true, WorktreeFrom::New),
        Ok(want)
    );
}

#[test]
fn opens_an_existing_local_or_remote_branch_in_a_worktree_next_to_the_repo() {
    let Repo { git, dir, wt, .. } = &repo();
    let run = &git.cli;
    git.run(&["branch", "feat/old"], dir);
    let old = wt.join(name(dir)).join("feat-old");
    assert_eq!(
        add_worktree(run, dir, "feat/old", wt, false, WorktreeFrom::Local),
        Ok(old.clone())
    );
    assert_eq!(git_status(run, &old).branch.as_deref(), Some("feat/old"));
    // The main checkout stays where it was.
    assert_eq!(git_status(run, dir).branch.as_deref(), Some("main"));
    // Checked out there already: said plainly, with the folder.
    let twice = add_worktree(run, dir, "feat/old", wt, false, WorktreeFrom::Local).unwrap_err();
    assert!(
        twice.starts_with("feat/old is already checked out in ") && twice.contains("feat-old."),
        "{twice}"
    );
    let main = add_worktree(run, dir, "main", wt, false, WorktreeFrom::Local).unwrap_err();
    assert!(
        main.starts_with("main is already checked out in "),
        "{main}"
    );

    // A worktree made from a worktree goes with the repo's others, not inside its folder name.
    assert_eq!(
        add_worktree(run, &old, "feat/next", wt, false, WorktreeFrom::New),
        Ok(wt.join(name(dir)).join("feat-next"))
    );

    // A remote branch nobody has locally: a new local branch tracking it.
    let bare = wt.join("remote.git");
    git.run(&["init", "-q", "--bare", &bare.to_string_lossy()], wt);
    git.run(&["remote", "add", "origin", &bare.to_string_lossy()], dir);
    git.run(&["push", "-q", "origin", "main:feat/remote"], dir);
    git.run(&["fetch", "-q", "origin"], dir);
    let rem = wt.join(name(dir)).join("feat-remote");
    assert_eq!(
        add_worktree(
            run,
            dir,
            "origin/feat/remote",
            wt,
            false,
            WorktreeFrom::Remote
        ),
        Ok(rem.clone())
    );
    assert_eq!(
        git.run(&["rev-parse", "--abbrev-ref", "@{u}"], &rem).trim(),
        "origin/feat/remote"
    );
    assert_eq!(
        add_worktree(run, dir, "origin/nope", wt, false, WorktreeFrom::Remote),
        Err("No branch origin/nope here any more.".into())
    );
}

#[test]
fn commits_everything_then_pushes_with_an_upstream_on_the_first_push() {
    let Repo { git, dir, wt, .. } = &repo();
    let run = &git.cli;
    assert_eq!(
        commit_all(run, dir, "  "),
        Err("Write a commit message first.".into())
    );
    assert_eq!(
        commit_all(run, dir, &"x".repeat(COMMIT_MESSAGE_MAX + 1)),
        Err("The message is too long.".into())
    );
    assert_eq!(
        push_branch(run, dir),
        Err("This repository has no remote to push to.".into())
    );
    fs::write(dir.join("a.txt"), "two\n").unwrap();
    fs::write(dir.join("b.txt"), "b\n").unwrap();
    let c = commit_all(run, dir, "feat: two files").unwrap();
    let hash = c.strip_prefix("Committed ").unwrap();
    assert!(!hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(git_status(run, dir).changed, 0);
    assert_eq!(
        git.run(&["log", "-1", "--format=%s"], dir).trim(),
        "feat: two files"
    );

    let bare = wt.join("remote.git");
    git.run(&["init", "-q", "--bare", &bare.to_string_lossy()], wt);
    git.run(&["remote", "add", "origin", &bare.to_string_lossy()], dir);
    assert_eq!(push_branch(run, dir), Ok("Pushed main to origin".into()));
    fs::write(dir.join("c.txt"), "c\n").unwrap();
    commit_all(run, dir, "more").unwrap();
    assert_eq!(push_branch(run, dir), Ok("Pushed main".into()));
    assert_eq!(
        git.run(&["log", "-1", "--format=%s", "main"], &bare).trim(),
        "more"
    );

    // The remote is a local folder, not GitHub or GitLab.
    assert_eq!(
        pull_request_link(run, dir),
        Err("Pull requests open for GitHub and GitLab remotes only.".into())
    );
}

#[test]
fn lists_branches_newest_first_and_switches_local_remote_and_new_ones() {
    let Repo { git, dir, wt, .. } = &repo();
    let run = &git.cli;
    let bare = wt.join("remote.git");
    git.run(&["init", "-q", "--bare", &bare.to_string_lossy()], wt);
    git.run(&["remote", "add", "origin", &bare.to_string_lossy()], dir);
    git.run(&["push", "-q", "-u", "origin", "main"], dir);
    // A branch only the remote has (pushed from another checkout).
    git.run(&["push", "-q", "origin", "main:refs/heads/theirs"], dir);
    git.run(&["fetch", "-q", "origin"], dir);
    git.run(&["branch", "-q", "older"], dir);

    let list = list_branches(run, dir).unwrap();
    assert_eq!(list.current.as_deref(), Some("main"));
    let mut local: Vec<&str> = list.local.iter().map(|b| b.name.as_str()).collect();
    local.sort();
    assert_eq!(local, ["main", "older"]);
    let main = list.local.iter().find(|b| b.name == "main").unwrap();
    assert_eq!(main.upstream.as_deref(), Some("origin/main"));
    let remote: Vec<&str> = list.remote.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(remote, ["origin/theirs"]);

    let switch = |to| switch_branch(run, dir, &to);
    assert_eq!(
        switch(SwitchTarget::Local("older".into())),
        Ok("Switched to older".into())
    );
    assert_eq!(
        switch(SwitchTarget::Remote("origin/theirs".into())),
        Ok("Switched to theirs".into())
    );
    assert_eq!(
        git.run(&["rev-parse", "--abbrev-ref", "@{u}"], dir).trim(),
        "origin/theirs"
    );
    assert_eq!(
        switch(SwitchTarget::Create("feat/new".into())),
        Ok("Switched to feat/new".into())
    );
    assert_eq!(
        switch(SwitchTarget::Local("feat/new".into())),
        Ok("Already on feat/new".into())
    );
}

#[test]
fn refuses_unlisted_names_bad_new_names_and_a_switch_that_would_lose_changes() {
    let Repo { git, dir, .. } = &repo();
    let run = &git.cli;
    assert_eq!(
        switch_branch(run, dir, &SwitchTarget::Local("--orphan=x".into())),
        Err("No branch --orphan=x here any more.".into())
    );
    assert_eq!(
        switch_branch(run, dir, &SwitchTarget::Create("-x".into())),
        Err("Not a valid branch name.".into())
    );
    git.run(&["switch", "-qc", "other"], dir);
    fs::write(dir.join("a.txt"), "other\n").unwrap();
    git.run(&["commit", "-qam", "other"], dir);
    git.run(&["switch", "-q", "main"], dir);
    fs::write(dir.join("a.txt"), "uncommitted\n").unwrap();
    let r = switch_branch(run, dir, &SwitchTarget::Local("other".into())).unwrap_err();
    assert!(r.contains("local changes"), "{r}");
    assert_eq!(git_status(run, dir).branch.as_deref(), Some("main"));
}

#[test]
fn removes_only_linked_worktrees_and_dirty_ones_only_when_forced() {
    let Repo { git, dir, wt, .. } = &repo();
    let run = &git.cli;
    assert_eq!(
        check_worktree(run, dir),
        Err("Not a linked worktree.".into())
    );
    let w = add_worktree(run, dir, "feat/y", wt, false, WorktreeFrom::New).unwrap();
    assert_eq!(
        check_worktree(run, &w),
        Ok(WorktreeCheck {
            branch: Some("feat/y".into()),
            changed: 0,
            main_root: dir.clone(),
        })
    );
    fs::write(w.join("wip.txt"), "w\n").unwrap();
    let refused = remove_worktree(run, &w, false).unwrap_err();
    assert!(refused.starts_with("1 uncommitted files"), "{refused}");
    assert_eq!(
        remove_worktree(run, &w, true),
        Ok("Kept branch feat/y".into())
    );
    let list = git.run(&["worktree", "list"], dir);
    assert_eq!(list.split('\n').filter(|l| !l.is_empty()).count(), 1);
    // The branch stays.
    assert_eq!(
        git.run(&["branch", "--list", "feat/y"], dir).trim(),
        "feat/y"
    );
}

/// A bare remote on `main`, a clone of it with one pushed commit, and a second clone.
struct Pull {
    git: Git,
    dir: PathBuf,
    other: PathBuf,
    _tmp: TempDir,
}

fn pull_repos() -> Pull {
    let git = Git::new();
    let (t, root) = temp();
    let bare = root.join("remote.git").to_string_lossy().into_owned();
    let dir = root.join("mine");
    let other = root.join("theirs");
    git.run(&["init", "-q", "--bare", "-b", "main", &bare], &root);
    git.run(&["clone", "-q", &bare, &dir.to_string_lossy()], &root);
    git.run(&["checkout", "-q", "-b", "main"], &dir);
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    commit_all(&git.cli, &dir, "first").unwrap();
    git.run(&["push", "-q", "-u", "origin", "main"], &dir);
    git.run(&["clone", "-q", &bare, &other.to_string_lossy()], &root);
    Pull {
        git,
        dir,
        other,
        _tmp: t,
    }
}

#[test]
fn says_up_to_date_pulls_new_commits_and_reports_what_git_refuses() {
    let Pull {
        git, dir, other, ..
    } = &pull_repos();
    let run = &git.cli;
    assert_eq!(pull_branch(run, dir), Ok("Up to date".into()));

    for n in ["2", "3"] {
        fs::write(other.join("a.txt"), format!("{n}\n")).unwrap();
        commit_all(run, other, &format!("theirs {n}")).unwrap();
    }
    git.run(&["push", "-q"], other);
    assert_eq!(pull_branch(run, dir), Ok("Pulled 2 commits".into()));
    assert_eq!(
        git.run(&["log", "-1", "--format=%s"], dir).trim(),
        "theirs 3"
    );

    // Ahead of the remote: up to date, with commits to push.
    fs::write(dir.join("b.txt"), "b\n").unwrap();
    commit_all(run, dir, "mine").unwrap();
    assert_eq!(
        pull_branch(run, dir),
        Ok("Up to date · 1 commit to push".into())
    );
    git.run(&["push", "-q"], dir);
    git.run(&["pull", "-q"], other);

    // A local change in the way: git's own error, nothing pulled.
    fs::write(other.join("a.txt"), "4\n").unwrap();
    commit_all(run, other, "theirs 4").unwrap();
    git.run(&["push", "-q"], other);
    fs::write(dir.join("a.txt"), "mine, not committed\n").unwrap();
    let r = pull_branch(run, dir).unwrap_err();
    assert!(
        r.contains("local changes") || r.contains("overwritten"),
        "{r}"
    );
    assert_eq!(git.run(&["log", "-1", "--format=%s"], dir).trim(), "mine");
}

#[test]
fn refuses_to_pull_without_an_upstream_or_on_a_detached_head() {
    let Pull { git, dir, .. } = &pull_repos();
    let run = &git.cli;
    git.run(&["checkout", "-q", "-b", "solo"], dir);
    assert_eq!(
        pull_branch(run, dir),
        Err("solo has no upstream branch to pull from. Push it first.".into())
    );
    git.run(&["checkout", "-q", "--detach"], dir);
    assert_eq!(
        pull_branch(run, dir),
        Err("Not on a branch (detached HEAD).".into())
    );
    assert_eq!(
        push_branch(run, dir),
        Err("Not on a branch (detached HEAD).".into())
    );
}

#[test]
fn lists_history_with_merges_and_refs_a_page_at_a_time_and_shows_a_commit() {
    let Pull { git, dir, .. } = &pull_repos();
    let run = &git.cli;
    git.run(&["checkout", "-q", "-b", "feat"], dir);
    fs::write(dir.join("f.txt"), "f\n").unwrap();
    commit_all(run, dir, "feature").unwrap();
    git.run(&["checkout", "-q", "main"], dir);
    fs::write(dir.join("m.txt"), "m\n").unwrap();
    commit_all(run, dir, "on main").unwrap();
    git.run(&["merge", "-q", "--no-ff", "-m", "Merge feat", "feat"], dir);
    git.run(&["tag", "v1"], dir);

    let log = git_log(run, dir, 0).unwrap();
    assert!(!log.more);
    let subjects: Vec<&str> = log.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, ["Merge feat", "on main", "feature", "first"]);
    assert_eq!(log.commits[0].parents.len(), 2);
    for r in ["HEAD -> main", "tag: v1"] {
        assert!(log.commits[0].refs.iter().any(|x| x == r), "{r}");
    }
    let rest = git_log(run, dir, 3).unwrap();
    let subjects: Vec<&str> = rest.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, ["first"]);

    // The graph lays the page out: the merge opens a second lane.
    let graph: Vec<GraphCommit> = log.commits.iter().map(GraphCommit::from).collect();
    let rows = layout_graph(&graph);
    assert_eq!(rows.iter().map(|r| r.width).max(), Some(2));

    let feature = log.commits.iter().find(|c| c.subject == "feature").unwrap();
    let shown = git_show(run, dir, &feature.sha).unwrap();
    let paths: Vec<String> = parse_patch(&shown.patch)
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(paths, ["f.txt"]);
    assert_eq!(git_show(run, dir, "--all"), Err("Not a commit id.".into()));
}

#[test]
fn an_empty_repository_has_no_history() {
    let git = Git::new();
    let (_t, dir) = temp();
    git.run(&["init", "-q", "-b", "main"], &dir);
    assert_eq!(git_log(&git.cli, &dir, 0), Ok(GitLog::default()));
    // No HEAD yet: the diff falls back to the unstaged one.
    fs::write(dir.join("a.txt"), "a\n").unwrap();
    let d = git_diff(&git.cli, &dir).unwrap();
    assert_eq!(d.untracked, ["a.txt"]);
}

#[test]
fn the_runner_keeps_what_came_first_past_its_buffer_and_reports_git_errors() {
    let git = Git::new();
    let (_t, dir) = temp();
    let small = GitCli {
        max_buffer: 10,
        ..git.cli.clone()
    };
    let e = small.run(&["--version"], &dir, false).unwrap_err();
    assert!(e.overflowed);
    assert_eq!(e.overflow_output(), Some("git versio"));
    // Not a repository (git may not look above the temp folder): git's fatal line, without
    // its prefix.
    let mut outside = git.cli.clone();
    let ceiling = dir.parent().unwrap().to_string_lossy().into_owned();
    outside
        .env
        .push(("GIT_CEILING_DIRECTORIES".into(), ceiling));
    let e = outside.run(&["status"], &dir, false).unwrap_err();
    assert!(!e.overflowed);
    assert!(e.first_line().starts_with("not a git repository"), "{e}");
    assert!(!git_status(&outside, &dir).repo);
}
