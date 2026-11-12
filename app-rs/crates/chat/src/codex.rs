//! codex's app-server for the chat view (#40), ported from `src/main/chat/codex.ts`: JSON-RPC
//! 2.0, one message a line, over stdio (`codex app-server`, experimental in codex-cli 0.163.0).
//! Chosen over `codex exec --json` because one process holds the conversation, stops a turn
//! (`turn/interrupt`) and asks before commands and edits (server requests answered with a
//! decision). Shapes checked against real output (tests/fixtures/codex-chat.ndjson). Model,
//! effort and mode go with each `turn/start`, so they switch without a restart.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Value};

use crate::adapter::{ArgsOptions, ChatAdapter, Decoded};
use crate::attach::{chat_images_dir, save_chat_images};
use crate::json::{get, js_string, now_ms, objects, s};
use crate::types::{
    clip, take_chars, AccountLimits, ApprovalDecision, ChatAccountInfo, ChatAnswers, ChatCommand,
    ChatControls, ChatEvent, ChatImage, ChatMode, ChatModel, LiveControls, NoticeLevel, ToolKind,
    ToolPreview, TurnUsage, UsageWindow, CHAT_EFFORTS, CLIP_MAX,
};

/// The modes codex has, as approval policy + sandbox (its Read only / Auto / Full access).
pub const CODEX_MODES: [ChatMode; 4] = [
    ChatMode::Default,
    ChatMode::AcceptEdits,
    ChatMode::Plan,
    ChatMode::BypassPermissions,
];

/// A mode as codex runs it: the approval policy, and the sandbox in both of codex's spellings
/// (`thread/start` takes the name, `turn/start` the policy object).
struct Policy {
    approval_policy: &'static str,
    sandbox: &'static str,
    sandbox_policy: Value,
}

fn policy_of(mode: ChatMode) -> Policy {
    let workspace =
        || json!({ "type": "workspaceWrite", "writableRoots": [], "networkAccess": false });
    match mode {
        // Works in the folder on its own; asks to go beyond it.
        ChatMode::AcceptEdits => Policy {
            approval_policy: "on-request",
            sandbox: "workspace-write",
            sandbox_policy: workspace(),
        },
        ChatMode::Plan => Policy {
            approval_policy: "on-request",
            sandbox: "read-only",
            sandbox_policy: json!({ "type": "readOnly", "networkAccess": false }),
        },
        ChatMode::BypassPermissions => Policy {
            approval_policy: "never",
            sandbox: "danger-full-access",
            sandbox_policy: json!({ "type": "dangerFullAccess" }),
        },
        // Asks before anything but trusted reads.
        _ => Policy {
            approval_policy: "untrusted",
            sandbox: "workspace-write",
            sandbox_policy: workspace(),
        },
    }
}

/// A mode codex has, else its default.
fn codex_mode(mode: Option<ChatMode>) -> ChatMode {
    mode.filter(|m| CODEX_MODES.contains(m))
        .unwrap_or(ChatMode::Default)
}

/// A picked model, or None for codex's own default.
fn model_choice(model: Option<&str>) -> Option<String> {
    model
        .filter(|m| !m.is_empty() && *m != "default")
        .map(String::from)
}

/// `a || b` for strings.
fn or<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.is_empty() {
        b
    } else {
        a
    }
}

fn some(v: &str) -> Option<String> {
    (!v.is_empty()).then(|| v.to_string())
}

/// `model/list` → the picker's models (hidden ones left out; effort levels we know).
pub fn parse_codex_models(result: &Value) -> Vec<ChatModel> {
    objects(result, "data")
        .into_iter()
        .filter(|m| get(m, "hidden") != &Value::Bool(true) && !s(m, "id").is_empty())
        .take(100)
        .map(|m| ChatModel {
            value: s(m, "id").to_string(),
            label: or(s(m, "displayName"), s(m, "id")).to_string(),
            description: take_chars(s(m, "description"), 400).to_string(),
            efforts: objects(m, "supportedReasoningEfforts")
                .into_iter()
                .map(|e| s(e, "reasoningEffort"))
                .filter(|e| CHAT_EFFORTS.contains(e))
                .map(String::from)
                .collect(),
            auto_mode: false,
        })
        .collect()
}

/// A skill codex found (`skills/list`), for the chat box's `$` menu and the turn's input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSkill {
    pub name: String,
    pub path: String,
    pub description: String,
}

