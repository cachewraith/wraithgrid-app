//! How git runs: the `GitRunner` seam every git function takes (so parsing and argument
//! building stay unit-testable with a fake), and `GitCli`, the real one (port of `runGit` in
//! `src/main/app.ts`): `std::process::Command`, never a shell, with a timeout and a cap on
//! the output it keeps.

use std::fmt;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// Runs `git <args>` in `cwd` and returns its stdout. `slow`: a commit (hooks) or push
/// (network) may take minutes, not the polling timeout.
pub trait GitRunner {
    fn run(&self, args: &[&str], cwd: &Path, slow: bool) -> Result<String, GitError>;
}

impl<F> GitRunner for F
where
    F: Fn(&[&str], &Path, bool) -> Result<String, GitError>,
{
    fn run(&self, args: &[&str], cwd: &Path, slow: bool) -> Result<String, GitError> {
        self(args, cwd, slow)
    }
}

/// A git run that failed. Past the runner's buffer `overflowed` is set and `stdout` is what
/// came first, so a big diff still shows that much (#140).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitError {
    pub message: String,
    pub stderr: String,
    pub stdout: String,
    pub overflowed: bool,
}

impl GitError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    /// The output git gave before it outran the runner's buffer, or None for any other failure.
    pub fn overflow_output(&self) -> Option<&str> {
        self.overflowed.then_some(self.stdout.as_str())
    }

    /// git's own error line (it may print progress lines first), without the "fatal: " prefix.
    pub fn first_line(&self) -> String {
        let text = if self.stderr.is_empty() {
            &self.message
        } else {
            &self.stderr
        };
        let lines: Vec<&str> = text
            .split('\n')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let line = lines
            .iter()
            .find(|l| l.starts_with("fatal: ") || l.starts_with("error: "))
            .or(lines.first())
            .copied()
            .unwrap_or("git failed");
        let line = line
            .strip_prefix("fatal: ")
            .or_else(|| line.strip_prefix("error: "))
            .unwrap_or(line);
        line.chars().take(300).collect()
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GitError {}

/// The real git. Optional locks off: status polling must never take index.lock while claude
/// is committing in the same repo. No prompts: a credential ask would hang. `env` is added
/// on top (tests point git at an empty config).
#[derive(Debug, Clone)]
pub struct GitCli {
    pub timeout: Duration,
    /// For `slow` runs: commits (hooks), pushes, pulls and checkouts.
    pub slow_timeout: Duration,
    /// Bytes of stdout kept; past it git is stopped and the run fails with what came first.
    pub max_buffer: usize,
    pub env: Vec<(String, String)>,
}

impl Default for GitCli {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            slow_timeout: Duration::from_secs(180),
            max_buffer: 8 * 1024 * 1024,
            env: Vec::new(),
        }
    }
}

/// Windows' CREATE_NO_WINDOW: no console flashes up for each git run.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

type Buf = Arc<Mutex<Vec<u8>>>;

impl GitRunner for GitCli {
    fn run(&self, args: &[&str], cwd: &Path, slow: bool) -> Result<String, GitError> {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| GitError::new(format!("spawn git: {e}")))?;

        let over = Arc::new(AtomicBool::new(false));
        let (done, finished) = mpsc::channel();
        let out: Buf = Arc::default();
        let err: Buf = Arc::default();
        read_into(
            child.stdout.take(),
            &out,
            self.max_buffer,
            Some(&over),
            &done,
        );
        read_into(child.stderr.take(), &err, self.max_buffer, None, &done);
        drop(done);

        let limit = if slow {
            self.slow_timeout
        } else {
            self.timeout
        };
        let deadline = Instant::now() + limit;
        let mut nap = Duration::from_millis(1);
        let status = loop {
            if let Ok(Some(status)) = child.try_wait() {
                break Some(status);
            }
            if over.load(Ordering::Relaxed) || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            thread::sleep(nap);
            nap = (nap * 2).min(Duration::from_millis(20));
        };
        // The readers end when the pipes close; a hook's leftover child may hold them open,
        // so wait no longer than the deadline (and a moment for output after a fast exit).
        for _ in 0..2 {
            let left = deadline.saturating_duration_since(Instant::now());
            if finished
                .recv_timeout(left.max(Duration::from_millis(100)))
                .is_err()
            {
                break;
            }
        }
        let stdout = take(&out);
        let stderr = take(&err);

        if over.load(Ordering::Relaxed) {
            return Err(GitError {
                message: "stdout maxBuffer length exceeded".into(),
                stderr,
                stdout,
                overflowed: true,
            });
        }
        let message = match status {
            Some(s) if s.success() => return Ok(stdout),
            Some(_) => format!("Command failed: git {}\n{stderr}", args.join(" ")),
            None => format!(
                "git {} timed out after {}s",
                args.first().unwrap_or(&""),
                limit.as_secs()
            ),
        };
        Err(GitError {
            message,
            stderr,
            stdout,
            overflowed: false,
        })
    }
}

/// Reads a pipe into `buf` on its own thread, keeping at most `max` bytes; past that it sets
/// `over` (when given) and stops reading, which closes the pipe.
fn read_into(
    pipe: Option<impl Read + Send + 'static>,
    buf: &Buf,
    max: usize,
    over: Option<&Arc<AtomicBool>>,
    done: &mpsc::Sender<()>,
) {
    let (buf, over, done) = (Arc::clone(buf), over.cloned(), done.clone());
    thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                let n = match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                let mut b = buf.lock().unwrap_or_else(PoisonError::into_inner);
                let room = max.saturating_sub(b.len());
                b.extend_from_slice(&chunk[..n.min(room)]);
                if n > room {
                    if let Some(over) = &over {
                        over.store(true, Ordering::Relaxed);
                    }
                    break;
                }
            }
        }
        let _ = done.send(());
    });
}

fn take(buf: &Buf) -> String {
    let b = buf.lock().unwrap_or_else(PoisonError::into_inner);
    String::from_utf8_lossy(&b).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(stderr: &str, message: &str) -> GitError {
        GitError {
            message: message.into(),
            stderr: stderr.into(),
            ..GitError::default()
        }
    }

    #[test]
    fn reports_the_fatal_or_error_line_after_progress_lines() {
        let e = err(
            "Cloning into 'x'...\nremote: Counting objects\nfatal: repository not found\n",
            "",
        );
        assert_eq!(e.first_line(), "repository not found");
        assert_eq!(
            err("  error: Your local changes would be overwritten\n", "").first_line(),
            "Your local changes would be overwritten"
        );
        // No stderr: the message; nothing at all: a fixed line.
        assert_eq!(
            err("", "not a git repository").first_line(),
            "not a git repository"
        );
        assert_eq!(err("", "").first_line(), "git failed");
        assert_eq!(err(&"x".repeat(400), "").first_line().len(), 300);
    }
}
