//! Reaching a remote machine's Wraithgrid (H5, `src/main/ssh.ts`): the system `ssh` runs
//! `wraithgrid host-stdio` (or the copied Rust host's bridge) there and its stdin/stdout carry
//! the host protocol. Keys or an agent only (BatchMode): the window cannot answer a password
//! prompt. `SshRunner` is the seam that starts processes, so tests never run a real ssh.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use wraithgrid_core::host::client::{Reader, Writer};

/// How much of ssh's stderr is kept: enough for its last line, which says why it failed.
const TAIL: usize = 600;

/// Host names from ~/.ssh/config, without patterns (`*`, `?`, `!`).
pub fn ssh_config_hosts(text: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some(rest) = line
            .get(..4)
            .filter(|k| k.eq_ignore_ascii_case("host"))
            .and_then(|_| line.get(4..))
            .filter(|r| r.starts_with(char::is_whitespace))
        else {
            continue;
        };
        for name in rest.split_whitespace() {
            let pattern = name.contains(['*', '?', '!']) || name.starts_with('-');
            if !pattern && !hosts.iter().any(|h| h == name) {
                hosts.push(name.to_string());
            }
        }
    }
    hosts
}

/// `--` ends ssh's options, so a host can never be read as one (the config refuses `-` too).
/// `command` runs there; None is `wraithgrid host-stdio`, else the Rust host's bridge when it
/// was copied over (remote_bin.rs).
pub fn ssh_args(ssh_host: &str, command: Option<&[String]>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "--",
        ssh_host,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    match command {
        Some(c) => args.extend(c.iter().cloned()),
        None => args.extend(["wraithgrid".to_string(), "host-stdio".to_string()]),
    }
    args
}

/// The ssh program: `WRAITHGRID_SSH` replaces it (tests' stand-in, docs/CONTEXT.md).
pub fn ssh_program(env: impl Fn(&str) -> Option<String>) -> String {
    env("WRAITHGRID_SSH")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "ssh".into())
}

#[derive(Default)]
struct TailState {
    text: String,
    done: bool,
}

/// The end of what ssh printed on stderr: why it failed, if it did. Filled by a reader
/// thread; cheap to clone.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<(Mutex<TailState>, Condvar)>);

impl StderrTail {
    /// A finished stderr with this text (fakes in tests).
    pub fn fixed(text: &str) -> StderrTail {
        let tail = StderrTail::default();
        tail.push(text);
        tail.finish();
        tail
    }

    /// Reads `stream` to its end on a thread, keeping the last 600 characters.
    pub fn read_from(mut stream: impl Read + Send + 'static) -> StderrTail {
        let tail = StderrTail::default();
        let filler = tail.clone();
        let spawned = std::thread::Builder::new()
            .name("ssh-stderr".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n @ 1..) = stream.read(&mut buf) {
                    filler.push(&String::from_utf8_lossy(&buf[..n]));
                }
                filler.finish();
            });
        if spawned.is_err() {
            tail.finish();
        }
        tail
    }

    fn push(&self, chunk: &str) {
        let mut s = self.0 .0.lock().unwrap();
        s.text.push_str(chunk);
        let n = s.text.chars().count();
        if n > TAIL {
            s.text = s.text.chars().skip(n - TAIL).collect();
        }
    }

    fn finish(&self) {
        self.0 .0.lock().unwrap().done = true;
        self.0 .1.notify_all();
    }

    /// What was kept, trimmed; waits up to `wait` for stderr to end first, since ssh's
    /// reason may land just after its stdout closed.
    pub fn text(&self, wait: Duration) -> String {
        let (lock, cvar) = &*self.0;
        let deadline = Instant::now() + wait;
        let mut s = lock.lock().unwrap();
        while !s.done {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            s = cvar.wait_timeout(s, left).unwrap().0;
        }
        s.text.trim().to_string()
    }

    /// The last line of what was kept.
    pub fn last_line(&self, wait: Duration) -> String {
        self.text(wait)
            .split('\n')
            .next_back()
            .unwrap_or("")
            .to_string()
    }
}

/// An ssh child joined into one stream: its stdout to read, its stdin to write. Dropping the
/// writer ends ssh.
pub struct SshLink {
    pub reader: Reader,
    pub writer: Writer,
    pub stderr: StderrTail,
}