/// Runs of whitespace as one space (`.replace(/\s+/g, ' ')`).
fn squash_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}

/// `skills/list` → the enabled skills, one per name (the first cwd entry wins).
pub fn parse_codex_skills(result: &Value) -> Vec<CodexSkill> {
    let mut seen: Vec<CodexSkill> = Vec::new();
    for e in objects(result, "data") {
        for sk in objects(e, "skills") {
            let name = s(sk, "name");
            if name.is_empty()
                || s(sk, "path").is_empty()
                || get(sk, "enabled") == &Value::Bool(false)
                || seen.iter().any(|x| x.name == name)
            {
                continue;
            }
            let ui = get(sk, "interface");
            let description = or(
                or(s(ui, "shortDescription"), s(sk, "shortDescription")),
                s(sk, "description"),
            );
            seen.push(CodexSkill {
                name: take_chars(name, 100).to_string(),
                path: s(sk, "path").to_string(),
                description: take_chars(&squash_spaces(description), 400).to_string(),
            });
        }
    }
    seen.truncate(300);
    seen
}

static SKILL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)\$([A-Za-z0-9_.:-]+)").expect("valid regex"));

/// The skills a message names as `$name`, as codex's `skill` inputs (how its own TUI sends them).
pub fn skill_inputs(text: &str, skills: &[CodexSkill]) -> Vec<Value> {
    let named: HashSet<&str> = SKILL_NAME
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    skills
        .iter()
        .filter(|sk| named.contains(sk.name.as_str()))
        .map(|sk| json!({ "type": "skill", "name": sk.name, "path": sk.path }))
        .collect()
}

/// codex's rate limits → the usage windows (primary = 5 hours, secondary = a week).
fn limits_of(snapshot: &Value) -> Option<ChatEvent> {
    if !snapshot.is_object() {
        return None;
    }
    let win = |w: &Value| match (get(w, "usedPercent").as_f64(), get(w, "resetsAt").as_f64()) {
        (Some(used), Some(resets)) => Some(UsageWindow {
            utilization: (used / 100.0).max(0.0),
            resets_at: resets * 1000.0,
        }),
        _ => None,
    };
    let five_hour = win(get(snapshot, "primary"));
    let seven_day = win(get(snapshot, "secondary"));
    (five_hour.is_some() || seven_day.is_some()).then(|| ChatEvent::Limits {
        limits: AccountLimits {
            five_hour,
            seven_day,
            at: now_ms(),
        },
    })
}

/// codex's token counts (`thread/tokenUsage/updated`); input includes the cached part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CodexTokens {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
}

fn tokens_of(u: &Value) -> Option<CodexTokens> {
    if !u.is_object() {
        return None;
    }
    let n = |key: &str| match get(u, key).as_f64() {
        Some(v) if v > 0.0 => v as u64,
        _ => 0,
    };
    Some(CodexTokens {
        input: n("inputTokens"),
        cached: n("cachedInputTokens"),
        cache_write: n("cacheWriteInputTokens"),
        output: n("outputTokens"),
    })
}

fn minus(a: CodexTokens, b: CodexTokens) -> CodexTokens {
    CodexTokens {
        input: a.input.saturating_sub(b.input),
        cached: a.cached.saturating_sub(b.cached),
        cache_write: a.cache_write.saturating_sub(b.cache_write),
        output: a.output.saturating_sub(b.output),
    }
}

/// `bash -lc '…'` as codex runs commands → the command itself.
fn command_of(item: &Value) -> String {
    let joined = objects(item, "commandActions")
        .into_iter()
        .map(|a| s(a, "command"))
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>()
        .join(" && ");
    if joined.is_empty() {
        s(item, "command").to_string()
    } else {
        joined
    }
}

/// How a codex item is drawn as a tool card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexToolDescription {
    pub name: String,
    pub kind: ToolKind,
    pub subtitle: String,
    pub preview: Option<ToolPreview>,
}

