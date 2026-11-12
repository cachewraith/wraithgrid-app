//! A machine's host (`remoteHost(m)` in `src/main/app.ts`): core's `PaneHost` over
//! `ssh … wraithgrid host-stdio` (H5), or over the Rust host's bridge copied there (ADR 0004
//! phase 5). Core's client has no `spawn`/`plan` request, no "lost" text for a remote link and
//! no probe, so this wraps the ssh stream: the writer turns a marked `create` into the `spawn`
//! or `plan` frame, the reader notes the welcome's version, and exits are reworded.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use wraithgrid_core::config::Machine;
use wraithgrid_core::host::client::{
    HostEvent, MismatchAsk, PaneHost, PaneHostDeps, Reader, Writer,
};
use wraithgrid_core::host::protocol::{parse_host_frame, HostFrame, SpawnRequest};
use wraithgrid_core::spawn::SpawnSpec;

use crate::plan::remote_plan;
use crate::remote_bin::{prepare_remote, PrepareDeps};
use crate::spec::{parse_spawn_spec, spawn_spec_json};
use crate::ssh::{ssh_args, SshRunner, StderrTail};

/// What core's `PaneHost` says when its host goes away under a pane.
pub const LOCAL_LOST: &str = "The background host stopped.";

/// Stands in for the file of a `create` that is really a `spawn`: no real path holds a NUL.
const SPAWN_MARK: &str = "\0wraithgrid-remote-spawn";

/// What a pane of this machine says when the link drops; its process may still run there.
pub fn lost_message(m: &Machine) -> String {
    format!(
        "Disconnected from {}. Restart to reconnect; the agent may still be running there.",
        m.name
    )
}

/// This build's static host for a target, or None.
pub type BinaryFor = dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync;

pub struct RemoteHostDeps {
    pub machine: Machine,
    pub app_version: String,
    /// `ssh_program(env)`: ssh, or the WRAITHGRID_SSH stand-in.
    pub program: String,
    pub ssh: Arc<dyn SshRunner>,
    /// This build's static host for a target (`remoteHostBinary` in app.ts), or None.
    pub binary: Arc<BinaryFor>,
    /// A pane's exit with `error` set means the link dropped (TS: `disconnected: true`).
    pub on_event: Arc<dyn Fn(HostEvent) + Send + Sync>,
    pub on_mismatch: Box<MismatchAsk>,
    /// Gets "<machine name>: <message>".
    pub log: Arc<dyn Fn(&str) + Send + Sync>,
}

/// Connects (starting the remote host if needed) and says which version answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub app_version: String,
    pub live: usize,
}

#[derive(Default)]
struct LinkState {
    /// The Rust bridge's ssh command, once the machine has it.
    bridge: Mutex<Option<Vec<String>>>,
    /// The last ssh started: its stderr says why a connection failed.
    stderr: Mutex<Option<StderrTail>>,
    /// The version the last welcome named.
    welcome: Arc<Mutex<Option<String>>>,
}

/// A machine's `PaneHost`; derefs to it for create/attach/write/resize/kill/detach.
pub struct RemoteHost {
    host: Arc<PaneHost>,
    state: Arc<LinkState>,
}

impl RemoteHost {
    pub fn new(d: RemoteHostDeps) -> RemoteHost {
        let state = Arc::new(LinkState::default());
        let m = d.machine.clone();
        let log: Arc<dyn Fn(&str) + Send + Sync> = {
            let (log, name) = (d.log.clone(), m.name.clone());
            Arc::new(move |msg: &str| log(&format!("{name}: {msg}")))
        };
        let connect = {
            let (state, ssh, program) = (state.clone(), d.ssh.clone(), d.program.clone());
            let (host, version, binary) = (m.ssh_host.clone(), d.app_version.clone(), d.binary);
            move || -> std::io::Result<(Reader, Writer)> {
                let mut bridge = state.bridge.lock().unwrap();
                if bridge.is_none() {
                    let run = |command: &str, input: Option<&[u8]>| {
                        ssh.run(&program, &ssh_args(&host, Some(&[command.into()])), input)
                    };
                    let prepared = prepare_remote(&PrepareDeps {
                        version: &version,
                        run: &run,
                        binary: &*binary,
                        log: &*log,
                    });
                    *bridge = prepared.unwrap_or_else(|e| {
                        log(&format!("preparing the Rust host: {e}"));
                        None
                    });
                }
                let link = ssh.open(&program, &ssh_args(&host, bridge.as_deref()))?;
                *state.stderr.lock().unwrap() = Some(link.stderr);
                let reader = WelcomeReader {
                    inner: link.reader,
                    first: Some(Vec::new()),
                    welcome: state.welcome.clone(),
                };
                let writer = SpawnWriter {
                    inner: link.writer,
                    bridge: bridge.is_some(),
                    line: Vec::new(),
                };
                Ok((Box::new(reader), Box::new(writer)))
            }
        };
        // Nothing to start from here: the remote end starts its own host.
        let start = {
            let (state, m) = (state.clone(), m.clone());
            move || -> Result<(), String> {
                let tail = state.stderr.lock().unwrap().clone();
                let why = tail.map_or(String::new(), |t| t.last_line(Duration::from_secs(1)));
                let why = if why.is_empty() {
                    why
                } else {
                    format!(": {why}")
                };
                let installed = if state.bridge.lock().unwrap().is_some() {
                    ""
                } else {
                    ", and Wraithgrid installed there with wraithgrid on PATH"
                };
                Err(format!(
                    "Could not reach {} ({}){why}. It needs ssh with a key or agent{installed}.",
                    m.name, m.ssh_host
                ))
            }
        };
        let on_event = {
            let (on_event, lost) = (d.on_event, lost_message(&m));
            Arc::new(move |e: HostEvent| match e {
                HostEvent::Exit {
                    pane_id,
                    exit_code,
                    signal,
                    error: Some(err),
                } if err == LOCAL_LOST => on_event(HostEvent::Exit {
                    pane_id,
                    exit_code,
                    signal,
                    error: Some(lost.clone()),
                }),
                other => on_event(other),
            })
        };
        let host = PaneHost::new(PaneHostDeps {
            connect: Box::new(connect),
            start: Box::new(start),
            // host-stdio puts in the remote host's token.
            read_token: Box::new(|| Ok(String::new())),
            app_version: d.app_version,
            on_event,
            on_mismatch: d.on_mismatch,
            start_timeout: Duration::from_secs(10),
            // ssh takes longer than a local socket.
            hello_timeout: Duration::from_secs(30),
        });
        RemoteHost { host, state }
    }

