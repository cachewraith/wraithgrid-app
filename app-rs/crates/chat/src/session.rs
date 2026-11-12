//! Runs a chat pane's CLI process (ADR 0002), ported from `src/main/chat/sessions.ts` and
//! `spawn.ts`: one headless process per pane, fed JSON lines on stdin, its stdout decoded by
//! the CLI's adapter into ChatEvents for the view. A process belongs to its [`ChatSession`]
//! handle and stops with it, so they end when Wraithgrid quits; the next start resumes the
//! conversation by its session id.
//!
//! Threads: one reads stdout (line in, events out), one keeps stderr's tail, one waits for the
//! exit; timers (Stop's wait, the stop grace) are short-lived threads. Events go to the sink
//! in order, under the session's lock: a callback sink must not call back into the session.
//! The turn bookkeeping is [`TurnState`], pure and tested without a process.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::adapter::{ArgsOptions, ChatAdapter};
use crate::types::{
    is_valid_model, is_valid_session_id, with_attachments, ApprovalDecision, ChatAnswers,
    ChatEvent, ChatImage, ChatMode, CHAT_EFFORTS, CHAT_FILES_MAX, CHAT_IMAGES_MAX, CHAT_IMAGE_MAX,
    CHAT_TEXT_MAX,
};

/// A line this long is not a protocol line: dropped rather than grown without end.
const LINE_MAX: usize = 16_000_000;
const STDERR_KEEP: usize = 2000;
/// How long the exit report waits for the last output after the process ended.
const DRAIN_WAIT: Duration = Duration::from_millis(500);
const EXIT_POLL: Duration = Duration::from_millis(20);
const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(30);
const LIST_MODELS_MAX: usize = 1 << 20;

/// How to start a chat: the CLI, where, as whom, and which conversation.
#[derive(Debug, Clone)]
pub struct ChatLaunch {
    /// The CLI binary (resolved by the app: PATH, the user's override).
    pub program: PathBuf,
    /// The pane's own args; the adapter's (`-p --input-format stream-json …`) follow them.
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The whole environment the CLI sees ([`ChatLaunch::new`] starts from this process's).
    pub env: BTreeMap<String, String>,
    /// The account's config dir, set as the CLI's own variable (claude: CLAUDE_CONFIG_DIR).
    pub config_dir: Option<PathBuf>,
    /// The conversation to resume (or the folder's last), model, effort and mode.
    pub options: ArgsOptions,
    /// How long Stop waits for the CLI to end the turn before the chat is marked idle anyway.
    pub interrupt_wait: Duration,
    /// How long a stop waits after SIGTERM before it kills the process tree.
    pub stop_grace: Duration,
}

impl ChatLaunch {
    pub fn new(program: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        ChatLaunch {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: std::env::vars().collect(),
            config_dir: None,
            options: ArgsOptions::default(),
            interrupt_wait: Duration::from_millis(3000),
            stop_grace: Duration::from_millis(2000),
        }
    }