/// One codex item drawn as a tool card, or None when it isn't one.
pub fn describe_codex_item(item: &Value) -> Option<CodexToolDescription> {
    let d = |name: &str, kind, subtitle: &str, preview| {
        Some(CodexToolDescription {
            name: name.to_string(),
            kind,
            subtitle: subtitle.to_string(),
            preview,
        })
    };
    match s(item, "type") {
        "commandExecution" => {
            let cmd = command_of(item);
            d(
                "Shell",
                ToolKind::Shell,
                &cmd,
                Some(ToolPreview::Code { text: cmd.clone() }),
            )
        }
        "fileChange" => {
            let changes = objects(item, "changes");
            let diff = changes
                .iter()
                .map(|c| s(c, "diff"))
                .collect::<Vec<_>>()
                .join("\n");
            let paths = changes
                .iter()
                .map(|c| s(c, "path"))
                .collect::<Vec<_>>()
                .join(", ");
            let preview = (!diff.is_empty()).then(|| ToolPreview::Code {
                text: clip(&diff, 20_000),
            });
            d("Edit", ToolKind::Edit, &paths, preview)
        }
        "mcpToolCall" => d(
            &format!("{}.{}", s(item, "server"), s(item, "tool")),
            ToolKind::Other,
            "",
            None,
        ),
        "dynamicToolCall" => d(s(item, "tool"), ToolKind::Other, "", None),
        "webSearch" => d("Web search", ToolKind::Web, s(item, "query"), None),
        "collabAgentToolCall" => d("Agent", ToolKind::Agent, s(item, "prompt"), None),
        "imageView" => d("View image", ToolKind::Read, s(item, "path"), None),
        _ => None,
    }
}

/// A finished tool item → its card's output and whether it failed.
fn result_of(item: &Value) -> (String, bool) {
    let status = s(item, "status");
    let failed = status == "failed" || status == "declined";
    match s(item, "type") {
        "commandExecution" => {
            let code = item.get("exitCode").filter(|c| c.is_number());
            let out = s(item, "aggregatedOutput");
            let output = if !out.is_empty() {
                out.to_string()
            } else if status == "declined" {
                "Declined".to_string()
            } else if let Some(c) = code {
                format!("Exit code {}", js_string(Some(c)))
            } else {
                String::new()
            };
            let nonzero = code.is_some_and(|c| c.as_f64() != Some(0.0));
            (output, failed || nonzero)
        }
        "mcpToolCall" => {
            let err = s(get(item, "error"), "message");
            if !err.is_empty() {
                return (err.to_string(), true);
            }
            // JSON.stringify(item.result ?? '').
            let json = match item.get("result") {
                None | Some(Value::Null) => "\"\"".to_string(),
                Some(v) => serde_json::to_string(v).unwrap_or_default(),
            };
            (take_chars(&json, 8000).to_string(), false)
        }
        _ => {
            let output = match (failed, status == "declined") {
                (false, _) => "",
                (true, true) => "Declined",
                (true, false) => "Failed",
            };
            (output.to_string(), failed)
        }
    }
}

/// How the chat was started; model and mode change as the user picks them (they go with the
/// next turn).
#[derive(Debug, Clone)]
struct Start {
    session_id: Option<String>,
    continue_last: bool,
    model: Option<String>,
    effort: Option<String>,
    mode: ChatMode,
}

/// An approval codex waits on: its request id and method.
struct PendingRequest {
    id: Value,
    method: String,
}

