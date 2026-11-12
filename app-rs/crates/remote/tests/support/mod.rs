//! Stand-ins for the far end of ssh, so the machine tests never run a real ssh: a fake ssh
//! runner whose links are in-memory pipes, and a fake background host that speaks just
//! enough of the protocol (hello, create, plan, list) and records every frame it got.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use wraithgrid_core::host::client::{Reader, Writer};
use wraithgrid_remote::{SshLink, SshRun, SshRunner, StderrTail};

/// Two OS pipes as one duplex link: (this end, the far end).
pub fn pipe_pair() -> ((Reader, Writer), (Reader, Writer)) {
    let (a_read, a_write) = std::io::pipe().unwrap();
    let (b_read, b_write) = std::io::pipe().unwrap();
    (
        (Box::new(a_read), Box::new(b_write)),
        (Box::new(b_read), Box::new(a_write)),
    )
}

/// A host on some machine: what it was sent and the panes it runs.
#[derive(Default)]
pub struct FakeHost {
    /// The token a hello must carry; None takes any.
    pub token: Option<String>,
    pub frames: Mutex<Vec<Value>>,
    pub panes: Mutex<Vec<String>>,
    /// Drops the link on a `resize`, like a network that went away.
    pub cut_on_resize: bool,
}

impl FakeHost {
    pub fn frames_of(&self, t: &str) -> Vec<Value> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["t"] == t)
            .cloned()
            .collect()
    }

    /// Answers one client until it hangs up.
    pub fn serve(&self, input: Reader, mut output: Writer) {
        for line in BufReader::new(input).lines().map_while(Result::ok) {
            let f: Value = serde_json::from_str(&line).unwrap();
            self.frames.lock().unwrap().push(f.clone());
            let id = f["id"].clone();
            let reply = match f["t"].as_str().unwrap_or("") {
                "hello" => {
                    if self.token.as_ref().is_some_and(|t| f["token"] != *t) {
                        json!({ "t": "refused", "error": "bad token" })
                    } else {
                        json!({ "t": "welcome", "protocol": 1, "appVersion": "2.0.0", "pid": 1 })
                    }
                }
                "create" | "plan" => {
                    let pane = f["req"]["paneId"]
                        .as_str()
                        .or(f["plan"]["paneId"].as_str())
                        .unwrap_or("")
                        .to_string();
                    self.panes.lock().unwrap().push(pane);
                    json!({ "t": "reply", "id": id, "reply": { "ok": true, "pid": 7 } })
                }
                "list" => {
                    let panes: Vec<Value> = self
                        .panes
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|p| json!({ "paneId": p, "pid": 7, "exit": null, "state": "running" }))
                        .collect();
                    json!({ "t": "reply", "id": id, "reply": { "ok": true, "panes": panes } })
                }
                "resize" if self.cut_on_resize => return,
                _ => continue,
            };
            if output.write_all(format!("{reply}\n").as_bytes()).is_err() {
                return;
            }
        }
    }
}

/// A command run on the machine: its arguments and stdin.
pub type Run = (Vec<String>, Option<Vec<u8>>);

type Far = dyn Fn(Reader, Writer) + Send + Sync;

/// An ssh runner whose links run `far` on a thread at the other end.
pub struct FakeSsh {
    /// What the preflight command prints.
    pub preflight: String,
    pub upload_code: Option<i32>,
    /// Every `run`: its arguments and stdin.
    pub runs: Mutex<Vec<Run>>,
    /// Every `open`: its arguments.
    pub opens: Mutex<Vec<Vec<String>>>,
    /// Set: ssh fails at once, printing this on stderr.
    pub fail: Option<String>,
    pub far: Arc<Far>,
}

impl FakeSsh {
    pub fn new(preflight: &str, far: impl Fn(Reader, Writer) + Send + Sync + 'static) -> Self {
        FakeSsh {
            preflight: preflight.into(),
            upload_code: Some(0),
            runs: Mutex::default(),
            opens: Mutex::default(),
            fail: None,
            far: Arc::new(far),
        }
    }
}

impl SshRunner for FakeSsh {
    fn open(&self, _program: &str, args: &[String]) -> std::io::Result<SshLink> {
        self.opens.lock().unwrap().push(args.to_vec());
        if let Some(stderr) = &self.fail {
            return Ok(SshLink {
                reader: Box::new(std::io::empty()),
                writer: Box::new(std::io::sink()),
                stderr: StderrTail::fixed(stderr),
            });
        }
        let ((reader, writer), (far_read, far_write)) = pipe_pair();
        let far = self.far.clone();
        std::thread::spawn(move || far(far_read, far_write));
        Ok(SshLink {
            reader,
            writer,
            stderr: StderrTail::fixed(""),
        })
    }

    fn run(
        &self,
        _program: &str,
        args: &[String],
        input: Option<&[u8]>,
    ) -> std::io::Result<SshRun> {
        self.runs
            .lock()
            .unwrap()
            .push((args.to_vec(), input.map(<[u8]>::to_vec)));
        Ok(match input {
            Some(_) => SshRun {
                code: self.upload_code,
                ..SshRun::default()
            },
            None => SshRun {
                code: Some(0),
                stdout: self.preflight.clone(),
                stderr: String::new(),
            },
        })
    }
}

/// Reads one frame from a link.
pub fn read_frame(r: &mut Reader) -> Value {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while r.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
        line.push(byte[0]);
    }
    serde_json::from_slice(&line).unwrap()
}