    /// The full argument list for `adapter` (the pane's args, then the adapter's).
    pub fn command_args(&self, adapter: &mut dyn ChatAdapter) -> Vec<String> {
        let mut args = self.args.clone();
        args.extend(adapter.args(&self.options));
        args
    }
}

/// Why a chat did not start.
#[derive(Debug)]
pub enum StartError {
    /// A resume id, model or effort that is not a plain token / known value.
    Invalid(&'static str),
    /// The process could not be started (not found, not executable, …).
    Spawn(io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Invalid(what) => write!(f, "Invalid request: {what}"),
            StartError::Spawn(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StartError {}

/// The checks the IPC schema made on a start (`chatStartArgs`).
fn validate(o: &ArgsOptions) -> Result<(), StartError> {
    if o.session_id
        .as_deref()
        .is_some_and(|s| !is_valid_session_id(s))
    {
        return Err(StartError::Invalid("not a conversation id"));
    }
    if o.model.as_deref().is_some_and(|m| !is_valid_model(m)) {
        return Err(StartError::Invalid("not a model name"));
    }
    if o.effort
        .as_deref()
        .is_some_and(|e| !CHAT_EFFORTS.contains(&e))
    {
        return Err(StartError::Invalid("not an effort level"));
    }
    Ok(())
}

/// A turn's bookkeeping: which of the adapter's events reach the view. Turns sent and not
/// finished are counted, since a message sent while one runs is queued by the CLI.
#[derive(Debug, Default, Clone)]
pub struct TurnState {
    turns: u32,
    /// Stop was asked: the next turn end clears the queue too.
    interrupted: bool,
    /// The turn ended in an error (a usage limit…): the CLI runs none of the queued ones.
    failed: bool,
    /// Background work was running when last heard of: it dies with the process.
    background: bool,
    /// The Stop wait running (by generation), ended early by a turn end.
    stop_wait: Option<u64>,
    stop_gen: u64,
}

impl TurnState {
    /// Mid-turn or with background work: what a quit would cut short (#137).
    pub fn is_busy(&self) -> bool {
        self.turns > 0 || self.background
    }

    /// A message was sent: the event to show.
    pub fn sent(&mut self) -> ChatEvent {
        self.turns += 1;
        ChatEvent::Busy { busy: true }
    }

    /// The adapter's events for one line, less the ones that would mislead the view.
    pub fn filter(&mut self, events: Vec<ChatEvent>) -> Vec<ChatEvent> {
        let mut out = Vec::with_capacity(events.len());
        for e in events {
            match &e {
                ChatEvent::Background { tasks } => self.background = !tasks.is_empty(),
                ChatEvent::Busy { busy: true } => {
                    // A turn the CLI started on its own (background work ended, #39); one
                    // already running covers it.
                    if self.turns > 0 {
                        continue;
                    }
                    self.turns = 1;
                }
                ChatEvent::TurnEnd { error: Some(_), .. } => self.failed = true,
                ChatEvent::Busy { busy: false } => {
                    // An earlier turn ending must not mark the chat idle while a queued one
                    // runs. claude also folds queued messages into one turn, and a failed turn
                    // (a usage limit) ends them all: either way nothing is left waiting.
                    self.turns = if self.interrupted || self.failed {
                        0
                    } else {
                        self.turns.saturating_sub(1)
                    };
                    self.interrupted = false;
                    self.failed = false;
                    self.stop_wait = None;
                    if self.turns > 0 {
                        continue;
                    }
                }
                _ => {}
            }
            out.push(e);
        }
        out
    }

    /// Stop was asked: returns the generation to hand to [`TurnState::stop_waited`] later.
    pub fn interrupt(&mut self) -> u64 {
        self.interrupted = true;
        self.stop_gen += 1;
        self.stop_wait = Some(self.stop_gen);
        self.stop_gen
    }

    /// The Stop wait `gen` ran out. claude answers a Stop with the turn's end; with no turn
    /// running there it says nothing, and the chat would wait for ever. Then it is idle: the
    /// event to show, if any.
    pub fn stop_waited(&mut self, gen: u64) -> Option<ChatEvent> {
        if self.stop_wait != Some(gen) {
            return None;
        }
        self.stop_wait = None;
        if self.turns == 0 {
            return None;
        }
        self.turns = 0;
        self.interrupted = false;
        Some(ChatEvent::Busy { busy: false })
    }
}

/// Where a session's events go.
type Sink = Box<dyn Fn(ChatEvent) + Send + Sync>;

struct State {
    adapter: Box<dyn ChatAdapter>,
    stdin: Option<ChildStdin>,
    turns: TurnState,
    /// Set once stop() was asked, so the exit is not reported as a failure.
    stopping: bool,
    exited: bool,
    stderr: String,
}

struct Shared {
    state: Mutex<State>,
    exit: Condvar,
    child: Mutex<Child>,
    sink: Sink,
    pid: u32,
    interrupt_wait: Duration,
    stop_grace: Duration,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn write_line(s: &mut State, msg: &Value) {
    if let Some(stdin) = s.stdin.as_mut() {
        // A write after the process died must not fail the caller.
        let _ = writeln!(stdin, "{msg}").and_then(|_| stdin.flush());
    }
}

/// One running chat process. Dropping it stops the process.
pub struct ChatSession {
    shared: Arc<Shared>,
}

impl ChatSession {
    /// Starts the CLI; its events arrive on the returned receiver.
    pub fn start(
        launch: ChatLaunch,
        adapter: Box<dyn ChatAdapter>,
    ) -> Result<(ChatSession, Receiver<ChatEvent>), StartError> {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let session = Self::start_with(launch, adapter, move |e| {
            let _ = tx.lock().unwrap_or_else(PoisonError::into_inner).send(e);
        })?;
        Ok((session, rx))
    }

    /// Starts the CLI; each event is handed to `sink` (called on the session's threads, in
    /// order, under its lock: it must not call back into this session).
    pub fn start_with(
        launch: ChatLaunch,
        mut adapter: Box<dyn ChatAdapter>,
        sink: impl Fn(ChatEvent) + Send + Sync + 'static,
    ) -> Result<ChatSession, StartError> {
        validate(&launch.options)?;
        let args = launch.command_args(&mut *adapter);
        let mut env = launch.env.clone();
        if let (Some(dir), Some(var)) = (&launch.config_dir, adapter.config_dir_var()) {
            env.insert(var.to_string(), dir.to_string_lossy().into_owned());
        }
        let mut cmd = Command::new(&launch.program);
        cmd.args(&args)
            .current_dir(&launch.cwd)
            .env_clear()
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // std opens every fd close-on-exec, so nothing past stdio leaks into the CLI (the TS
        // masked Chromium's fds by hand). On POSIX the CLI leads its own process group, so a
        // stop also ends the tools it started.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
        let mut child = cmd.spawn().map_err(StartError::Spawn)?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let hello = adapter.hello();
        let list_models = adapter.list_models();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                adapter,
                stdin,
                turns: TurnState::default(),
                stopping: false,
                exited: false,
                stderr: String::new(),
            }),
            exit: Condvar::new(),
            child: Mutex::new(child),
            sink: Box::new(sink),
            pid,
            interrupt_wait: launch.interrupt_wait,
            stop_grace: launch.stop_grace,
        });
        {
            let mut s = shared.lock();
            for line in &hello {
                write_line(&mut s, line);
            }
        }

        let (out_done_tx, out_done) = mpsc::channel::<()>();
        if let Some(stdout) = stdout {
            let sh = shared.clone();
            thread::spawn(move || {
                read_lines(stdout, |line| handle_line(&sh, line));
                let _ = out_done_tx.send(());
            });
        }
        let (err_done_tx, err_done) = mpsc::channel::<()>();
        if let Some(mut stderr) = stderr {
            let sh = shared.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut s = sh.lock();
                    s.stderr.push_str(&String::from_utf8_lossy(&buf[..n]));
                    let len = s.stderr.chars().count();
                    if len > STDERR_KEEP {
                        let cut = s
                            .stderr
                            .char_indices()
                            .nth(len - STDERR_KEEP)
                            .map_or(0, |c| c.0);
                        s.stderr.drain(..cut);
                    }
                }
                let _ = err_done_tx.send(());
            });
        }
        let sh = shared.clone();
        thread::spawn(move || {
            let status = loop {
                match sh
                    .child
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .try_wait()
                {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => {}
                    Err(_) => break None,
                }
                thread::sleep(EXIT_POLL);
            };
            // The last lines and stderr come in after the exit; a tool the CLI left running
            // may hold the pipes open, so the wait is short.
            let _ = out_done.recv_timeout(DRAIN_WAIT);
            let _ = err_done.recv_timeout(DRAIN_WAIT);
            ended(&sh, status);
        });