/// The codex adapter for one process.
pub struct CodexAdapter {
    seq: u64,
    /// What our requests were, by id, for their answers.
    asked: HashMap<u64, &'static str>,
    start: Start,
    thread: Option<String>,
    /// The running turn (for interrupt), and messages added to it while it ran.
    turn: Option<String>,
    steered: usize,
    stop_asked: bool,
    /// The thread's token total when this turn began, and the latest one (#53). `last` is per
    /// model call and a turn can repeat it, so a turn's use is the change in `total`.
    turn_base: Option<CodexTokens>,
    latest: Option<CodexTokens>,
    /// Messages (their inputs) sent before the thread was ready.
    waiting: Vec<Vec<Value>>,
    /// Items that already streamed their text, so the full copy isn't shown twice.
    streamed: HashSet<String>,
    /// Approvals codex waits on, in the order asked: our id → its request.
    pending: Vec<(String, PendingRequest)>,
    /// What codex offers, kept together: each answer fills in its half.
    models: Vec<ChatModel>,
    account: Option<ChatAccountInfo>,
    skills: Vec<CodexSkill>,
    /// Tool items seen, so an approval can show what it is about.
    items: HashMap<String, Value>,
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexAdapter {
    pub fn new() -> Self {
        CodexAdapter {
            seq: 0,
            asked: HashMap::new(),
            start: Start {
                session_id: None,
                continue_last: false,
                model: None,
                effort: None,
                mode: ChatMode::Default,
            },
            thread: None,
            turn: None,
            steered: 0,
            stop_asked: false,
            turn_base: None,
            latest: None,
            waiting: Vec::new(),
            streamed: HashSet::new(),
            pending: Vec::new(),
            models: Vec::new(),
            account: None,
            skills: Vec::new(),
            items: HashMap::new(),
        }
    }

    /// A request of ours, remembered by id for its answer.
    fn request(&mut self, method: &'static str, params: Value) -> Value {
        self.seq += 1;
        self.asked.insert(self.seq, method);
        json!({ "jsonrpc": "2.0", "id": self.seq, "method": method, "params": params })
    }

    fn turn_start(&mut self, input: Vec<Value>) -> Value {
        let p = policy_of(self.start.mode);
        let params = json!({
            "threadId": self.thread,
            "input": input,
            "model": self.start.model,
            "effort": self.start.effort,
            "approvalPolicy": p.approval_policy,
            "sandboxPolicy": p.sandbox_policy,
        });
        self.request("turn/start", params)
    }

    /// `thread/start`'s params, and `thread/resume`'s after `threadId`.
    fn thread_params(&self, thread_id: Option<&str>) -> Value {
        let p = policy_of(self.start.mode);
        let mut params = serde_json::Map::new();
        if let Some(id) = thread_id {
            params.insert("threadId".into(), id.into());
        }
        params.insert("model".into(), json!(self.start.model));
        params.insert("approvalPolicy".into(), p.approval_policy.into());
        params.insert("sandbox".into(), p.sandbox.into());
        Value::Object(params)
    }

    fn commands(&self) -> Vec<ChatCommand> {
        self.skills
            .iter()
            .map(|sk| ChatCommand {
                name: sk.name.clone(),
                description: sk.description.clone(),
                argument_hint: String::new(),
                aliases: Vec::new(),
                builtin: false,
                mention: true,
            })
            .collect()
    }

    fn capabilities(&self) -> ChatEvent {
        ChatEvent::Capabilities {
            models: self.models.clone(),
            account: self.account.clone(),
            commands: Some(self.commands()),
        }
    }

    /// An answer to one of our requests.
    fn answered(&mut self, msg: &Value, out: &mut Decoded) {
        let what = get(msg, "id")
            .as_u64()
            .and_then(|id| self.asked.remove(&id))
            .unwrap_or("");
        let r = get(msg, "result");
        let error = get(msg, "error");
        if error.is_object() {
            let text = or(s(error, "message"), "codex refused a request.").to_string();
            match what {
                "thread/resume" => {
                    // A conversation codex no longer has: start a new one rather than fail.
                    out.events.push(ChatEvent::Notice {
                        level: NoticeLevel::Info,
                        text: format!("Could not resume ({text}); started a new conversation."),
                    });
                    let params = self.thread_params(None);
                    out.replies.push(self.request("thread/start", params));
                }
                "turn/start" => {
                    // A Stop asked for this turn ends with it: the next turn must not be cut
                    // short (#140).
                    self.stop_asked = false;
                    out.events.push(ChatEvent::Notice {
                        level: NoticeLevel::Error,
                        text,
                    });
                    out.events.push(ChatEvent::Busy { busy: false });
                }
                "account/rateLimits/read" | "model/list" | "skills/list" => {}
                _ => out.events.push(ChatEvent::Notice {
                    level: NoticeLevel::Error,
                    text,
                }),
            }
            return;
        }
        match what {
            "initialize" => {
                out.replies
                    .push(json!({ "jsonrpc": "2.0", "method": "initialized" }));
                let first = if let Some(id) = self.start.session_id.clone() {
                    let params = self.thread_params(Some(&id));
                    self.request("thread/resume", params)
                } else if self.start.continue_last {
                    self.request("thread/list", json!({ "limit": 1, "cwd": null }))
                } else {
                    let params = self.thread_params(None);
                    self.request("thread/start", params)
                };
                out.replies.push(first);
                out.replies.push(self.request("model/list", json!({})));
                out.replies.push(self.request("account/read", json!({})));
                out.replies
                    .push(self.request("account/rateLimits/read", Value::Null));
                out.replies.push(self.request("skills/list", json!({})));
            }
            "thread/list" => {
                let last = objects(r, "data")
                    .first()
                    .map(|l| s(l, "id").to_string())
                    .unwrap_or_default();
                let next = if last.is_empty() {
                    let params = self.thread_params(None);
                    self.request("thread/start", params)
                } else {
                    let params = self.thread_params(Some(&last));
                    self.request("thread/resume", params)
                };
                out.replies.push(next);
            }
            "thread/start" | "thread/resume" => {
                self.thread = some(s(get(r, "thread"), "id"));
                if let Some(id) = &self.thread {
                    out.events.push(ChatEvent::Session {
                        id: id.clone(),
                        model: some(s(r, "model")),
                        terminal_only: None,
                    });
                }
                out.events.push(ChatEvent::Controls {
                    controls: ChatControls {
                        live: LiveControls {
                            interrupt: true,
                            model: true,
                            mode: true,
                        },
                        modes: CODEX_MODES.to_vec(),
                        approvals: true,
                    },
                });
                for input in std::mem::take(&mut self.waiting) {
                    let line = self.turn_start(input);
                    out.replies.push(line);
                }
            }
            "model/list" => {
                self.models = parse_codex_models(r);
                out.events.push(self.capabilities());
            }
            "account/read" => {
                let a = get(r, "account");
                if r.is_object()
                    && !a.is_object()
                    && get(r, "requiresOpenaiAuth") == &Value::Bool(true)
                {
                    out.events.push(ChatEvent::Notice {
                        level: NoticeLevel::Error,
                        text: "codex is not signed in for this account. Use Login on the Accounts page."
                            .into(),
                    });
                }
                if a.is_object() {
                    let plan = if s(a, "planType").is_empty() {
                        some(s(a, "type"))
                    } else {
                        Some(format!("ChatGPT {}", s(a, "planType")))
                    };
                    self.account = Some(ChatAccountInfo {
                        email: some(s(a, "email")),
                        plan,
                    });
                    // The list and the account come in either order: say both each time.
                    out.events.push(self.capabilities());
                }
            }
            "skills/list" => {
                self.skills = parse_codex_skills(r);
                out.events.push(self.capabilities());
            }
            "account/rateLimits/read" => {
                if let Some(l) = limits_of(get(r, "rateLimits")) {
                    out.events.push(l);
                }
            }
            _ => {}
        }
    }

    /// A request codex waits on.
    fn server_request(&mut self, method: &str, msg: &Value, out: &mut Decoded) {
        let p = get(msg, "params");
        let id = get(msg, "id").clone();
        let item_id = s(p, "itemId");
        let tool_id = some(item_id);
        let req_id = format!("cx-{}", js_string(Some(&id)));
        match method {
            "item/commandExecution/requestApproval" | "execCommandApproval" => {
                let mut cmd = command_of(p);
                if cmd.is_empty() {
                    if let Value::Array(parts) = get(p, "command") {
                        cmd = parts
                            .iter()
                            .map(|x| x.as_str().unwrap_or(""))
                            .collect::<Vec<_>>()
                            .join(" ");
                    }
                }
                self.pending.push((
                    req_id.clone(),
                    PendingRequest {
                        id,
                        method: method.to_string(),
                    },
                ));
                out.events.push(ChatEvent::Approval {
                    req_id,
                    tool_id,
                    name: "Shell".into(),
                    subtitle: or(s(p, "reason"), &cmd).to_string(),
                    preview: (!cmd.is_empty()).then(|| ToolPreview::Code { text: cmd.clone() }),
                    can_always: method == "item/commandExecution/requestApproval",
                    questions: None,
                });
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                self.pending.push((
                    req_id.clone(),
                    PendingRequest {
                        id,
                        method: method.to_string(),
                    },
                ));
                let shown = self.items.get(item_id).and_then(describe_codex_item);
                let subtitle = or(
                    or(
                        shown.as_ref().map_or("", |d| d.subtitle.as_str()),
                        s(p, "reason"),
                    ),
                    "Change files",
                )
                .to_string();
                out.events.push(ChatEvent::Approval {
                    req_id,
                    tool_id,
                    name: "Edit".into(),
                    subtitle,
                    preview: shown.and_then(|d| d.preview),
                    can_always: method == "item/fileChange/requestApproval",
                    questions: None,
                });
            }
            _ => {
                // Questions and tool calls meant for a richer host: say no, or codex waits for
                // ever.
                out.replies.push(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": "Not supported by Wraithgrid" }
                }));
            }
        }
    }

    /// A notification: something happened in the thread.
    fn notification(&mut self, method: &str, msg: &Value, out: &mut Decoded) {
        let p = get(msg, "params");
        let push = &mut out.events;
        match method {
            "turn/started" => {
                self.turn = some(s(get(p, "turn"), "id"));
                if self.stop_asked {
                    if let Some(turn) = self.turn.clone() {
                        self.stop_asked = false;
                        let params = json!({ "threadId": self.thread, "turnId": turn });
                        out.replies.push(self.request("turn/interrupt", params));
                    }
                }
            }
            "item/agentMessage/delta" => {
                self.streamed.insert(s(p, "itemId").to_string());
                push.push(ChatEvent::Text {
                    id: s(p, "itemId").to_string(),
                    delta: s(p, "delta").to_string(),
                });
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                push.push(ChatEvent::Thinking {
                    id: s(p, "itemId").to_string(),
                    delta: s(p, "delta").to_string(),
                });
            }
            "item/started" | "item/completed" => {
                let item = get(p, "item");
                let id = s(item, "id").to_string();
                if s(item, "type") == "agentMessage" {
                    if method == "item/completed"
                        && !self.streamed.remove(&id)
                        && !s(item, "text").is_empty()
                    {
                        push.push(ChatEvent::Text {
                            id,
                            delta: s(item, "text").to_string(),
                        });
                    }
                    return;
                }
                let Some(tool) = describe_codex_item(item) else {
                    return;
                };
                self.items.insert(id.clone(), item.clone());
                push.push(ChatEvent::Tool {
                    id: id.clone(),
                    name: tool.name,
                    kind: tool.kind,
                    subtitle: tool.subtitle,
                    preview: tool.preview,
                });
                if method == "item/completed" {
                    self.items.remove(&id);
                    let (output, is_error) = result_of(item);
                    push.push(ChatEvent::ToolResult {
                        id,
                        output: clip(&output, CLIP_MAX),
                        is_error,
                    });
                }
            }
            "serverRequest/resolved" => {
                // Answered elsewhere, or no longer wanted: the bar must go.
                let resolved = js_string(p.get("requestId"));
                let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
                    .into_iter()
                    .partition(|(_, q)| js_string(Some(&q.id)) == resolved);
                self.pending = kept;
                for (req_id, _) in gone {
                    push.push(ChatEvent::ApprovalDone {
                        req_id,
                        decision: ApprovalDecision::Deny,
                        answers: None,
                    });
                }
            }
            "thread/tokenUsage/updated" => {
                let u = get(p, "tokenUsage");
                let Some(total) = tokens_of(get(u, "total")) else {
                    return;
                };
                // First count seen (a resumed thread's total holds its history): start before
                // it.
                if self.latest.is_none() {
                    self.turn_base = Some(minus(total, tokens_of(get(u, "last")).unwrap_or(total)));
                }
                self.latest = Some(total);
            }
            "account/rateLimits/updated" => {
                if let Some(l) = limits_of(get(p, "rateLimits")) {
                    push.push(l);
                }
            }
            "error" => {
                let text = s(get(p, "error"), "message");
                if get(p, "willRetry") != &Value::Bool(true) && !text.is_empty() {
                    push.push(ChatEvent::Notice {
                        level: NoticeLevel::Error,
                        text: text.to_string(),
                    });
                }
            }
            "skills/changed" => {
                out.replies
                    .push(self.request("skills/list", json!({ "forceReload": true })));
            }
            "thread/compacted" => push.push(ChatEvent::Notice {
                level: NoticeLevel::Info,
                text: "Context compacted".into(),
            }),
            "turn/completed" => {
                let t = get(p, "turn");
                let status = s(t, "status");
                self.turn = None;
                self.stop_asked = false;
                if status == "interrupted" {
                    push.push(ChatEvent::Notice {
                        level: NoticeLevel::Info,
                        text: "Interrupted".into(),
                    });
                }
                let used = match (self.latest, self.turn_base) {
                    (Some(latest), Some(base)) => Some(minus(latest, base)),
                    _ => None,
                };
                self.turn_base = self.latest;
                let error = (status == "failed")
                    .then(|| or(s(get(t, "error"), "message"), "The turn failed.").to_string());
                push.push(ChatEvent::TurnEnd {
                    cost_usd: None,
                    duration_ms: get(t, "durationMs").as_f64(),
                    error,
                    // codex reports tokens, never a cost (#53).
                    usage: used.map(|u| TurnUsage {
                        model: self.start.model.clone(),
                        cost_usd: None,
                        input: u.input.saturating_sub(u.cached),
                        output: u.output,
                        cache_read: u.cached,
                        cache_write: u.cache_write,
                    }),
                });
                // Messages added to a running turn end with it.
                let ends = 1 + std::mem::take(&mut self.steered);
                for _ in 0..ends {
                    push.push(ChatEvent::Busy { busy: false });
                }
            }
            _ => {}
        }
    }
}

