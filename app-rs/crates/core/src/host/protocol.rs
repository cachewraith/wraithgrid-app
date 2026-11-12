//! Wire format between the pty host and its clients (`src/host/protocol.ts`, PROTOCOL_VERSION
//! 1): newline-delimited JSON. The host is `host-rs`, unchanged, so the new app attaches to the
//! same background agents the Electron app started.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bump on any change a client of the previous version would not understand.
pub const PROTOCOL_VERSION: i64 = 1;

/// Largest frame accepted; a write is capped at 1 MB of text, JSON escaping can triple it.
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

/// What a host runs for a pane, resolved by the window (spawn.rs).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnRequest {
    pub pane_id: String,
    pub file: String,
    /// A list, or one verbatim command line (a Windows `.cmd` shim through cmd.exe).
    pub args: Value,
    pub cwd: String,
    pub env: std::collections::BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
    /// The pane CLI's "needs approval" output as a RegExp source; none for a shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ClientFrame {
    Hello {
        protocol: i64,
        #[serde(rename = "appVersion")]
        app_version: String,
        token: String,
        role: String,
    },
    Create {
        id: u64,
        req: SpawnRequest,
    },
    #[serde(rename_all = "camelCase")]
    Attach {
        id: u64,
        pane_id: String,
        cols: u16,
        rows: u16,
    },
    List {
        id: u64,
    },
    #[serde(rename_all = "camelCase")]
    Write {
        pane_id: String,
        data: String,
    },
    #[serde(rename_all = "camelCase")]
    Resize {
        pane_id: String,
        cols: u16,
        rows: u16,
    },
    #[serde(rename_all = "camelCase")]
    Kill {
        pane_id: String,
    },
    /// Stops every pane; with `exit` the host also quits once they are gone.
    Shutdown {
        id: u64,
        exit: bool,
    },
    #[serde(rename_all = "camelCase")]
    Read {
        id: u64,
        pane_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        lines: Option<u32>,
    },
    /// Resolves once the pane is in `until` (for `quiet_ms` when idle), or after `timeout_ms`.
    #[serde(rename_all = "camelCase")]
    Wait {
        id: u64,
        pane_id: String,
        until: String,
        quiet_ms: u64,
        timeout_ms: u64,
    },
    /// A command for the window (new pane, split), relayed to the GUI client.
    Gui {
        id: u64,
        action: Value,
    },
    /// The GUI's answer to a relayed `gui` frame.
    GuiReply {
        id: u64,
        reply: Value,
    },
    /// GUI settings the host enforces.
    #[serde(rename_all = "camelCase")]
    Config {
        cli_access: bool,
    },
}

