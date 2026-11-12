//! `wraithgrid <command>` (H6, `src/main/cli.ts`): a client of the background host, so a
//! script, or an agent in a pane, can list panes, read their screens, type into them, wait for
//! them and open new ones. Runs in the app's own binary before any window exists, and exits.
//! Pane titles and accounts come from config.json (read only); live state from the host.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::agent_cli::AgentCli;
use crate::config::Config;
use crate::host::client::{HostConnection, Reader, Writer};
use crate::host::protocol::{ClientFrame, HostFrame, PaneSummary};

pub const CLI_COMMANDS: &[&str] = &[
    "list", "read", "send", "wait", "new", "split", "attach", "stop", "help",
];
const PANE_STATES: &[&str] = &["running", "idle", "blocked", "exited"];

/// True when the app was started as `wraithgrid <command>` rather than to open its window.
pub fn is_cli_command(arg: Option<&str>) -> bool {
    arg.is_some_and(|a| CLI_COMMANDS.contains(&a) || a == "--help")
}

/// Ctrl+]: leaves `attach`, the pane keeps running (as telnet does).
pub const DETACH_KEY: char = '\x1d';

/// Undoes what a pane's program may have switched on in the terminal it was drawn in.
const TTY_RESET: &str =
    "\x1b[?1049l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?25h\x1b[0m";

pub const CLI_HELP: &str = "Usage: wraithgrid <command> [options]

Talks to the agents Wraithgrid runs. Turn it on first: Settings → Command line.
A pane is named by its id or its title; inside a pane, $WRAITHGRID_PANE is its own id.

  list [--json]                     panes with their state (running, idle, blocked, exited)
  read <pane> [--lines N]           the pane's screen as text (N: last N lines, scrollback too)
  send <pane> <text…> [--no-enter]  types the text into the pane, then Enter
  wait <pane> [--until STATE] [--quiet MS] [--timeout S]
                                    waits until the pane is idle (default; quiet for MS,
                                    1500), running, blocked (asking for approval) or exited
  new [--account A] [--cwd DIR] [--workspace W] [--shell] [-- ARGS…]
                                    opens a pane in the window and prints its id
  split <pane> [--account A] [--cwd DIR] [--shell] [-- ARGS…]
                                    opens a pane beside another, same account and folder
  attach <pane>                     shows the pane in this terminal and types into it;
                                    Ctrl+] leaves, the pane keeps running (ssh -t works)
  stop                              stops every agent and the background host

Exit status: 0 done, 1 failed or timed out, 2 wrong usage.";

/// The local terminal, for `wraithgrid attach` (H7).
pub trait Tty: Send {
    fn size(&self) -> (u16, u16);
    fn write(&mut self, data: &str);
    /// Raw mode on: every key goes to the pane.
    fn raw(&mut self, on: bool);
    /// Starts delivering keyboard input; returns the receiver.
    fn input(&mut self) -> mpsc::Receiver<String>;
}

pub type Connect = Box<dyn Fn() -> std::io::Result<(Reader, Writer)>>;

pub struct CliDeps {
    pub connect: Connect,
    pub read_token: Box<dyn Fn() -> std::io::Result<String>>,
    pub read_config: Box<dyn Fn() -> Option<Config>>,
    pub app_version: String,
    pub cwd: std::path::PathBuf,
    pub out: Box<dyn FnMut(&str)>,
    pub err: Box<dyn FnMut(&str)>,
    pub sleep: Box<dyn Fn(Duration)>,
    /// Absent when stdin/stdout is not a terminal.
    pub tty: Option<Box<dyn Tty>>,
}

enum CliError {
    Usage(String),
    Failed(String),
}

fn usage<T>(msg: impl Into<String>) -> Result<T, CliError> {
    Err(CliError::Usage(msg.into()))
}

fn failed<T>(msg: impl Into<String>) -> Result<T, CliError> {
    Err(CliError::Failed(msg.into()))
}

