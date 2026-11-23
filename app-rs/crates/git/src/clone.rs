//! Cloning a Git repository into a new project folder (New project → Git repository, and the
//! `wraithgrid://clone` link). The URL may come from a web page, so only remote https/ssh
//! forms pass: no local paths, `file://`, `ext::` helpers or anything git would take as an
//! option; it reaches git after `--`.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::git::valid_branch_name;
use crate::runner::{GitError, GitRunner};

pub const CLONE_URL_MAX: usize = 2000;

/// `https://[user[:token]@]host[:port]/path`, `ssh://[user@]host[:port]/path`, or scp-like
/// `user@host:path` (git's own short ssh form).
static HTTPS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^https://(?:[^@/\s]+@)?[A-Za-z0-9][A-Za-z0-9.-]*(?::[0-9]{1,5})?/[A-Za-z0-9._~%/+-]+$",
    )
    .unwrap()
});
static SSH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ssh://(?:[A-Za-z0-9._-]+@)?[A-Za-z0-9][A-Za-z0-9.-]*(?::[0-9]{1,5})?/[A-Za-z0-9._~%/+-]+$")
        .unwrap()
});
static SCP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9._-]+@[A-Za-z0-9][A-Za-z0-9.-]*:[A-Za-z0-9._~/+][A-Za-z0-9._~%/+-]*$")
        .unwrap()
});

/// Whether `url` is a remote repository URL Wraithgrid will clone.
pub fn valid_clone_url(url: &str) -> bool {
    (1..=CLONE_URL_MAX).contains(&url.len())
        && !url.starts_with('-')
        && (HTTPS.is_match(url) || SSH.is_match(url) || SCP.is_match(url))
        && !url.contains("::")
}

/// The folder a clone gets: the URL's last path part without `.git` (what `git clone` picks),
/// kept to `[A-Za-z0-9._-]`; `repo` when nothing usable is left.
pub fn repo_dir_name(url: &str) -> String {
    let path = url.trim_end_matches('/');
    let last = path.rsplit(['/', ':']).next().unwrap_or("");
    let last = last.strip_suffix(".git").unwrap_or(last);
    let name: String = last
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(100)
        .collect();
    let name = name.trim_start_matches(['.', '-']);
    if name.is_empty() {
        "repo".into()
    } else {
        name.into()
    }
}

