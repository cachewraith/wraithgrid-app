//! `wraithgrid host-stdio` (H5, `src/main/host-bridge.ts`): runs on a remote machine, started
//! by the window over `ssh`, and joins the ssh channel to that machine's background host. ssh
//! already proved who the user is, so the bridge puts in the host's token itself. It also
//! turns `spawn` frames into `create` ones, resolving the CLI, folder and env on this machine.
//! The Rust host's own `stdio` bridge (host-rs/src/bridge.rs) does the same for `plan` frames.

use std::io::{Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wraithgrid_core::host::client::{Reader, Writer};
use wraithgrid_core::host::protocol::{FrameReader, SpawnRequest};
use wraithgrid_core::spawn::SpawnSpec;

use crate::spec::parse_spawn_spec;

/// Resolves a pane spec on this machine.
pub type Resolve = dyn Fn(&SpawnSpec) -> Result<SpawnRequest, String> + Send;

pub struct BridgeDeps {
    /// ssh's channel: frames from the window, and where the host's go.
    pub input: Reader,
    pub output: Writer,
    /// This machine's host; fails when none runs.
    pub connect_host: Box<dyn Fn() -> std::io::Result<(Reader, Writer)> + Send>,
    pub start_host: Box<dyn Fn() -> Result<(), String> + Send>,
    pub read_token: Box<dyn Fn() -> std::io::Result<String> + Send>,
    /// `resolve_spawn` with this machine's context.
    pub resolve: Box<Resolve>,
    pub start_timeout: Duration,
}

fn ensure_host(d: &BridgeDeps) -> Result<(Reader, Writer), String> {
    if let Ok(host) = (d.connect_host)() {
        return Ok(host);
    }
    // None runs on this machine yet.
    (d.start_host)()?;
    let deadline = Instant::now() + d.start_timeout;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        match (d.connect_host)() {
            Ok(host) => return Ok(host),
            Err(e) if Instant::now() > deadline => return Err(e.to_string()),
            Err(_) => {}
        }
    }
}

/// Bridges until either side closes. Blocking writes give the backpressure the TS needed by
/// hand (#141): a slow ssh link stops the host reader rather than buffering without end.
/// Threads still blocked on the other side are left to the process's exit.
pub fn run_bridge(d: BridgeDeps) -> Result<(), String> {
    let (mut host_read, mut host_write) = ensure_host(&d)?;
    let token = (d.read_token)().map_err(|e| e.to_string())?;
    let token = token.trim().to_string();
    // Frame by frame, so a reply the bridge writes itself never lands inside a host frame.
    let output = Arc::new(Mutex::new(d.output));
    let (done, finished) = mpsc::channel::<()>();

    {
        let (output, done) = (output.clone(), done.clone());
        std::thread::spawn(move || {
            let mut frames = FrameReader::default();
            let mut buf = vec![0u8; 64 * 1024];
            'read: while let Ok(n @ 1..) = host_read.read(&mut buf) {
                let mut lines = Vec::new();
                if frames
                    .push(&buf[..n], |l| lines.push(l.to_string()))
                    .is_err()
                {
                    break;
                }
                for line in lines {
                    if serde_json::from_str::<Value>(&line).is_err() {
                        break 'read;
                    }
                    let mut out = output.lock().unwrap();
                    if out
                        .write_all(format!("{line}\n").as_bytes())
                        .and_then(|_| out.flush())
                        .is_err()
                    {
                        break 'read;
                    }
                }
            }
            let _ = done.send(());
        });
    }

    let (mut input, resolve) = (d.input, d.resolve);
    std::thread::spawn(move || {
        // One frame at a time, in order: a `spawn` waits for its lookup before later frames go.
        let mut greeted = false;
        let mut forward = |raw: Value| -> std::io::Result<bool> {
            if !greeted {
                greeted = true;
                if raw.get("t").and_then(Value::as_str) != Some("hello") {
                    return Ok(false);
                }
                let mut hello = raw;
                hello["token"] = json!(token);
                hello["role"] = json!("gui");
                host_write.write_all(format!("{hello}\n").as_bytes())?;
                return Ok(true);
            }
            if raw.get("t").and_then(Value::as_str) != Some("spawn") {
                host_write.write_all(format!("{raw}\n").as_bytes())?;
                return Ok(true);
            }
            let id = raw.get("id").filter(|v| v.is_number()).cloned();
            let id = id.unwrap_or(json!(-1));
            let resolved = match raw.get("spec").and_then(parse_spawn_spec) {
                Some(spec) => resolve(&spec),
                None => Err("Invalid pane request".into()),
            };
            match resolved {
                Ok(req) => {
                    let frame = json!({ "t": "create", "id": id, "req": req });
                    host_write.write_all(format!("{frame}\n").as_bytes())?;
                }
                Err(error) => {
                    let reply =
                        json!({ "t": "reply", "id": id, "reply": { "ok": false, "error": error } });
                    let mut out = output.lock().unwrap();
                    out.write_all(format!("{reply}\n").as_bytes())?;
                    out.flush()?;
                }
            }
            Ok(true)
        };
        let mut frames = FrameReader::default();
        let mut buf = vec![0u8; 64 * 1024];
        'read: while let Ok(n @ 1..) = input.read(&mut buf) {
            let mut lines = Vec::new();
            if frames
                .push(&buf[..n], |l| lines.push(l.to_string()))
                .is_err()
            {
                break;
            }
            for line in lines {
                let Ok(raw) = serde_json::from_str::<Value>(&line) else {
                    break 'read;
                };
                if !matches!(forward(raw), Ok(true)) {
                    break 'read;
                }
            }
        }
        // Dropping the host writer ends our side of the host link.
        drop(host_write);
        let _ = done.send(());
    });

    let _ = finished.recv();
    Ok(())
}