const VALUE_FLAGS: &[&str] = &[
    "--lines",
    "--until",
    "--quiet",
    "--timeout",
    "--account",
    "--cwd",
    "--workspace",
];

#[derive(Default)]
struct Parsed {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
    /// Everything after `--`: the new pane's launch args.
    rest: Vec<String>,
}

impl Parsed {
    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == flag)
    }

    fn str(&self, flag: &str) -> Option<String> {
        self.flags
            .iter()
            .rev()
            .find(|(f, _)| f == flag)
            .and_then(|(_, v)| v.clone())
    }

    fn num(&self, flag: &str, fallback: f64) -> Result<f64, CliError> {
        match self.str(flag) {
            None => Ok(fallback),
            Some(v) => match v.parse::<f64>() {
                Ok(n) if n.is_finite() && n >= 0.0 => Ok(n),
                _ => usage(format!("{flag} must be a number")),
            },
        }
    }
}

fn parse(args: &[String]) -> Result<Parsed, CliError> {
    let mut p = Parsed::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            p.rest = args[i + 1..].to_vec();
            break;
        }
        if a.starts_with("--") {
            let (name, inline) = match a.find('=') {
                Some(eq) if eq > 0 => (a[..eq].to_string(), Some(a[eq + 1..].to_string())),
                _ => (a.clone(), None),
            };
            if VALUE_FLAGS.contains(&name.as_str()) {
                let value = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        match args.get(i) {
                            Some(v) => v.clone(),
                            None => return usage(format!("{name} needs a value")),
                        }
                    }
                };
                p.flags.push((name, Some(value)));
            } else {
                p.flags.push((name, None));
            }
        } else {
            p.positional.push(a.clone());
        }
        i += 1;
    }
    Ok(p)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PaneRow {
    pub id: String,
    pub title: String,
    pub workspace: String,
    pub account: String,
    pub cli: String,
    /// running, idle, blocked, exited, or stopped (in the config but not in the host).
    pub state: String,
    pub pid: Option<i64>,
}

pub fn rows(config: Option<&Config>, live: &[PaneSummary]) -> Vec<PaneRow> {
    let mut remaining: Vec<&PaneSummary> = live.iter().collect();
    let mut out = Vec::new();
    for ws in config.map(|c| c.workspaces.as_slice()).unwrap_or_default() {
        for pane in &ws.panes {
            let account =
                config.and_then(|c| pane.account_id.as_deref().and_then(|id| c.account(id)));
            let l = remaining
                .iter()
                .position(|l| l.pane_id == pane.id)
                .map(|i| remaining.remove(i));
            out.push(PaneRow {
                id: pane.id.clone(),
                title: pane.title.clone(),
                workspace: ws.name.clone(),
                account: if pane.shell {
                    String::new()
                } else {
                    account.map(|a| a.name.clone()).unwrap_or_default()
                },
                cli: if pane.shell {
                    "shell".into()
                } else {
                    account.map_or(AgentCli::Claude, |a| a.cli).as_str().into()
                },
                state: l.map_or("stopped".into(), |l| l.state.clone()),
                pid: l.map(|l| l.pid),
            });
        }
    }
    // Running in the host but not in the config (e.g. a pane closed from another window).
    for l in remaining {
        out.push(PaneRow {
            id: l.pane_id.clone(),
            title: String::new(),
            workspace: String::new(),
            account: String::new(),
            cli: String::new(),
            state: l.state.clone(),
            pid: Some(l.pid),
        });
    }
    out
}