/// One command run over ssh to its end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshRun {
    /// None when it was killed by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    /// The last 600 characters, trimmed.
    pub stderr: String,
}

/// Starts `program` (ssh, or a stand-in) with `args`.
pub trait SshRunner: Send + Sync {
    /// Starts it with all three stdio piped and joins them into a link.
    fn open(&self, program: &str, args: &[String]) -> std::io::Result<SshLink>;
    /// Runs it to its end, `input` on its stdin.
    fn run(&self, program: &str, args: &[String], input: Option<&[u8]>) -> std::io::Result<SshRun>;
}

/// The real runner: `std::process::Command`, never a shell, no console window on Windows.
pub struct SystemSsh;

fn command(program: &str, args: &[String]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// ssh's stdin; closing our side ends ssh too.
struct ChildWriter {
    stdin: Option<ChildStdin>,
    child: Child,
}

impl Write for ChildWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match &mut self.stdin {
            Some(s) => s.write(buf),
            None => Err(std::io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.stdin {
            Some(s) => s.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for ChildWriter {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl SshRunner for SystemSsh {
    fn open(&self, program: &str, args: &[String]) -> std::io::Result<SshLink> {
        let mut child = command(program, args).spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("ssh stdio is not piped"));
        };
        Ok(SshLink {
            reader: Box::new(stdout),
            writer: Box::new(ChildWriter {
                stdin: Some(stdin),
                child,
            }),
            stderr: StderrTail::read_from(stderr),
        })
    }

    fn run(&self, program: &str, args: &[String], input: Option<&[u8]>) -> std::io::Result<SshRun> {
        let mut child = command(program, args).spawn()?;
        let stdin = child.stdin.take();
        let input = input.map(<[u8]>::to_vec);
        // ssh may exit before reading it all; its code says why.
        let feeder = std::thread::spawn(move || {
            if let (Some(mut s), Some(bytes)) = (stdin, input) {
                let _ = s.write_all(&bytes);
            }
        });
        let stderr = child
            .stderr
            .take()
            .map(StderrTail::read_from)
            .unwrap_or_else(|| StderrTail::fixed(""));
        let mut out = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_end(&mut out);
        }
        let status = child.wait()?;
        let _ = feeder.join();
        Ok(SshRun {
            code: status.code(),
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: stderr.text(Duration::from_secs(5)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_override() {
        assert_eq!(ssh_program(|_| None), "ssh");
        assert_eq!(ssh_program(|_| Some(String::new())), "ssh");
        assert_eq!(
            ssh_program(|_| Some("/t/fake-ssh.sh".into())),
            "/t/fake-ssh.sh"
        );
    }

    #[test]
    fn stderr_keeps_the_last_line_of_its_tail() {
        let long = format!("{}\nPermission denied (publickey).\n", "x".repeat(900));
        let tail = StderrTail::fixed(&long);
        assert_eq!(tail.text(Duration::ZERO).chars().count(), TAIL - 1);
        assert_eq!(
            tail.last_line(Duration::ZERO),
            "Permission denied (publickey)."
        );
        assert_eq!(StderrTail::fixed("").last_line(Duration::ZERO), "");
    }

    #[cfg(unix)]
    #[test]
    fn system_runner_runs_a_stand_in() {
        // `sh -c` stands in for ssh: the runner only starts a program with arguments.
        let args = vec!["-c".to_string(), "cat; echo oops >&2; exit 3".to_string()];
        let r = SystemSsh.run("sh", &args, Some(b"hi")).unwrap();
        assert_eq!(
            r,
            SshRun {
                code: Some(3),
                stdout: "hi".into(),
                stderr: "oops".into()
            }
        );
        let mut link = SystemSsh
            .open(
                "sh",
                &["-c".to_string(), "head -n1; echo bye >&2".to_string()],
            )
            .unwrap();
        link.writer.write_all(b"ping\n").unwrap();
        let mut got = String::new();
        link.reader.read_to_string(&mut got).unwrap();
        assert_eq!(got, "ping\n");
        assert_eq!(link.stderr.last_line(Duration::from_secs(5)), "bye");
    }
}
