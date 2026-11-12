//! Port of `tests/unit/file-mention.test.ts`: the `@` query, sent mentions, listing a
//! project (git or a walk) and ranking its paths.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use wraithgrid_chat::files::*;
use wraithgrid_chat::slash::{file_query, split_mentions, TextPart, WordQuery};

fn not_repo(_: &[&str], _: &Path) -> Result<String, GitFailure> {
    Err(GitFailure::Failed)
}

fn now() -> i64 {
    wraithgrid_chat::now_ms()
}

fn project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for d in ["src/main", "node_modules/left-pad", ".git/refs", "docs"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    for f in [
        "src/main/app.ts",
        "src/main/ipc.ts",
        "src/index.ts",
        "README.md",
        "node_modules/left-pad/index.js",
        ".git/HEAD",
        "docs/guide.md",
    ] {
        std::fs::write(root.join(f), "").unwrap();
    }
    (dir, root)
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn opens_on_an_at_word_at_the_start_or_after_a_space() {
    let wq = |start, query: &str| {
        Some(WordQuery {
            start,
            query: query.into(),
        })
    };
    assert_eq!(file_query("@", 1), wq(0, ""));
    assert_eq!(file_query("look at @src/ma", 15), wq(8, "src/ma"));
    assert_eq!(file_query("look at @src/ma and", 15), wq(8, "src/ma"));
}

#[test]
fn does_not_open_for_an_email_after_a_space_or_a_second_at() {
    assert_eq!(file_query("me@example.com", 14), None);
    assert_eq!(file_query("@src done", 9), None);
    assert_eq!(file_query("@a@b", 4), None);
}

#[test]
fn splits_at_paths_out_of_the_text() {
    let p = |text: &str, mention| TextPart {
        text: text.into(),
        mention,
    };
    assert_eq!(
        split_mentions("read @src/app.ts and @README.md, thanks @bob"),
        [
            p("read ", false),
            p("@src/app.ts", true),
            p(" and ", false),
            p("@README.md,", true),
            p(" thanks @bob", false)
        ]
    );
    assert_eq!(split_mentions("@docs/"), [p("@docs/", true)]);
    assert_eq!(split_mentions("mail me@x.com"), [p("mail me@x.com", false)]);
}

#[test]
fn walks_a_folder_that_is_not_a_repo_skipping_git_and_node_modules() {
    let (_dir, root) = project();
    let mut paths = list_project_paths(&root, &not_repo, &now);
    paths.sort();
    let mut want = strings(&[
        "README.md",
        "docs/",
        "docs/guide.md",
        "src/",
        "src/index.ts",
        "src/main/",
        "src/main/app.ts",
        "src/main/ipc.ts",
    ]);
    want.sort();
    assert_eq!(paths, want);
}

#[test]
fn uses_git_ls_files_in_a_repo_adding_the_folders() {
    let calls: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
    let git = |args: &[&str], _: &Path| {
        calls
            .lock()
            .unwrap()
            .push(args.iter().map(|s| s.to_string()).collect());
        Ok("src/a.ts\0src/lib/b.ts\0node_modules/x.js\0README.md\0".to_string())
    };
    assert_eq!(
        list_project_paths(Path::new("/p"), &git, &now),
        strings(&["src/", "src/lib/", "src/a.ts", "src/lib/b.ts", "README.md"])
    );
    assert_eq!(
        calls.lock().unwrap()[0],
        strings(&[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z"
        ])
    );
}

#[test]
fn keeps_what_git_listed_before_it_outran_the_buffer() {
    let git = |_: &[&str], _: &Path| Err(GitFailure::Overflowed("a.ts\0b.ts\0c.t".into()));
    assert_eq!(
        list_project_paths(Path::new("/p"), &git, &now),
        strings(&["a.ts", "b.ts"])
    );
}

#[test]
fn reuses_a_folders_list_for_a_few_seconds() {
    let t = Arc::new(AtomicI64::new(0));
    let runs = Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let clock = t.clone();
    let list = FileLister::with_clock(
        move |_: &[&str], _: &Path| {
            r.fetch_add(1, Ordering::SeqCst);
            Ok("a.ts\0".to_string())
        },
        move || clock.load(Ordering::SeqCst),
    );
    list.list(Path::new("/p"));
    assert_eq!(*list.list(Path::new("/p")), strings(&["a.ts"]));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    t.fetch_add(FILE_LIST_TTL_MS, Ordering::SeqCst);
    list.list(Path::new("/p"));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

fn paths() -> Vec<String> {
    with_folders(&strings(&[
        "src/main/app.ts",
        "src/main/apply.ts",
        "src/renderer/App.tsx",
        "src/shared/mapping.ts",
        "tests/unit/app.test.ts",
        "README.md",
        "package.json",
    ]))
}

#[test]
fn lists_the_top_level_for_a_bare_at_folders_first() {
    assert_eq!(
        rank_paths(&paths(), "", FILE_MENTIONS_MAX),
        strings(&["src/", "tests/", "package.json", "README.md"])
    );
}

#[test]
fn puts_file_name_matches_before_path_matches() {
    assert_eq!(
        rank_paths(&paths(), "app", FILE_MENTIONS_MAX)[..4],
        strings(&[
            "src/main/app.ts",
            "src/renderer/App.tsx",
            "tests/unit/app.test.ts",
            "src/main/apply.ts"
        ])
    );
    assert_eq!(
        rank_paths(&paths(), "src/ma", FILE_MENTIONS_MAX)[..3],
        strings(&["src/main/", "src/main/app.ts", "src/main/apply.ts"])
    );
}

#[test]
fn opens_a_folder() {
    assert_eq!(
        rank_paths(&paths(), "src/", FILE_MENTIONS_MAX),
        strings(&["src/main/", "src/renderer/", "src/shared/"])
    );
}

#[test]
fn caps_the_answer_and_matches_nothing_for_nonsense() {
    assert_eq!(rank_paths(&paths(), "a", 2).len(), 2);
    assert!(rank_paths(&paths(), "zzz", FILE_MENTIONS_MAX).is_empty());
}