impl ClientFrame {
    pub fn encode(&self) -> String {
        let mut s = serde_json::to_string(self).expect("frame serializes");
        s.push('\n');
        s
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PaneExit {
    #[serde(rename = "exitCode")]
    pub exit_code: i64,
    pub signal: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneSummary {
    pub pane_id: String,
    pub pid: i64,
    /// Set when the process ended while no client was attached.
    pub exit: Option<PaneExit>,
    /// running, idle, blocked or exited.
    pub state: String,
}

/// A reply, kept as JSON: its shape depends on the request (see HostReply in protocol.ts).
#[derive(Clone, Debug, PartialEq)]
pub struct Reply(pub Value);

impl Reply {
    pub fn ok(&self) -> bool {
        self.0.get("ok").and_then(Value::as_bool) == Some(true)
    }

    pub fn error(&self) -> String {
        self.0
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("The background host failed.")
            .to_string()
    }

    pub fn closed() -> Reply {
        Reply(serde_json::json!({ "ok": false, "error": "Host connection closed" }))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum HostFrame {
    Welcome {
        protocol: i64,
        app_version: String,
        pid: i64,
    },
    Refused {
        error: String,
    },
    Reply {
        id: u64,
        reply: Reply,
    },
    /// `replay`: a reattached pane's screen, sent ahead of its live output.
    Data {
        pane_id: String,
        data: String,
        replay: bool,
    },
    Exit {
        pane_id: String,
        exit_code: i64,
        signal: Option<i64>,
    },
    Gui {
        id: u64,
        action: Value,
    },
}

/// Host frames are trusted once the hello succeeded; anything unrecognised is None.
pub fn parse_host_frame(line: &str) -> Option<HostFrame> {
    let v: Value = serde_json::from_str(line).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    let n = |k: &str| v.get(k).and_then(Value::as_i64);
    Some(match v.get("t")?.as_str()? {
        "welcome" => HostFrame::Welcome {
            protocol: n("protocol")?,
            app_version: s("appVersion")?,
            pid: n("pid")?,
        },
        "refused" => HostFrame::Refused { error: s("error")? },
        "reply" => HostFrame::Reply {
            id: v.get("id")?.as_u64()?,
            reply: Reply(v.get("reply")?.clone()),
        },
        "data" => HostFrame::Data {
            pane_id: s("paneId")?,
            data: s("data")?,
            replay: v.get("replay").and_then(Value::as_bool) == Some(true),
        },
        "exit" => HostFrame::Exit {
            pane_id: s("paneId")?,
            exit_code: n("exitCode")?,
            signal: n("signal"),
        },
        "gui" => HostFrame::Gui {
            id: v.get("id")?.as_u64()?,
            action: v.get("action")?.clone(),
        },
        _ => return None,
    })
}

/// Splits a byte stream into lines. A frame over MAX_FRAME is an error: the caller drops the
/// link (no resync on garbage).
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn push(
        &mut self,
        chunk: &[u8],
        mut on_line: impl FnMut(&str),
    ) -> Result<(), &'static str> {
        let mut from = 0;
        for (i, b) in chunk.iter().enumerate() {
            if *b == b'\n' {
                self.buf.extend_from_slice(&chunk[from..i]);
                from = i + 1;
                if !self.buf.is_empty() {
                    let line = std::mem::take(&mut self.buf);
                    match std::str::from_utf8(&line) {
                        Ok(text) => on_line(text),
                        Err(_) => return Err("bad frame"),
                    }
                }
            }
        }
        self.buf.extend_from_slice(&chunk[from..]);
        if self.buf.len() > MAX_FRAME {
            self.buf.clear();
            return Err("frame too large");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_the_ts_client() {
        let hello = ClientFrame::Hello {
            protocol: 1,
            app_version: "2.0.0".into(),
            token: "t".into(),
            role: "gui".into(),
        };
        assert_eq!(hello.encode(), "{\"t\":\"hello\",\"protocol\":1,\"appVersion\":\"2.0.0\",\"token\":\"t\",\"role\":\"gui\"}\n");
        let w = ClientFrame::Write {
            pane_id: "p1".into(),
            data: "ls\r".into(),
        };
        assert_eq!(
            w.encode(),
            "{\"t\":\"write\",\"paneId\":\"p1\",\"data\":\"ls\\r\"}\n"
        );
        let c = ClientFrame::Config { cli_access: true };
        assert_eq!(c.encode(), "{\"t\":\"config\",\"cliAccess\":true}\n");
        let w = ClientFrame::Wait {
            id: 4,
            pane_id: "p".into(),
            until: "idle".into(),
            quiet_ms: 1500,
            timeout_ms: 600000,
        };
        assert_eq!(w.encode(), "{\"t\":\"wait\",\"id\":4,\"paneId\":\"p\",\"until\":\"idle\",\"quietMs\":1500,\"timeoutMs\":600000}\n");
        let gr = ClientFrame::GuiReply {
            id: 3,
            reply: serde_json::json!({"ok": true}),
        };
        assert!(gr.encode().starts_with("{\"t\":\"guiReply\",\"id\":3"));
    }

    #[test]
    fn parses_host_frames() {
        assert_eq!(
            parse_host_frame(r#"{"t":"data","paneId":"p","data":"x","replay":true}"#),
            Some(HostFrame::Data {
                pane_id: "p".into(),
                data: "x".into(),
                replay: true
            })
        );
        assert_eq!(
            parse_host_frame(r#"{"t":"exit","paneId":"p","exitCode":0,"signal":null}"#),
            Some(HostFrame::Exit {
                pane_id: "p".into(),
                exit_code: 0,
                signal: None
            })
        );
        let r = parse_host_frame(r#"{"t":"reply","id":2,"reply":{"ok":false,"error":"nope"}}"#)
            .unwrap();
        let HostFrame::Reply { reply, .. } = r else {
            panic!()
        };
        assert!(!reply.ok());
        assert_eq!(reply.error(), "nope");
        assert_eq!(parse_host_frame("{\"t\":\"other\"}"), None);
    }

    #[test]
    fn frame_reader_joins_chunks() {
        let mut r = FrameReader::default();
        let mut lines = Vec::new();
        r.push(b"{\"a\":1}\n{\"b\"", |l| lines.push(l.to_string()))
            .unwrap();
        r.push(b":2}\n\n", |l| lines.push(l.to_string())).unwrap();
        assert_eq!(lines, vec!["{\"a\":1}", "{\"b\":2}"]);
        assert!(r.push(&vec![b'x'; MAX_FRAME + 1], |_| {}).is_err());
    }
}
