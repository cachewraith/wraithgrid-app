//! The GUI's side of the background pty host (`src/main/pane-host.ts`): finds or starts the
//! host, speaks its protocol, and offers create/attach/write/resize/kill. Blocking I/O with a
//! reader thread: callers run requests off the UI thread and get frames through a callback.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::protocol::*;

/// What the host tells the window, delivered from the reader thread.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    /// `replay` marks a reattached pane's screen, written before its live output.
    Data {
        pane_id: String,
        data: String,
        replay: bool,
    },
    /// `error` is set when the host went away under a pane rather than the pane exiting.
    Exit {
        pane_id: String,
        exit_code: i64,
        signal: Option<i64>,
        error: Option<String>,
    },
    /// A `wraithgrid new|split` command for the window; answer with `PaneHost::gui_reply`.
    Gui { id: u64, action: Value },
}

pub type Reader = Box<dyn Read + Send>;
pub type Writer = Box<dyn Write + Send>;

/// Connects to the host socket (a named pipe on Windows); fails when nothing listens.
pub fn connect_socket(path: &Path) -> std::io::Result<(Reader, Writer)> {
    #[cfg(unix)]
    {
        let stream = std::os::unix::net::UnixStream::connect(path)?;
        let reader = stream.try_clone()?;
        Ok((Box::new(reader), Box::new(stream)))
    }
    #[cfg(windows)]
    {
        windows_pipe::connect(path)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Welcome {
    pub protocol: i64,
    pub app_version: String,
    pub pid: i64,
}

type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<Reply>>>>;

/// One authenticated connection to a host.
pub struct HostConnection {
    writer: Mutex<Writer>,
    pending: Pending,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    pub welcome: Welcome,
}

impl HostConnection {
    /// Sends the hello; returns once the host welcomed this client. `on_frame` gets every
    /// later frame; `on_close` runs once when the link drops.
    pub fn open(
        (mut reader, mut writer): (Reader, Writer),
        token: &str,
        app_version: &str,
        role: &str,
        on_frame: Box<dyn Fn(HostFrame) + Send>,
        on_close: Box<dyn FnOnce() + Send>,
        timeout: Duration,
    ) -> Result<HostConnection, String> {
        let hello = ClientFrame::Hello {
            protocol: PROTOCOL_VERSION,
            app_version: app_version.into(),
            token: token.into(),
            role: role.into(),
        };
        writer
            .write_all(hello.encode().as_bytes())
            .map_err(|e| e.to_string())?;
        let pending: Pending = Arc::default();
        let closed = Arc::new(AtomicBool::new(false));
        let (welcome_tx, welcome_rx) = mpsc::channel::<Result<Welcome, String>>();
        {
            let (pending, closed) = (pending.clone(), closed.clone());
            std::thread::Builder::new()
                .name("host-reader".into())
                .spawn(move || {
                    let mut frames = FrameReader::default();
                    let mut buf = vec![0u8; 256 * 1024];
                    let mut welcomed = false;
                    'read: loop {
                        let n = match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        let mut lines = Vec::new();
                        if frames
                            .push(&buf[..n], |l| lines.push(l.to_string()))
                            .is_err()
                        {
                            break;
                        }
                        for line in lines {
                            let frame = parse_host_frame(&line);
                            if !welcomed {
                                let result = match frame {
                                    Some(HostFrame::Welcome {
                                        protocol,
                                        app_version,
                                        pid,
                                    }) => Ok(Welcome {
                                        protocol,
                                        app_version,
                                        pid,
                                    }),
                                    Some(HostFrame::Refused { error }) => Err(format!(
                                        "The background host refused the connection ({error})."
                                    )),
                                    _ => Err(
                                        "The background host refused the connection (bad reply)."
                                            .into(),
                                    ),
                                };
                                welcomed = result.is_ok();
                                let _ = welcome_tx.send(result);
                                if !welcomed {
                                    break 'read;
                                }
                                continue;
                            }
                            match frame {
                                Some(HostFrame::Reply { id, reply }) => {
                                    if let Some(done) = pending.lock().unwrap().remove(&id) {
                                        let _ = done.send(reply);
                                    }
                                }
                                Some(other) => on_frame(other),
                                None => {}
                            }
                        }
                    }
                    closed.store(true, Ordering::Release);
                    for (_, done) in pending.lock().unwrap().drain() {
                        let _ = done.send(Reply::closed());
                    }
                    if welcomed {
                        on_close();
                    } else {
                        let _ = welcome_tx
                            .send(Err("The background host closed the connection.".into()));
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        let welcome = match welcome_rx.recv_timeout(timeout) {
            Ok(result) => result?,
            Err(_) => return Err("The background host did not answer.".into()),
        };
        Ok(HostConnection {
            writer: Mutex::new(writer),
            pending,
            next_id: AtomicU64::new(1),
            closed,
            welcome,
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Sends a request built around its id and waits for the reply.
    pub fn request(&self, frame: impl FnOnce(u64) -> ClientFrame) -> Reply {
        if self.is_closed() {
            return Reply::closed();
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self
            .writer
            .lock()
            .unwrap()
            .write_all(frame(id).encode().as_bytes())
            .is_err()
        {
            self.pending.lock().unwrap().remove(&id);
            return Reply::closed();
        }
        rx.recv().unwrap_or_else(|_| Reply::closed())
    }

    pub fn send(&self, frame: &ClientFrame) {
        if !self.is_closed() {
            let _ = self
                .writer
                .lock()
                .unwrap()
                .write_all(frame.encode().as_bytes());
        }
    }

    pub fn close(&self) {
        let _ = self.writer.lock().unwrap().flush();
        self.closed.store(true, Ordering::Release);
    }
}

/// The user's answer when an older host still runs agents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MismatchChoice {
    Restart,
    Keep,
}

/// Asked with the older host's version and its live agent count.
pub type MismatchAsk = dyn Fn(&str, usize) -> MismatchChoice + Send + Sync;

pub struct PaneHostDeps {
    pub connect: Box<dyn Fn() -> std::io::Result<(Reader, Writer)> + Send + Sync>,
    /// Starts a detached host.
    pub start: Box<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub read_token: Box<dyn Fn() -> std::io::Result<String> + Send + Sync>,
    pub app_version: String,
    pub on_event: Arc<dyn Fn(HostEvent) + Send + Sync>,
    /// A host of another protocol has `live` panes; ask whether to restart them.
    pub on_mismatch: Box<MismatchAsk>,
    pub start_timeout: Duration,
    pub hello_timeout: Duration,
}

/// The window's view of the host: the panes it created or attached in this run.
pub struct PaneHost {
    deps: PaneHostDeps,
    conn: Mutex<Option<Arc<HostConnection>>>,
    /// Panes this run created or attached; told "exited" if the host goes away.
    mine: Arc<Mutex<HashSet<String>>>,
    cli_access: AtomicBool,
    /// Serializes connecting, so two panes starting at once don't start two hosts.
    opening: Mutex<()>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttachResult {
    pub pid: i64,
    pub exit: Option<PaneExit>,
}

fn live_count(reply: &Reply) -> usize {
    reply
        .0
        .get("panes")
        .and_then(Value::as_array)
        .map_or(0, |p| {
            p.iter()
                .filter(|x| x.get("exit").is_none_or(Value::is_null))
                .count()
        })
}

impl PaneHost {
    pub fn new(deps: PaneHostDeps) -> Arc<PaneHost> {
        Arc::new(PaneHost {
            deps,
            conn: Mutex::new(None),
            mine: Arc::default(),
            cli_access: AtomicBool::new(false),
            opening: Mutex::new(()),
        })
    }

    fn live(&self) -> Option<Arc<HostConnection>> {
        self.conn.lock().unwrap().clone().filter(|c| !c.is_closed())
    }

    /// Tells the host whether `wraithgrid` commands may connect (settings.cliAccess).
    pub fn set_cli_access(&self, on: bool) {
        self.cli_access.store(on, Ordering::Relaxed);
        if let Some(c) = self.live() {
            c.send(&ClientFrame::Config { cli_access: on });
        }
    }

    /// Starts or joins the host now, so `wraithgrid` commands work before any pane exists.
    pub fn connect(self: &Arc<Self>) -> Result<(), String> {
        self.connection().map(|_| ())
    }

    pub fn create(self: &Arc<Self>, req: SpawnRequest) -> Result<i64, String> {
        let conn = self.connection()?;
        let pane_id = req.pane_id.clone();
        let r = conn.request(|id| ClientFrame::Create { id, req });
        if !r.ok() {
            return Err(r.error());
        }
        self.mine.lock().unwrap().insert(pane_id);
        Ok(r.0.get("pid").and_then(Value::as_i64).unwrap_or(0))
    }

    /// Reattaches a pane the host still has; None when it has none by that id. The screen
    /// arrives as `replay` data events ahead of the reply.
    pub fn attach(self: &Arc<Self>, pane_id: &str, cols: u16, rows: u16) -> Option<AttachResult> {
        let conn = self.connection().ok()?;
        let r = conn.request(|id| ClientFrame::Attach {
            id,
            pane_id: pane_id.into(),
            cols,
            rows,
        });
        if !r.ok() || r.0.get("attached").is_none() {
            return None;
        }
        let exit: Option<PaneExit> =
            r.0.get("exit")
                .and_then(|e| serde_json::from_value(e.clone()).ok());
        if exit.is_none() {
            self.mine.lock().unwrap().insert(pane_id.into());
        }
        Some(AttachResult {
            pid: r.0.get("pid").and_then(Value::as_i64).unwrap_or(0),
            exit,
        })
    }

    pub fn write(&self, pane_id: &str, data: &str) {
        if let Some(c) = self.live() {
            c.send(&ClientFrame::Write {
                pane_id: pane_id.into(),
                data: data.into(),
            });
        }
    }

    pub fn resize(&self, pane_id: &str, cols: u16, rows: u16) {
        if let Some(c) = self.live() {
            c.send(&ClientFrame::Resize {
                pane_id: pane_id.into(),
                cols,
                rows,
            });
        }
    }

    pub fn kill(&self, pane_id: &str) {
        self.mine.lock().unwrap().remove(pane_id);
        if let Some(c) = self.live() {
            c.send(&ClientFrame::Kill {
                pane_id: pane_id.into(),
            });
        }
    }

    pub fn gui_reply(&self, id: u64, reply: Value) {
        if let Some(c) = self.live() {
            c.send(&ClientFrame::GuiReply { id, reply });
        }
    }

    /// Screen text of a pane: the visible rows, or the last `lines` with scrollback.
    pub fn read(self: &Arc<Self>, pane_id: &str, lines: Option<u32>) -> Result<String, String> {
        let conn = self.connection()?;
        let r = conn.request(|id| ClientFrame::Read {
            id,
            pane_id: pane_id.into(),
            lines,
        });
        if !r.ok() {
            return Err(r.error());
        }
        Ok(r.0
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// Every pane the host has (any client's) with its state; empty when no host runs.
    pub fn list(self: &Arc<Self>) -> Vec<PaneSummary> {
        let Some(c) = self.live() else { return vec![] };
        let r = c.request(|id| ClientFrame::List { id });
        r.0.get("panes")
            .and_then(|p| serde_json::from_value(p.clone()).ok())
            .unwrap_or_default()
    }

    /// Panes still running in the host (any client's); 0 when no host runs.
    pub fn live_count(self: &Arc<Self>) -> usize {
        match self.existing() {
            Some(c) => live_count(&c.request(|id| ClientFrame::List { id })),
            None => 0,
        }
    }

    /// Stops every pane and the host itself, if one runs.
    pub fn stop_all(self: &Arc<Self>) {
        if let Some(c) = self.existing() {
            self.mine.lock().unwrap().clear();
            c.request(|id| ClientFrame::Shutdown { id, exit: true });
        }
    }

    /// Leaves the host and its panes running (quit with "keep agents running").
    pub fn detach(&self) {
        self.mine.lock().unwrap().clear();
        if let Some(c) = self.conn.lock().unwrap().take() {
            c.close();
        }
    }

    /// Connects to a running host without starting one.
    fn existing(self: &Arc<Self>) -> Option<Arc<HostConnection>> {
        if let Some(c) = self.live() {
            return Some(c);
        }
        let _guard = self.opening.lock().unwrap();
        self.dial()
            .ok()
            .inspect(|c| *self.conn.lock().unwrap() = Some(c.clone()))
    }

    fn connection(self: &Arc<Self>) -> Result<Arc<HostConnection>, String> {
        if let Some(c) = self.live() {
            return Ok(c);
        }
        let _guard = self.opening.lock().unwrap();
        if let Some(c) = self.live() {
            return Ok(c);
        }
        let conn = self.open()?;
        *self.conn.lock().unwrap() = Some(conn.clone());
        Ok(conn)
    }

    fn open(self: &Arc<Self>) -> Result<Arc<HostConnection>, String> {
        match self.dial() {
            Ok(c) => return Ok(c),
            Err(DialError::Kept(msg)) => return Err(msg),
            // Nothing listens (or an older host was just stopped): start one below.
            Err(DialError::Other(_)) => {}
        }
        (self.deps.start)()?;
        let deadline = Instant::now() + self.deps.start_timeout;
        let mut last = "The background host did not start.".to_string();
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            match self.dial() {
                Ok(c) => return Ok(c),
                Err(DialError::Kept(msg)) => return Err(msg),
                Err(DialError::Other(msg)) => last = msg,
            }
        }
        Err(last)
    }

    fn dial(self: &Arc<Self>) -> Result<Arc<HostConnection>, DialError> {
        let stream = (self.deps.connect)().map_err(|e| DialError::Other(e.to_string()))?;
        let token = (self.deps.read_token)().map_err(|e| DialError::Other(e.to_string()))?;
        let weak = Arc::downgrade(self);
        let on_event = self.deps.on_event.clone();
        let on_frame = Box::new(move |frame: HostFrame| match frame {
            HostFrame::Data {
                pane_id,
                data,
                replay,
            } => on_event(HostEvent::Data {
                pane_id,
                data,
                replay,
            }),
            HostFrame::Exit {
                pane_id,
                exit_code,
                signal,
            } => {
                if let Some(host) = weak.upgrade() {
                    host.mine.lock().unwrap().remove(&pane_id);
                }
                on_event(HostEvent::Exit {
                    pane_id,
                    exit_code,
                    signal,
                    error: None,
                });
            }
            HostFrame::Gui { id, action } => on_event(HostEvent::Gui { id, action }),
            _ => {}
        });
        let (mine, on_event) = (self.mine.clone(), self.deps.on_event.clone());
        let on_close = Box::new(move || {
            // The host died under us: every pane it ran for this window is gone.
            let lost: Vec<String> = mine.lock().unwrap().drain().collect();
            for pane_id in lost {
                on_event(HostEvent::Exit {
                    pane_id,
                    exit_code: -1,
                    signal: None,
                    error: Some("The background host stopped.".into()),
                });
            }
        });
        let conn = HostConnection::open(
            stream,
            token.trim(),
            &self.deps.app_version,
            "gui",
            on_frame,
            on_close,
            self.deps.hello_timeout,
        )
        .map_err(DialError::Other)?;
        if conn.welcome.protocol != PROTOCOL_VERSION {
            return Err(self.mismatch(conn));
        }
        conn.send(&ClientFrame::Config {
            cli_access: self.cli_access.load(Ordering::Relaxed),
        });
        Ok(Arc::new(conn))
    }

    fn mismatch(&self, conn: HostConnection) -> DialError {
        // `list` and `shutdown` keep their shape across versions (protocol.ts).
        let live = live_count(&conn.request(|id| ClientFrame::List { id }));
        let choice = if live == 0 {
            MismatchChoice::Restart
        } else {
            (self.deps.on_mismatch)(&conn.welcome.app_version, live)
        };
        if choice == MismatchChoice::Keep {
            conn.close();
            return DialError::Kept(format!(
                "Agents are running in an older background host (Wraithgrid {}). Quit and stop them to start panes here.",
                conn.welcome.app_version
            ));
        }
        conn.request(|id| ClientFrame::Shutdown { id, exit: true });
        // open() starts a new one.
        DialError::Other("Stopped an older background host.".into())
    }
}

enum DialError {
    /// The user chose to leave an older host's agents running; no host is started over it.
    Kept(String),
    Other(String),
}

#[cfg(windows)]
mod windows_pipe {
    //! A named pipe opened for both reading and writing from two threads: a synchronous
    //! handle would serialize a pending read with every write, so a small tokio runtime owns
    //! the pipe and bridges it to blocking Read/Write halves.
    use std::io::{Read, Write};
    use std::path::Path;
    use std::sync::mpsc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub struct PipeReader {
        rx: mpsc::Receiver<Vec<u8>>,
        left: Vec<u8>,
    }

    impl Read for PipeReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.left.is_empty() {
                match self.rx.recv() {
                    Ok(chunk) => self.left = chunk,
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(self.left.len());
            buf[..n].copy_from_slice(&self.left[..n]);
            self.left.drain(..n);
            Ok(n)
        }
    }

    pub struct PipeWriter(tokio::sync::mpsc::UnboundedSender<Vec<u8>>);

    impl Write for PipeWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .send(buf.to_vec())
                .map_err(|_| std::io::Error::other("pipe closed"))?;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    pub fn connect(path: &Path) -> std::io::Result<(super::Reader, super::Writer)> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()?;
        let client = rt
            .block_on(async { tokio::net::windows::named_pipe::ClientOptions::new().open(path) })?;
        let (read_tx, read_rx) = mpsc::channel();
        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("host-pipe".into())
            .spawn(move || {
                rt.block_on(async move {
                    let (mut r, mut w) = tokio::io::split(client);
                    let reader = async {
                        let mut buf = vec![0u8; 256 * 1024];
                        loop {
                            match r.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    if read_tx.send(buf[..n].to_vec()).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    };
                    let writer = async {
                        while let Some(chunk) = write_rx.recv().await {
                            if w.write_all(&chunk).await.is_err() {
                                break;
                            }
                        }
                    };
                    tokio::select! { _ = reader => {}, _ = writer => {} }
                });
            })?;
        Ok((
            Box::new(PipeReader {
                rx: read_rx,
                left: Vec::new(),
            }),
            Box::new(PipeWriter(write_tx)),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::os::unix::net::UnixStream;

    /// A fake host on one end of a socket pair: welcomes, then answers by request type.
    fn fake_host(protocol: i64) -> (Reader, Writer) {
        let (client, server) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let mut out = server.try_clone().unwrap();
            let lines = std::io::BufReader::new(server).lines();
            for line in lines.map_while(Result::ok) {
                let v: Value = serde_json::from_str(&line).unwrap();
                let reply = match v["t"].as_str().unwrap() {
                    "hello" => {
                        assert_eq!(v["token"], "tok");
                        format!("{{\"t\":\"welcome\",\"protocol\":{protocol},\"appVersion\":\"9.9.9\",\"pid\":42}}")
                    }
                    "attach" => format!(
                        "{{\"t\":\"data\",\"paneId\":\"{p}\",\"data\":\"screen\",\"replay\":true}}\n{{\"t\":\"reply\",\"id\":{id},\"reply\":{{\"ok\":true,\"pid\":7,\"attached\":true,\"exit\":null}}}}",
                        p = v["paneId"].as_str().unwrap(),
                        id = v["id"]
                    ),
                    "list" => format!("{{\"t\":\"reply\",\"id\":{},\"reply\":{{\"ok\":true,\"panes\":[]}}}}", v["id"]),
                    "write" => format!("{{\"t\":\"data\",\"paneId\":\"{}\",\"data\":\"echo:{}\"}}", v["paneId"].as_str().unwrap(), v["data"].as_str().unwrap()),
                    "shutdown" => break,
                    _ => continue,
                };
                writeln!(out, "{reply}").unwrap();
            }
        });
        let reader = client.try_clone().unwrap();
        (Box::new(reader), Box::new(client))
    }

    fn host_with(protocol: i64, events: Arc<Mutex<Vec<HostEvent>>>) -> Arc<PaneHost> {
        PaneHost::new(PaneHostDeps {
            connect: Box::new(move || Ok(fake_host(protocol))),
            start: Box::new(|| Ok(())),
            read_token: Box::new(|| Ok("tok\n".into())),
            app_version: "2.0.0".into(),
            on_event: Arc::new(move |e| events.lock().unwrap().push(e)),
            on_mismatch: Box::new(|_, _| MismatchChoice::Keep),
            start_timeout: Duration::from_millis(300),
            hello_timeout: Duration::from_secs(2),
        })
    }

    #[test]
    fn attach_replays_then_streams() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let host = host_with(1, events.clone());
        let r = host.attach("p1", 80, 24).unwrap();
        assert_eq!(r, AttachResult { pid: 7, exit: None });
        host.write("p1", "ls");
        std::thread::sleep(Duration::from_millis(100));
        let got = events.lock().unwrap().clone();
        assert_eq!(
            got[0],
            HostEvent::Data {
                pane_id: "p1".into(),
                data: "screen".into(),
                replay: true
            }
        );
        assert_eq!(
            got[1],
            HostEvent::Data {
                pane_id: "p1".into(),
                data: "echo:ls".into(),
                replay: false
            }
        );
        assert_eq!(host.live_count(), 0);
    }

    #[test]
    fn host_going_away_exits_my_panes() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let host = host_with(1, events.clone());
        host.attach("p1", 80, 24).unwrap();
        host.stop_all();
        host.attach("p2", 80, 24);
        std::thread::sleep(Duration::from_millis(100));
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .all(|e| !matches!(e, HostEvent::Exit { pane_id, .. } if pane_id == "p1")));
    }

    #[test]
    fn older_host_with_no_agents_is_restarted() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let host = host_with(0, events);
        // Every dial meets protocol 0 with no live panes: it is told to stop, then the start
        // wait times out, so the last error is reported.
        assert_eq!(
            host.connect().unwrap_err(),
            "Stopped an older background host."
        );
    }
}