pub fn table(list: &[PaneRow]) -> String {
    let head = ["ID", "TITLE", "WORKSPACE", "ACCOUNT", "CLI", "STATE", "PID"];
    let cells: Vec<Vec<String>> = list
        .iter()
        .map(|r| {
            vec![
                r.id.clone(),
                r.title.clone(),
                r.workspace.clone(),
                r.account.clone(),
                r.cli.clone(),
                r.state.clone(),
                r.pid
                    .filter(|p| *p != 0)
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
            ]
        })
        .collect();
    let width = |i: usize| {
        cells
            .iter()
            .map(|c| c[i].chars().count())
            .chain([head[i].len()])
            .max()
            .unwrap_or(0)
    };
    let widths: Vec<usize> = (0..head.len()).map(width).collect();
    std::iter::once(head.iter().map(|h| h.to_string()).collect::<Vec<_>>())
        .chain(cells)
        .map(|c| {
            c.iter()
                .enumerate()
                .map(|(i, v)| {
                    format!(
                        "{v}{}",
                        " ".repeat(widths[i].saturating_sub(v.chars().count()))
                    )
                })
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A pane by id, else by a title only one pane has (case-insensitive).
fn resolve_pane<'a>(r: &str, list: &'a [PaneRow]) -> Result<&'a PaneRow, CliError> {
    if let Some(exact) = list.iter().find(|p| p.id == r) {
        return Ok(exact);
    }
    let titled: Vec<&PaneRow> = list
        .iter()
        .filter(|p| p.title.to_lowercase() == r.to_lowercase())
        .collect();
    match titled.len() {
        1 => Ok(titled[0]),
        0 => failed(format!("No pane \"{r}\" (wraithgrid list shows them).")),
        n => failed(format!(
            "{n} panes are titled \"{r}\"; use an id (wraithgrid list)."
        )),
    }
}

pub fn run_cli(argv: &[String], mut deps: CliDeps) -> i32 {
    let command = argv.first().map(String::as_str);
    let Some(command) = command.filter(|c| *c != "help" && *c != "--help") else {
        (deps.out)(CLI_HELP);
        return 0;
    };
    let (frames_tx, frames_rx) = mpsc::channel::<Option<HostFrame>>();
    let result = run(command, &argv[1..], &mut deps, frames_tx, frames_rx);
    match result {
        Ok(code) => code,
        Err(CliError::Usage(msg)) => {
            (deps.err)(&format!("wraithgrid: {msg}\n\n{CLI_HELP}"));
            2
        }
        Err(CliError::Failed(msg)) => {
            (deps.err)(&format!("wraithgrid: {msg}"));
            1
        }
    }
}

fn run(
    command: &str,
    args: &[String],
    deps: &mut CliDeps,
    frames_tx: mpsc::Sender<Option<HostFrame>>,
    frames_rx: mpsc::Receiver<Option<HostFrame>>,
) -> Result<i32, CliError> {
    let p = parse(args)?;
    let stream = (deps.connect)().map_err(|_| {
        CliError::Failed(
            "Wraithgrid is not running any agents (no background host). Open Wraithgrid first."
                .into(),
        )
    })?;
    let token = (deps.read_token)().map_err(|e| CliError::Failed(e.to_string()))?;
    let close_tx = frames_tx.clone();
    let c = Arc::new(
        HostConnection::open(
            stream,
            token.trim(),
            &deps.app_version,
            "cli",
            Box::new(move |f| {
                let _ = frames_tx.send(Some(f));
            }),
            Box::new(move || {
                let _ = close_tx.send(None);
            }),
            Duration::from_secs(5),
        )
        .map_err(CliError::Failed)?,
    );
    let config = (deps.read_config)();
    let list_panes = || -> Result<Vec<PaneRow>, CliError> {
        let r = c.request(|id| ClientFrame::List { id });
        if !r.ok() {
            return failed(r.error());
        }
        let live: Vec<PaneSummary> =
            r.0.get("panes")
                .and_then(|p| serde_json::from_value(p.clone()).ok())
                .unwrap_or_default();
        Ok(rows(config.as_ref(), &live))
    };
    let need = |i: usize, what: &str| -> Result<String, CliError> {
        match p.positional.get(i) {
            Some(v) => Ok(v.clone()),
            None => usage(format!("{command} needs {what}")),
        }
    };
    let code = match command {
        "list" => {
            let list = list_panes()?;
            let text = if p.has("--json") {
                serde_json::to_string_pretty(&list).unwrap()
            } else {
                table(&list)
            };
            (deps.out)(&text);
            0
        }
        "read" => {
            let list = list_panes()?;
            let pane = resolve_pane(&need(0, "a pane")?, &list)?;
            let lines = if p.has("--lines") {
                Some(p.num("--lines", 0.0)? as u32).filter(|n| *n > 0)
            } else {
                None
            };
            let r = c.request(|id| ClientFrame::Read {
                id,
                pane_id: pane.id.clone(),
                lines,
            });
            if !r.ok() {
                return failed(r.error());
            }
            (deps.out)(r.0.get("text").and_then(Value::as_str).unwrap_or(""));
            0
        }
        "send" => {
            let list = list_panes()?;
            let pane = resolve_pane(&need(0, "a pane")?, &list)?;
            let text = p.positional[1..].join(" ");
            if text.is_empty() && p.has("--no-enter") {
                return usage("send needs text");
            }
            if pane.state == "stopped" || pane.state == "exited" {
                return failed(format!("Pane {} is not running.", pane.id));
            }
            // A line break inside the text is the CLI's "new line" key, not Enter.
            let new_line = AgentCli::parse(&pane.cli).and_then(|cli| cli.spec().new_line);
            let typed = match new_line {
                Some(nl) => text.replace("\r\n", "\n").replace('\n', nl),
                None => text,
            };
            if !typed.is_empty() {
                c.send(&ClientFrame::Write {
                    pane_id: pane.id.clone(),
                    data: typed,
                });
            }
            if !p.has("--no-enter") {
                // Text and Enter in one chunk would read as a paste to some CLIs, and not submit.
                (deps.sleep)(Duration::from_millis(120));
                c.send(&ClientFrame::Write {
                    pane_id: pane.id.clone(),
                    data: "\r".into(),
                });
            }
            // Make sure the writes left before the connection closes.
            c.request(|id| ClientFrame::List { id });
            // The window marks the pane as having a conversation, so a restart resumes it.
            if !p.has("--no-enter") {
                c.request(|id| ClientFrame::Gui {
                    id,
                    action: json!({ "kind": "sent", "paneId": pane.id }),
                });
            }
            0
        }
        "wait" => {
            let list = list_panes()?;
            let pane = resolve_pane(&need(0, "a pane")?, &list)?;
            let until = p.str("--until").unwrap_or_else(|| "idle".into());
            if !PANE_STATES.contains(&until.as_str()) {
                return usage(format!("--until is one of {}", PANE_STATES.join(", ")));
            }
            let quiet_ms = p.num("--quiet", 1500.0)?.round() as u64;
            let timeout_ms = ((p.num("--timeout", 600.0)? * 1000.0).round() as u64).max(1);
            let r = c.request(|id| ClientFrame::Wait {
                id,
                pane_id: pane.id.clone(),
                until: until.clone(),
                quiet_ms,
                timeout_ms,
            });
            if !r.ok() {
                return failed(r.error());
            }
            (deps.out)(r.0.get("state").and_then(Value::as_str).unwrap_or(&until));
            0
        }
        "new" | "split" => {
            let mut action = serde_json::Map::new();
            if command == "split" {
                let list = list_panes()?;
                let pane = resolve_pane(&need(0, "a pane")?, &list)?;
                action.insert("kind".into(), json!("split"));
                action.insert("paneId".into(), json!(pane.id));
            } else {
                action.insert("kind".into(), json!("new"));
            }
            if let Some(a) = p.str("--account") {
                action.insert("account".into(), json!(a));
            }
            let cwd = p
                .str("--cwd")
                .map(|d| deps.cwd.join(d))
                .or_else(|| (command == "new").then(|| deps.cwd.clone()));
            if let Some(cwd) = cwd {
                action.insert("cwd".into(), json!(cwd.to_string_lossy()));
            }
            if let (Some(w), "new") = (p.str("--workspace"), command) {
                action.insert("workspace".into(), json!(w));
            }
            if p.has("--shell") {
                action.insert("shell".into(), json!(true));
            }
            if !p.rest.is_empty() {
                action.insert("args".into(), json!(p.rest));
            }
            let r = c.request(|id| ClientFrame::Gui {
                id,
                action: Value::Object(action),
            });
            if !r.ok() {
                return failed(r.error());
            }
            let pane_id =
                r.0.pointer("/result/paneId")
                    .map(|v| {
                        v.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string())
                    })
                    .unwrap_or_default();
            // The window starts the pane a moment later; return once the host runs it.
            for _ in 0..100 {
                let l = c.request(|id| ClientFrame::List { id });
                if l.0
                    .get("panes")
                    .and_then(Value::as_array)
                    .is_some_and(|ps| ps.iter().any(|x| x["paneId"] == json!(pane_id)))
                {
                    break;
                }
                (deps.sleep)(Duration::from_millis(100));
            }
            (deps.out)(&pane_id);
            0
        }
        "attach" => {
            let list = list_panes()?;
            let pane = resolve_pane(&need(0, "a pane")?, &list)?.clone();
            let Some(tty) = deps.tty.as_mut() else {
                return failed("attach needs a terminal (over ssh, use ssh -t).");
            };
            if pane.state == "stopped" {
                return failed(format!("Pane {} is not running.", pane.id));
            }
            attach(&c, &pane.id, tty.as_mut(), frames_rx)?
        }
        "stop" => {
            let live = list_panes()?
                .iter()
                .filter(|r| r.state != "stopped" && r.state != "exited")
                .count();
            let r = c.request(|id| ClientFrame::Shutdown { id, exit: true });
            if !r.ok() {
                return failed(r.error());
            }
            (deps.out)(&format!(
                "Stopped {live} agent{}.",
                if live == 1 { "" } else { "s" }
            ));
            0
        }
        other => return usage(format!("Unknown command \"{other}\"")),
    };
    c.close();
    Ok(code)
}