/// Two remote URLs name the same repository (case of the host, a trailing `/` or `.git` aside).
fn same_remote(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim().trim_end_matches('/');
        s.strip_suffix(".git").unwrap_or(s).to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cloned {
    pub dir: PathBuf,
    /// The folder already held a clone of this repository, which is used as it is.
    pub existing: bool,
}

/// git's error, said for someone who just pasted a URL. All of stderr is read: ssh's own
/// line comes before git's `fatal:` one.
fn explain_clone_error(e: &GitError) -> String {
    let l = format!("{}\n{}", e.stderr, e.message).to_ascii_lowercase();
    if l.contains("could not read username") || l.contains("terminal prompts disabled") {
        "This repository needs a sign-in. Set up a git credential helper (or use its SSH URL) and try again.".into()
    } else if l.contains("host key verification failed") {
        "SSH doesn’t know this host yet. Connect to it once with ssh in a terminal, then try again."
            .into()
    } else if l.contains("permission denied (publickey") {
        "The SSH server refused your key. Check that it is added to your account there.".into()
    } else if l.contains("repository") && l.contains("not found") {
        "Repository not found. Check the URL, or that you have access to it.".into()
    } else {
        e.first_line()
    }
}

/// Clones `url` into `parent/<repo name>` (optionally on `branch`) and returns that folder.
/// A folder of that name holding a clone of the same repository is reused; any other is
/// refused, never overwritten.
pub fn clone_repo(
    run: &dyn GitRunner,
    parent: &Path,
    url: &str,
    branch: Option<&str>,
) -> Result<Cloned, String> {
    let url = url.trim();
    if !valid_clone_url(url) {
        return Err("Use an https:// or SSH address of a Git repository.".into());
    }
    if let Some(b) = branch {
        if !valid_branch_name(b) {
            return Err("Not a valid branch name.".into());
        }
    }
    if !parent.is_dir() {
        return Err(format!("Folder not found: {}", parent.display()));
    }
    let name = repo_dir_name(url);
    let dir = parent.join(&name);
    if dir.exists() {
        if dir.join(".git").exists() {
            // The URL as configured: `remote get-url` applies insteadOf rewrites (https → ssh).
            if let Ok(origin) = run.run(&["config", "--get", "remote.origin.url"], &dir, false) {
                if same_remote(&origin, url) {
                    return Ok(Cloned {
                        dir,
                        existing: true,
                    });
                }
            }
        }
        return Err(format!(
            "{name} already exists in {}. Pick another folder.",
            parent.display()
        ));
    }
    let d = dir.to_string_lossy();
    let mut args = vec!["clone"];
    if let Some(b) = branch {
        args.extend(["--branch", b]);
    }
    args.extend(["--", url, &d]);
    run.run(&args, parent, true)
        .map_err(|e| explain_clone_error(&e))?;
    Ok(Cloned {
        dir,
        existing: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn accepts_remote_https_and_ssh_urls_only() {
        for ok in [
            "https://github.com/cachewraith/wraithgrid.git",
            "https://github.com/cachewraith/wraithgrid",
            "https://gitlab.example.com:8443/group/sub/repo.git",
            "https://user:tok@github.com/o/r.git",
            "ssh://git@github.com/o/r.git",
            "ssh://git@host:2222/o/r",
            "git@github.com:o/r.git",
            "git@gitlab.com:group/sub/r",
        ] {
            assert!(valid_clone_url(ok), "{ok}");
        }
        for bad in [
            "",
            "-uhttps://x/y",
            "--upload-pack=touch /tmp/x",
            "file:///etc",
            "/home/me/repo",
            "../repo",
            "ext::sh -c touch% /tmp/pwned",
            "http://github.com/o/r",
            "https://github.com",
            "https://github.com/o/r two",
            "https://github.com/o/r\n",
            "git@github.com:-o/r",
            "ssh://-oProxyCommand=x/y",
            "fd::17/foo",
            "ssh://-oProxyCommand/y",
            "git@-oProxyCommand:o/r",
            "https://-h/o/r",
        ] {
            assert!(!valid_clone_url(bad), "{bad:?}");
        }
        assert!(!valid_clone_url(&format!(
            "https://h/{}",
            "a".repeat(CLONE_URL_MAX)
        )));
    }

    #[test]
    fn names_the_folder_like_git_clone() {
        assert_eq!(
            repo_dir_name("https://github.com/o/wraithgrid.git"),
            "wraithgrid"
        );
        assert_eq!(
            repo_dir_name("https://github.com/o/wraithgrid/"),
            "wraithgrid"
        );
        assert_eq!(repo_dir_name("git@github.com:o/my.repo.git"), "my.repo");
        assert_eq!(repo_dir_name("git@github.com:solo"), "solo");
        assert_eq!(repo_dir_name("https://h/o/%2e%2e"), "2e2e");
        assert_eq!(repo_dir_name("https://h/o/.git"), "repo");
        assert_eq!(repo_dir_name("https://h/o/..git"), "repo");
    }

    type Calls = RefCell<Vec<(Vec<String>, PathBuf)>>;

    fn recorder(calls: &Calls, answer: Result<String, GitError>) -> impl GitRunner + '_ {
        move |args: &[&str], cwd: &Path, _slow: bool| {
            calls.borrow_mut().push((
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_path_buf(),
            ));
            answer.clone()
        }
    }

    #[test]
    fn clones_after_a_double_dash_into_the_parent() {
        let t = tempfile::tempdir().unwrap();
        let tmp = t.path().to_path_buf();
        let calls = Calls::default();
        let run = recorder(&calls, Ok(String::new()));
        let r = clone_repo(&run, &tmp, " https://github.com/o/r.git ", Some("dev")).unwrap();
        assert_eq!(
            r,
            Cloned {
                dir: tmp.join("r"),
                existing: false
            }
        );
        let (args, cwd) = calls.borrow()[0].clone();
        let dir = tmp.join("r").to_string_lossy().into_owned();
        assert_eq!(
            args,
            [
                "clone",
                "--branch",
                "dev",
                "--",
                "https://github.com/o/r.git",
                dir.as_str()
            ]
        );
        assert_eq!(cwd, tmp);
    }

    #[test]
    fn refuses_bad_input_before_running_git() {
        let calls = Calls::default();
        let run = recorder(&calls, Ok(String::new()));
        let p = Path::new("/nowhere");
        assert!(clone_repo(&run, p, "file:///etc", None).is_err());
        assert!(clone_repo(&run, p, "https://h/o/r", Some("-x")).is_err());
        assert!(clone_repo(&run, p, "https://h/o/r", Some("a..b")).is_err());
        assert!(calls.borrow().is_empty());
        let e = clone_repo(&run, p, "https://h/o/r", None).unwrap_err();
        assert_eq!(e, "Folder not found: /nowhere");
    }

    #[test]
    fn reuses_a_clone_of_the_same_repo_and_refuses_any_other_folder() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("r/.git")).unwrap();
        std::fs::create_dir_all(tmp.path().join("other")).unwrap();
        let calls = Calls::default();
        let same = recorder(&calls, Ok("https://GitHub.com/o/r\n".into()));
        let r = clone_repo(&same, tmp.path(), "https://github.com/o/r.git", None).unwrap();
        assert!(r.existing);
        let diff = recorder(&calls, Ok("https://github.com/x/r\n".into()));
        let e = clone_repo(&diff, tmp.path(), "https://github.com/o/r.git", None).unwrap_err();
        assert!(e.starts_with("r already exists in"), "{e}");
        let e = clone_repo(&same, tmp.path(), "git@github.com:o/other.git", None).unwrap_err();
        assert!(e.starts_with("other already exists"), "{e}");
        // Neither ran a clone.
        assert!(calls.borrow().iter().all(|(a, _)| a[0] != "clone"));
    }

    #[test]
    fn explains_sign_in_and_ssh_failures() {
        let calls = Calls::default();
        let fail = |stderr: &str| GitError {
            message: "Command failed".into(),
            stderr: stderr.into(),
            ..GitError::default()
        };
        let t = tempfile::tempdir().unwrap();
        let p = t.path();
        let run = recorder(
            &calls,
            Err(fail("Cloning into 'r'...\nfatal: could not read Username for 'https://github.com': terminal prompts disabled\n")),
        );
        let e = clone_repo(&run, p, "https://github.com/o/r", None).unwrap_err();
        assert!(e.contains("needs a sign-in"), "{e}");
        let run = recorder(
            &calls,
            Err(fail(
                "Host key verification failed.\nfatal: Could not read from remote repository.",
            )),
        );
        let e = clone_repo(&run, p, "git@github.com:o/r", None).unwrap_err();
        assert!(e.contains("doesn’t know this host"), "{e}");
        let run = recorder(
            &calls,
            Err(fail(
                "fatal: Remote branch nope not found in upstream origin",
            )),
        );
        let e = clone_repo(&run, p, "git@github.com:o/r", Some("nope")).unwrap_err();
        assert_eq!(e, "Remote branch nope not found in upstream origin");
    }
}