impl ChatAdapter for CodexAdapter {
    fn config_dir_var(&self) -> Option<&'static str> {
        Some("CODEX_HOME")
    }

    fn args(&mut self, o: &ArgsOptions) -> Vec<String> {
        self.start = Start {
            session_id: o.session_id.clone(),
            continue_last: o.continue_last,
            model: model_choice(o.model.as_deref()),
            effort: o.effort.clone(),
            mode: codex_mode(o.mode),
        };
        vec!["app-server".into()]
    }

    fn hello(&mut self) -> Vec<Value> {
        let params = json!({
            "clientInfo": { "name": "wraithgrid", "title": "Wraithgrid", "version": "1" },
            "capabilities": null
        });
        vec![self.request("initialize", params)]
    }

    fn user(&mut self, text: &str, images: &[ChatImage]) -> Option<Value> {
        let mut input = Vec::new();
        if !text.is_empty() {
            input.push(json!({ "type": "text", "text": text, "text_elements": [] }));
        }
        input.extend(skill_inputs(text, &self.skills));
        // An image that can't be written is left out rather than failing the message.
        let paths = save_chat_images(images, &chat_images_dir(), now_ms()).unwrap_or_default();
        input.extend(
            paths
                .iter()
                .map(|p| json!({ "type": "localImage", "path": p.to_string_lossy() })),
        );
        if self.thread.is_none() {
            self.waiting.push(input);
            return None;
        }
        // A message while a turn runs joins that turn (codex steers it).
        if self.turn.is_some() {
            self.steered += 1;
        }
        Some(self.turn_start(input))
    }

    fn interrupt(&mut self) -> Option<Value> {
        if let (Some(thread), Some(turn)) = (&self.thread, &self.turn) {
            let params = json!({ "threadId": thread, "turnId": turn });
            return Some(self.request("turn/interrupt", params));
        }
        // Not started yet: stop it as soon as it is.
        self.stop_asked = true;
        None
    }

    fn set_model(&mut self, model: &str) -> Option<Value> {
        self.start.model = model_choice(Some(model));
        None
    }

    fn set_mode(&mut self, mode: ChatMode) -> Option<Value> {
        self.start.mode = codex_mode(Some(mode));
        None
    }

    fn answer(
        &mut self,
        req_id: &str,
        decision: ApprovalDecision,
        _answers: Option<&ChatAnswers>,
    ) -> Option<Value> {
        let at = self.pending.iter().position(|(id, _)| id == req_id)?;
        let (_, q) = self.pending.remove(at);
        let legacy = q.method == "execCommandApproval" || q.method == "applyPatchApproval";
        let value = match (legacy, decision) {
            (true, ApprovalDecision::Deny) => "denied",
            (true, ApprovalDecision::Always) => "approved_for_session",
            (true, ApprovalDecision::Allow) => "approved",
            (false, ApprovalDecision::Deny) => "decline",
            (false, ApprovalDecision::Always) => "acceptForSession",
            (false, ApprovalDecision::Allow) => "accept",
        };
        Some(json!({ "jsonrpc": "2.0", "id": q.id, "result": { "decision": value } }))
    }

    fn decode(&mut self, msg: &Value) -> Decoded {
        let mut out = Decoded::default();
        if !msg.is_object() {
            return out;
        }
        let method = s(msg, "method").to_string();
        let has_id = msg.get("id").is_some();
        if method.is_empty() && has_id {
            // ---- Answers to our requests.
            self.answered(msg, &mut out);
        } else if !method.is_empty() && has_id {
            // ---- Requests codex waits on.
            self.server_request(&method, msg, &mut out);
        } else {
            // ---- Notifications.
            self.notification(&method, msg, &mut out);
        }
        out
    }
}
