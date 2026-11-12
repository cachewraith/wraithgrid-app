//! Port of `tests/unit/transcript.test.ts`: copying a conversation to another account's
//! config dir (claude and codex), plus the saved-view store.

use std::fs;
use std::path::{Path, PathBuf};

use wraithgrid_chat::transcript::{copy_codex_rollout, copy_transcript, is_strictly_inside};
use wraithgrid_chat::views;

const SID: &str = "bb740a2b-b0b4-422a-b18f-89ef6a25b391";
const NOT_FOUND: &str = "This conversation’s file was not found.";

struct Setup {
    _dir: tempfile::TempDir,
    root: PathBuf,
    a: PathBuf,
    b: PathBuf,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("accounts");
    let a = root.join("work");
    let b = root.join("home");
    let proj = a.join("projects").join("-home-me-proj");
    fs::create_dir_all(&proj).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(proj.join(format!("{SID}.jsonl")), "{\"type\":\"user\"}\n").unwrap();
    fs::create_dir_all(proj.join(SID).join("subagents")).unwrap();
    fs::write(proj.join(SID).join("subagents").join("x.jsonl"), "sub\n").unwrap();
    Setup {
        _dir: dir,
        root,
        a,
        b,
    }
}

fn read(p: impl AsRef<Path>) -> String {
    fs::read_to_string(p).unwrap()
}

#[test]
fn copies_the_conversation_and_its_subagent_files_to_the_same_project_folder() {
    let s = setup();
    assert_eq!(copy_transcript(&s.a, &s.b, SID, &s.root), Ok(()));
    let dst = s.b.join("projects").join("-home-me-proj");
    assert_eq!(
        read(dst.join(format!("{SID}.jsonl"))),
        "{\"type\":\"user\"}\n"
    );
    assert_eq!(
        read(dst.join(SID).join("subagents").join("x.jsonl")),
        "sub\n"
    );
    // The original stays with the first account.
    assert!(s
        .a
        .join("projects/-home-me-proj")
        .join(format!("{SID}.jsonl"))
        .exists());
}

#[test]
fn switching_back_overwrites_the_older_copy() {
    let s = setup();
    copy_transcript(&s.a, &s.b, SID, &s.root).unwrap();
    let in_b =
        s.b.join("projects/-home-me-proj")
            .join(format!("{SID}.jsonl"));
    fs::write(&in_b, "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n").unwrap();
    assert_eq!(copy_transcript(&s.b, &s.a, SID, &s.root), Ok(()));
    assert_eq!(
        read(
            s.a.join("projects/-home-me-proj")
                .join(format!("{SID}.jsonl"))
        ),
        "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n"
    );
}

#[test]
fn refuses_dirs_outside_the_accounts_folder_bad_ids_and_the_same_account() {
    let s = setup();
    let outside = tempfile::tempdir().unwrap();
    let out = outside.path();
    assert!(copy_transcript(&s.a, out, SID, &s.root).is_err());
    assert!(copy_transcript(out, &s.b, SID, &s.root).is_err());
    assert!(copy_transcript(&s.root, &s.b, SID, &s.root).is_err());
    for bad in ["../x", "a/b", "", "..", &"x".repeat(200)] {
        assert_eq!(
            copy_transcript(&s.a, &s.b, bad, &s.root),
            Err("Not a conversation id.".into())
        );
    }
    assert!(copy_transcript(&s.a, &s.a, SID, &s.root).is_err());
    // A path that only looks inside once `..` is worked out is outside.
    assert!(copy_transcript(&s.a, &s.root.join("x/../../escape"), SID, &s.root).is_err());
    assert!(!out.join("projects").exists());
}

#[test]
fn says_so_when_the_conversation_file_is_not_there() {
    let s = setup();
    assert_eq!(
        copy_transcript(&s.b, &s.a, SID, &s.root),
        Err(NOT_FOUND.into())
    );
    assert!(copy_transcript(&s.a, &s.b, "other-session", &s.root).is_err());
}

const TID: &str = "01a162ae-bfc7-7822-b8f5-0ca9e9ddd3b4";

fn rel() -> PathBuf {
    Path::new("sessions/2026/10/22").join(format!("rollout-2026-10-22T15-16-21-{TID}.jsonl"))
}

fn codex_setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("accounts");
    let a = root.join("org-codex");
    let b = root.join("infra-codex");
    fs::create_dir_all(a.join(rel()).parent().unwrap()).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join(rel()), "{\"type\":\"session_meta\"}\n").unwrap();
    // Another thread the same day must not be taken for it.
    fs::write(
        a.join("sessions/2026/10/22/rollout-2026-10-22T09-00-00-other.jsonl"),
        "",
    )
    .unwrap();
    Setup {
        _dir: dir,
        root,
        a,
        b,
    }
}

#[test]
fn copies_the_rollout_file_to_the_same_dated_folder() {
    let s = codex_setup();
    assert_eq!(copy_codex_rollout(&s.a, &s.b, TID, &s.root), Ok(()));
    assert_eq!(read(s.b.join(rel())), "{\"type\":\"session_meta\"}\n");
    assert!(s.a.join(rel()).exists());
    let names: Vec<_> = fs::read_dir(s.b.join("sessions/2026/10/22"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, [rel().file_name().unwrap()]);
}

#[test]
fn codex_refuses_outside_dirs_bad_ids_the_same_account_and_a_missing_file() {
    let s = codex_setup();
    let outside = tempfile::tempdir().unwrap();
    assert!(copy_codex_rollout(&s.a, outside.path(), TID, &s.root).is_err());
    assert_eq!(
        copy_codex_rollout(&s.a, &s.b, "../etc", &s.root),
        Err("Not a conversation id.".into())
    );
    assert!(copy_codex_rollout(&s.a, &s.a, TID, &s.root).is_err());
    assert_eq!(
        copy_codex_rollout(&s.b, &s.a, TID, &s.root),
        Err(NOT_FOUND.into())
    );
}

#[test]
fn strictly_inside_means_below_and_not_the_folder_itself() {
    assert!(is_strictly_inside(Path::new("/a/b"), Path::new("/a")));
    assert!(!is_strictly_inside(Path::new("/a"), Path::new("/a")));
    assert!(!is_strictly_inside(Path::new("/ab"), Path::new("/a")));
    assert!(!is_strictly_inside(Path::new("/a/b/.."), Path::new("/a")));
}

#[test]
fn saves_loads_and_deletes_a_panes_view() {
    let dir = tempfile::tempdir().unwrap();
    let chats = dir.path().join("chats");
    assert_eq!(views::load_view(&chats, "p1"), None);
    views::save_view(&chats, "p1", "{\"v\":1}").unwrap();
    assert_eq!(views::load_view(&chats, "p1").as_deref(), Some("{\"v\":1}"));
    views::save_view(&chats, "p1", "{\"v\":1,\"items\":[]}").unwrap();
    assert_eq!(
        views::load_view(&chats, "p1").as_deref(),
        Some("{\"v\":1,\"items\":[]}")
    );
    // Only the view file is left behind.
    assert_eq!(fs::read_dir(&chats).unwrap().count(), 1);
    assert!(views::save_view(&chats, "../evil", "x").is_err());
    assert_eq!(views::view_file(&chats, "a/b"), None);
    views::delete_view(&chats, "p1").unwrap();
    views::delete_view(&chats, "p1").unwrap();
    assert_eq!(views::load_view(&chats, "p1"), None);
}