    /// The host itself, for a router that holds hosts by machine.
    pub fn pane_host(&self) -> Arc<PaneHost> {
        self.host.clone()
    }

    /// Starts a pane the machine resolves: a `plan` for the Rust bridge, else a `spawn` for
    /// `wraithgrid host-stdio`. Ok is the pid.
    pub fn spawn(&self, spec: &SpawnSpec) -> Result<i64, String> {
        // Sent as a marked `create`, so core tracks the pane like one it created.
        self.host.create(SpawnRequest {
            pane_id: spec.pane_id.clone(),
            file: SPAWN_MARK.into(),
            args: spawn_spec_json(spec),
            cwd: String::new(),
            env: BTreeMap::new(),
            cols: spec.cols,
            rows: spec.rows,
            approval: None,
        })
    }

    /// Connects (the machine starting its host if needed) and says which version answered
    /// and how many panes run there.
    pub fn probe(&self) -> Result<Probe, String> {
        self.host.connect()?;
        let live = self.host.list().iter().filter(|p| p.exit.is_none()).count();
        let app_version = self.state.welcome.lock().unwrap().clone();
        Ok(Probe {
            app_version: app_version.unwrap_or_default(),
            live,
        })
    }
}

impl Deref for RemoteHost {
    type Target = Arc<PaneHost>;

    fn deref(&self) -> &Arc<PaneHost> {
        &self.host
    }
}

/// ssh's stdout; notes the version of the welcome, its first frame.
struct WelcomeReader {
    inner: Reader,
    first: Option<Vec<u8>>,
    welcome: Arc<Mutex<Option<String>>>,
}

impl Read for WelcomeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if let Some(acc) = &mut self.first {
            acc.extend_from_slice(&buf[..n]);
            if let Some(end) = acc.iter().position(|b| *b == b'\n') {
                let line = String::from_utf8_lossy(&acc[..end]);
                if let Some(HostFrame::Welcome { app_version, .. }) = parse_host_frame(&line) {
                    *self.welcome.lock().unwrap() = Some(app_version);
                }
                self.first = None;
            } else if n == 0 || acc.len() > 64 * 1024 {
                self.first = None;
            }
        }
        Ok(n)
    }
}

/// ssh's stdin; turns a `create` marked by `RemoteHost::spawn` into the frame the remote end
/// resolves. Core writes each frame whole, so lines are buffered only across short writes.
struct SpawnWriter {
    inner: Writer,
    /// The Rust bridge runs there: it takes a plan, not a spec.
    bridge: bool,
    line: Vec<u8>,
}

impl SpawnWriter {
    fn translate(&self, line: &[u8]) -> Option<Vec<u8>> {
        if !line.starts_with(br#"{"t":"create""#) {
            return None;
        }
        let v: Value = serde_json::from_slice(line).ok()?;
        if v["req"]["file"].as_str() != Some(SPAWN_MARK) {
            return None;
        }
        let (id, spec) = (&v["id"], &v["req"]["args"]);
        let frame = match parse_spawn_spec(spec).filter(|_| self.bridge) {
            Some(s) => json!({ "t": "plan", "id": id, "plan": remote_plan(&s) }),
            None => json!({ "t": "spawn", "id": id, "spec": spec }),
        };
        let mut out = serde_json::to_vec(&frame).ok()?;
        out.push(b'\n');
        Some(out)
    }
}

impl Write for SpawnWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.line.extend_from_slice(buf);
        while let Some(end) = self.line.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.line.drain(..=end).collect();
            match self.translate(&line) {
                Some(frame) => self.inner.write_all(&frame)?,
                None => self.inner.write_all(&line)?,
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