        if let Some(list) = list_models {
            let sh = shared.clone();
            let (program, cwd) = (launch.program.clone(), launch.cwd.clone());
            thread::spawn(move || {
                let out = run_capped(&program, &list.args, &cwd, &env);
                let models = out.map(|o| (list.parse)(&o)).unwrap_or_default();
                if !models.is_empty() {
                    announce(
                        &sh,
                        ChatEvent::Capabilities {
                            models,
                            account: None,
                            commands: Some(Vec::new()),
                        },
                    );
                }
            });
        }
        Ok(ChatSession { shared })
    }

    /// The process id.
    pub fn pid(&self) -> u32 {
        self.shared.pid
    }

    /// The process runs and no stop was asked.
    pub fn is_running(&self) -> bool {
        let s = self.shared.lock();
        !s.exited && !s.stopping
    }

    /// Mid-turn or with background work: what a quit would cut short (#137).
    pub fn is_busy(&self) -> bool {
        let s = self.shared.lock();
        !s.exited && !s.stopping && s.turns.is_busy()
    }

    /// Sends a message with its pasted images and attached file paths; false when the
    /// session ended or the message is past the limits.
    pub fn send(&self, text: &str, images: &[ChatImage], files: &[String]) -> bool {
        let too_big = text.chars().count() > CHAT_TEXT_MAX
            || images.len() > CHAT_IMAGES_MAX
            || files.len() > CHAT_FILES_MAX
            || images.iter().any(|i| {
                i.data.len() > CHAT_IMAGE_MAX
                    || !i
                        .data
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
            });
        let mut s = self.shared.lock();
        if too_big || s.exited || s.stopping {
            return false;
        }
        if let Some(line) = s.adapter.user(&with_attachments(text, files), images) {
            write_line(&mut s, &line);
        }
        let e = s.turns.sent();
        (self.shared.sink)(e);
        true
    }

    /// Answers an approval; false when it is not pending (any more).
    pub fn approve(
        &self,
        req_id: &str,
        decision: ApprovalDecision,
        answers: Option<ChatAnswers>,
    ) -> bool {
        let mut s = self.shared.lock();
        if s.exited || s.stopping {
            return false;
        }
        let Some(line) = s.adapter.answer(req_id, decision, answers.as_ref()) else {
            return false;
        };
        write_line(&mut s, &line);
        (self.shared.sink)(ChatEvent::ApprovalDone {
            req_id: req_id.to_string(),
            decision,
            answers,
        });
        true
    }

    /// Switches the running session's model; false when the CLI can't (restart it instead).
    pub fn set_model(&self, model: &str) -> bool {
        if !is_valid_model(model) {
            return false;
        }
        self.write_with(|a| a.set_model(model))
    }

    /// Switches the permission mode live; false when the CLI can't.
    pub fn set_mode(&self, mode: ChatMode) -> bool {
        self.write_with(|a| a.set_mode(mode))
    }

    fn write_with(&self, f: impl FnOnce(&mut dyn ChatAdapter) -> Option<Value>) -> bool {
        let mut s = self.shared.lock();
        if s.exited || s.stopping {
            return false;
        }
        let Some(line) = f(&mut *s.adapter) else {
            return false;
        };
        write_line(&mut s, &line);
        true
    }

    /// An event about the chat from outside its pipe (its CLI's model list).
    pub fn announce(&self, e: ChatEvent) {
        announce(&self.shared, e);
    }

    /// Stops the running turn, over the pipe (not a signal). With no answer within
    /// `interrupt_wait`, the chat is marked idle.
    pub fn interrupt(&self) {
        let gen = {
            let mut s = self.shared.lock();
            if s.exited || s.stopping {
                return;
            }
            let gen = s.turns.interrupt();
            if let Some(line) = s.adapter.interrupt() {
                write_line(&mut s, &line);
            }
            gen
        };
        let sh = self.shared.clone();
        thread::spawn(move || {
            thread::sleep(sh.interrupt_wait);
            let mut s = sh.lock();
            if s.exited || s.stopping {
                return;
            }
            if let Some(e) = s.turns.stop_waited(gen) {
                (sh.sink)(e);
            }
        });
    }

    /// Stops the process: closes stdin, then SIGTERM, then a forced kill of the whole tree
    /// after the grace time. Quiet: no exit event follows (only `background` emptied when
    /// work was running). Returns at once; see [`ChatSession::wait_exit`].
    pub fn stop(&self) {
        {
            let mut s = self.shared.lock();
            if s.stopping || s.exited {
                return;
            }
            s.stopping = true;
            s.stdin = None;
        }
        signal(&self.shared, false);
        let sh = self.shared.clone();
        thread::spawn(move || {
            if !wait_exit(&sh, sh.stop_grace) {
                signal(&sh, true);
            }
        });
    }

    /// Waits up to `timeout` for the process to end; true when it has.
    pub fn wait_exit(&self, timeout: Duration) -> bool {
        wait_exit(&self.shared, timeout)
    }
}