/// Mirrors one pane in the terminal until Ctrl+], the pane's exit or the host's.
fn attach(
    c: &Arc<HostConnection>,
    pane_id: &str,
    tty: &mut dyn Tty,
    frames: mpsc::Receiver<Option<HostFrame>>,
) -> Result<i32, CliError> {
    enum Ev {
        Frame(Option<HostFrame>),
        Input(String),
        Size(u16, u16),
        Reply(crate::host::protocol::Reply),
    }
    let (tx, rx) = mpsc::channel::<Ev>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for f in frames {
                if tx.send(Ev::Frame(f)).is_err() {
                    break;
                }
            }
        });
    }
    let input = tty.input();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for data in input {
                if tx.send(Ev::Input(data)).is_err() {
                    break;
                }
            }
        });
    }
    tty.raw(true);
    let (cols, rows) = tty.size();
    {
        let (tx, c, id) = (tx.clone(), c.clone(), pane_id.to_string());
        std::thread::spawn(move || {
            let _ = tx.send(Ev::Reply(c.request(|rid| ClientFrame::Attach {
                id: rid,
                pane_id: id,
                cols,
                rows,
            })));
        });
    }
    let mut size = (cols, rows);
    // The screen comes first as replay frames (one or several, #134), then live output, both in
    // stream order: clear once, before the first piece.
    let mut cleared = false;
    let (code, note) = loop {
        let ev = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ev) => ev,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let now = tty.size();
                if now != size {
                    size = now;
                    let _ = tx.send(Ev::Size(now.0, now.1));
                }
                continue;
            }
            Err(_) => break (1, "the background host stopped".to_string()),
        };
        match ev {
            Ev::Frame(Some(HostFrame::Data {
                pane_id: id,
                data,
                replay,
            })) if id == pane_id => {
                if replay && !cleared {
                    cleared = true;
                    tty.write(&format!("\x1b[H\x1b[2J{data}"));
                } else {
                    tty.write(&data);
                }
            }
            Ev::Frame(Some(HostFrame::Exit {
                pane_id: id,
                exit_code,
                ..
            })) if id == pane_id => {
                break (0, format!("pane exited with code {exit_code}"));
            }
            Ev::Frame(None) => break (1, "the background host stopped".into()),
            Ev::Frame(_) => {}
            Ev::Input(data) => match data.find(DETACH_KEY) {
                Some(cut) => {
                    if cut > 0 {
                        c.send(&ClientFrame::Write {
                            pane_id: pane_id.into(),
                            data: data[..cut].into(),
                        });
                    }
                    break (0, format!("detached from {pane_id}; it keeps running"));
                }
                None => c.send(&ClientFrame::Write {
                    pane_id: pane_id.into(),
                    data,
                }),
            },
            Ev::Size(cols, rows) => c.send(&ClientFrame::Resize {
                pane_id: pane_id.into(),
                cols,
                rows,
            }),
            Ev::Reply(r) => {
                if !r.ok() {
                    break (1, r.error());
                }
                if let Some(exit) = r.0.get("exit").filter(|e| !e.is_null()) {
                    break (0, format!("pane exited with code {}", exit["exitCode"]));
                }
            }
        }
    };
    tty.raw(false);
    tty.write(&format!("{TTY_RESET}\r\n[{note}]\r\n"));
    c.close();
    Ok(code)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::parse_config;
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;

    /// A host that answers `list` with one running pane and records what it was sent.
    fn fake_host(sent: Arc<Mutex<Vec<Value>>>) -> (Reader, Writer) {
        let (client, server) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let mut out = server.try_clone().unwrap();
            for line in std::io::BufReader::new(server)
                .lines()
                .map_while(Result::ok)
            {
                let v: Value = serde_json::from_str(&line).unwrap();
                sent.lock().unwrap().push(v.clone());
                let reply = match v["t"].as_str().unwrap() {
                    "hello" => {
                        json!({ "t": "welcome", "protocol": 1, "appVersion": "x", "pid": 1 })
                    }
                    "list" => {
                        json!({ "t": "reply", "id": v["id"], "reply": { "ok": true, "panes": [
                        { "paneId": "p1", "pid": 42, "exit": null, "state": "idle" },
                        { "paneId": "orphan", "pid": 7, "exit": null, "state": "running" }
                    ] } })
                    }
                    "read" => {
                        json!({ "t": "reply", "id": v["id"], "reply": { "ok": true, "text": "screen" } })
                    }
                    "wait" => {
                        json!({ "t": "reply", "id": v["id"], "reply": { "ok": true, "state": "idle" } })
                    }
                    "gui" => {
                        json!({ "t": "reply", "id": v["id"], "reply": { "ok": true, "result": { "paneId": "p1" } } })
                    }
                    _ => continue,
                };
                writeln!(out, "{reply}").unwrap();
            }
        });
        (Box::new(client.try_clone().unwrap()), Box::new(client))
    }

    fn config() -> Config {
        parse_config(&json!({
            "version": 1,
            "accounts": [{ "id": "a1", "name": "Work", "configDir": "~/a", "color": "#7c5cff" }],
            "workspaces": [{ "id": "w", "name": "proj", "panes": [{ "id": "p1", "accountId": "a1", "cwd": "~", "title": "builder" }], "layout": null }],
            "activeWorkspace": "w"
        }), true).unwrap()
    }

    fn run_with(args: &[&str]) -> (i32, String, String, Vec<Value>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (out, err) = (
            Arc::new(Mutex::new(String::new())),
            Arc::new(Mutex::new(String::new())),
        );
        let (o, e, s) = (out.clone(), err.clone(), sent.clone());
        let deps = CliDeps {
            connect: Box::new(move || Ok(fake_host(s.clone()))),
            read_token: Box::new(|| Ok("t".into())),
            read_config: Box::new(|| Some(config())),
            app_version: "2.0.0".into(),
            cwd: "/work".into(),
            out: Box::new(move |t| o.lock().unwrap().push_str(t)),
            err: Box::new(move |t| e.lock().unwrap().push_str(t)),
            sleep: Box::new(|_| {}),
            tty: None,
        };
        let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let code = run_cli(&argv, deps);
        std::thread::sleep(Duration::from_millis(50));
        let sent = sent.lock().unwrap().clone();
        let (out, err) = (out.lock().unwrap().clone(), err.lock().unwrap().clone());
        (code, out, err, sent)
    }

    #[test]
    fn list_joins_config_and_host() {
        let (code, out, _, sent) = run_with(&["list"]);
        assert_eq!(code, 0);
        assert!(
            out.starts_with("ID      TITLE    WORKSPACE  ACCOUNT  CLI     STATE    PID"),
            "{out}"
        );
        assert!(out.contains("p1      builder  proj       Work     claude  idle     42"));
        assert!(out.contains("orphan"));
        assert_eq!(sent[0]["role"], "cli");
    }

    #[test]
    fn send_uses_the_new_line_key_then_enter() {
        let (code, _, _, sent) = run_with(&["send", "builder", "line1\nline2"]);
        assert_eq!(code, 0);
        let writes: Vec<&Value> = sent.iter().filter(|v| v["t"] == "write").collect();
        assert_eq!(writes[0]["data"], "line1\x1b\rline2");
        assert_eq!(writes[1]["data"], "\r");
        assert!(sent
            .iter()
            .any(|v| v["t"] == "gui" && v["action"]["kind"] == "sent"));
    }

    #[test]
    fn usage_and_failures() {
        let (code, _, err, _) = run_with(&["read"]);
        assert_eq!(code, 2);
        assert!(err.starts_with("wraithgrid: read needs a pane\n\nUsage:"));
        let (code, _, err, _) = run_with(&["read", "nope"]);
        assert_eq!(code, 1);
        assert_eq!(
            err,
            "wraithgrid: No pane \"nope\" (wraithgrid list shows them)."
        );
        let (code, _, err, _) = run_with(&["wait", "p1", "--until", "soon"]);
        assert_eq!(code, 2);
        assert!(err.contains("--until is one of running, idle, blocked, exited"));
        let (code, out, _, _) = run_with(&["help"]);
        assert_eq!((code, out.starts_with("Usage: wraithgrid")), (0, true));
    }

    #[test]
    fn new_resolves_cwd_and_waits_for_the_pane() {
        let (code, out, _, sent) = run_with(&["new", "--cwd", "sub", "--shell", "--", "-x"]);
        assert_eq!((code, out.as_str()), (0, "p1"));
        let gui = sent.iter().find(|v| v["t"] == "gui").unwrap();
        assert_eq!(
            gui["action"],
            json!({ "kind": "new", "cwd": "/work/sub", "shell": true, "args": ["-x"] })
        );
    }

    #[test]
    fn no_host() {
        let deps = CliDeps {
            connect: Box::new(|| Err(std::io::Error::other("nope"))),
            read_token: Box::new(|| Ok("t".into())),
            read_config: Box::new(|| None),
            app_version: "x".into(),
            cwd: "/".into(),
            out: Box::new(|_| {}),
            err: Box::new(|e| assert!(e.contains("not running any agents"))),
            sleep: Box::new(|_| {}),
            tty: None,
        };
        assert_eq!(run_cli(&["list".into()], deps), 1);
    }
}