impl Drop for ChatSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn announce(sh: &Shared, e: ChatEvent) {
    let s = sh.lock();
    if !s.exited && !s.stopping {
        (sh.sink)(e);
    }
}

fn wait_exit(sh: &Shared, timeout: Duration) -> bool {
    let until = Instant::now() + timeout;
    let mut s = sh.lock();
    while !s.exited {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        s = sh
            .exit
            .wait_timeout(s, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    true
}

/// SIGTERM (or SIGKILL when `force`) to the CLI's process group, else the CLI alone.
/// Windows: ends the process, and with `force` its whole tree (`taskkill /T /F`).
fn signal(sh: &Shared, force: bool) {
    if sh.lock().exited {
        return;
    }
    #[cfg(unix)]
    {
        let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
        let pid = sh.pid as libc::pid_t;
        // SAFETY: kill(2) with a pid we started; it only sends a signal.
        let group = unsafe { libc::kill(-pid, sig) } == 0;
        if !group {
            // The group is gone; try the leader alone.
            // SAFETY: as above.
            unsafe { libc::kill(pid, sig) };
        }
    }
    #[cfg(not(unix))]
    {
        if force {
            let _ = Command::new("taskkill")
                .args(["/PID", &sh.pid.to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = sh
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .kill();
    }
}

/// Splits a stream into lines (no trailing `\r\n`), skipping any line past LINE_MAX.
fn read_lines(stream: impl Read, mut each: impl FnMut(&str)) {
    let mut r = BufReader::new(stream);
    let mut buf: Vec<u8> = Vec::new();
    let mut skipping = false;
    loop {
        let (used, newline) = match r.fill_buf() {
            Ok([]) | Err(_) => break,
            Ok(chunk) => match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    if !skipping {
                        buf.extend_from_slice(&chunk[..i]);
                    }
                    (i + 1, true)
                }
                None => {
                    if !skipping {
                        buf.extend_from_slice(chunk);
                    }
                    (chunk.len(), false)
                }
            },
        };
        r.consume(used);
        if !newline && buf.len() > LINE_MAX {
            buf.clear();
            skipping = true;
        }
        if newline {
            if !skipping {
                each(&String::from_utf8_lossy(&buf));
            }
            buf.clear();
            skipping = false;
        }
    }
    if !skipping && !buf.is_empty() {
        each(&String::from_utf8_lossy(&buf));
    }
}

fn handle_line(sh: &Shared, line: &str) {
    let mut s = sh.lock();
    // A stopped process still talks until it exits: not into the view.
    if s.stopping {
        return;
    }
    let decoded = s.adapter.decode_line(line);
    for r in &decoded.replies {
        write_line(&mut s, r);
    }
    for e in s.turns.filter(decoded.events) {
        (sh.sink)(e);
    }
}

/// The signal numbers the view knows by name (`SIGNALS` in the TS).
#[cfg(unix)]
fn signal_number(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status
        .signal()
        .filter(|s| [libc::SIGINT, libc::SIGKILL, libc::SIGTERM].contains(s))
}

#[cfg(not(unix))]
fn signal_number(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

fn ended(sh: &Shared, status: Option<std::process::ExitStatus>) {
    let mut s = sh.lock();
    if s.exited {
        return;
    }
    s.exited = true;
    s.stdin = None;
    sh.exit.notify_all();
    if s.turns.background {
        (sh.sink)(ChatEvent::Background { tasks: Vec::new() });
    }
    // A stop the app asked for (restart, view switch, close): a new process may already run
    // for the pane, so this one's exit is not news.
    if s.stopping {
        return;
    }
    let code = status.and_then(|st| st.code());
    let failed = code != Some(0);
    let last_err = s
        .stderr
        .trim()
        .split('\n')
        .rfind(|l| !l.is_empty())
        .map(String::from);
    (sh.sink)(ChatEvent::Busy { busy: false });
    (sh.sink)(ChatEvent::Exit {
        code,
        signal: status.as_ref().and_then(signal_number),
        error: if failed { last_err } else { None },
    });
}

/// Runs a short command (a model list) with the chat's env and folder: its stdout, or None
/// when it failed, ran past 30 s or printed more than 1 MB.
fn run_capped(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout
            .by_ref()
            .take(LIST_MODELS_MAX as u64 + 1)
            .read_to_end(&mut out);
        out
    });
    let until = Instant::now() + LIST_MODELS_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() < until => thread::sleep(EXIT_POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?;
    (status.success() && out.len() <= LIST_MODELS_MAX)
        .then(|| String::from_utf8_lossy(&out).into_owned())
}
