//! claude's headless mode for the chat view, ported from `src/main/chat/claude.ts`: `-p` with
//! stream-json in and out, and tool approvals answered over the same pipe
//! (`--permission-prompt-tool stdio`, the Agent SDK's control protocol). Shapes checked against
//! claude 2.1.306 output (tests/fixtures/claude-chat.ndjson). Decoding is pure: a line in,
//! events and replies out, so it is tested without a process.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

use crate::adapter::{ArgsOptions, ChatAdapter, Decoded};
use crate::json::{first_line, get, js_string, now_ms, obj, objects, random_token, s, str_of};
use crate::json::{to_base36, truthy};
use crate::types::{
    clip, take_chars, AccountLimits, ApprovalDecision, BackgroundKind, ChatAccountInfo,
    ChatAnswers, ChatBackgroundTask, ChatCommand, ChatEvent, ChatImage, ChatMode, ChatModel,
    ChatQuestion, ChatQuestionOption, NoticeLevel, TodoItem, TodoStatus, ToolKind, ToolPreview,
    TurnUsage, UsageWindow, CLIP_MAX,
};

/// How a claude tool call looks in the chat: its kind, one line, and a preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDescription {
    pub kind: ToolKind,
    pub subtitle: String,
    pub preview: Option<ToolPreview>,
}

/// AskUserQuestion's questions (claude 2.1: up to 4, each with 2–4 options).
pub fn questions_of(input: &Value) -> Vec<ChatQuestion> {
    objects(input, "questions")
        .into_iter()
        .take(4)
        .map(|q| ChatQuestion {
            question: take_chars(s(q, "question"), 2000).to_string(),
            header: take_chars(s(q, "header"), 40).to_string(),
            multi_select: get(q, "multiSelect") == &Value::Bool(true),
            options: objects(q, "options")
                .into_iter()
                .take(8)
                .map(|o| ChatQuestionOption {
                    label: take_chars(s(o, "label"), 200).to_string(),
                    description: take_chars(s(o, "description"), 600).to_string(),
                })
                .filter(|o| !o.label.is_empty())
                .collect(),
        })
        .filter(|q| !q.question.is_empty())
        .collect()
}

/// `a || b` for strings.
fn or<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.is_empty() {
        b
    } else {
        a
    }
}

fn code(text: &str, max: usize) -> Option<ToolPreview> {
    (!text.is_empty()).then(|| ToolPreview::Code {
        text: clip(text, max),
    })
}

/// How a claude tool call is drawn. `input` is the tool's input as claude sent it (Null when
/// it sent none).
pub fn describe_claude_tool(name: &str, input: &Value) -> ToolDescription {
    let empty = Value::Object(Map::new());
    let i = if input.is_object() { input } else { &empty };
    let path = or(or(s(i, "file_path"), s(i, "notebook_path")), s(i, "path")).to_string();
    let d = |kind, subtitle: &str, preview| ToolDescription {
        kind,
        subtitle: subtitle.to_string(),
        preview,
    };
    let diff = |before: String, after: String| {
        Some(ToolPreview::Diff {
            path: path.clone(),
            before: clip(&before, 6000),
            after: clip(&after, 6000),
        })
    };
    match name {
        "Bash" => d(
            ToolKind::Shell,
            or(s(i, "description"), first_line(s(i, "command"))),
            code(s(i, "command"), 4000),
        ),
        "Edit" => d(
            ToolKind::Edit,
            &path,
            diff(s(i, "old_string").into(), s(i, "new_string").into()),
        ),
        "MultiEdit" => {
            let edits = objects(i, "edits");
            let side = |key| {
                edits
                    .iter()
                    .map(|e| s(e, key))
                    .collect::<Vec<_>>()
                    .join("\n…\n")
            };
            d(
                ToolKind::Edit,
                &path,
                diff(side("old_string"), side("new_string")),
            )
        }
        "Write" => d(
            ToolKind::Edit,
            &path,
            diff(String::new(), s(i, "content").into()),
        ),
        "NotebookEdit" => d(ToolKind::Edit, &path, None),
        "Read" => d(ToolKind::Read, &path, None),
        "Grep" | "Glob" => {
            let parts: Vec<&str> = [s(i, "pattern"), path.as_str()]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect();
            d(ToolKind::Search, &parts.join("  in  "), None)
        }
        "WebFetch" => d(ToolKind::Web, s(i, "url"), None),
        "WebSearch" => d(ToolKind::Web, s(i, "query"), None),
        "Task" | "Agent" => d(
            ToolKind::Agent,
            or(s(i, "description"), first_line(s(i, "prompt"))),
            code(s(i, "prompt"), 4000),
        ),
        "TodoWrite" => {
            let todos = objects(i, "todos");
            let done = todos
                .iter()
                .filter(|t| s(t, "status") == "completed")
                .count();
            let items = todos
                .iter()
                .map(|t| TodoItem {
                    text: s(t, "content").to_string(),
                    status: match s(t, "status") {
                        "completed" => TodoStatus::Done,
                        "in_progress" => TodoStatus::Active,
                        _ => TodoStatus::Pending,
                    },
                })
                .collect();
            d(
                ToolKind::Todo,
                &format!("{done}/{} done", todos.len()),
                Some(ToolPreview::Todos { items }),
            )
        }
        _ => {
            let first_string = obj(i)
                .and_then(|m| m.values().find_map(|v| v.as_str()))
                .unwrap_or("");
            // JSON.stringify(undefined) is nothing at all; an empty object shows nothing either.
            let json = if input.is_null() {
                String::new()
            } else {
                serde_json::to_string_pretty(input).unwrap_or_default()
            };
            let preview = (!json.is_empty() && json != "{}").then(|| ToolPreview::Code {
                text: clip(&json, 4000),
            });
            d(ToolKind::Other, first_line(first_string), preview)
        }
    }
}

/// A tool result's text: a string, or the text blocks of a content list.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(list) => list
            .iter()
            .filter(|b| b.is_object())
            .map(|b| match s(b, "type") {
                "text" => s(b, "text"),
                "image" => "[image]",
                _ => "",
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

const INTERRUPTED: &str = "[Request interrupted by user";

/// A command or alias claude can be typed with: a plain token (`plugin:skill`).
fn command_token(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

/// An approval request claude waits on: what it asked to run, and what "always" would save.
struct Pending {
    input: Option<Value>,
    suggestions: Vec<Value>,
}

/// The claude adapter for one process.
pub struct ClaudeAdapter {
    seq: u64,
    /// Text block ids are this process's own: a view can hold several processes' turns.
    run: String,
    pending: HashMap<String, Pending>,
    /// Messages whose text arrived as stream deltas, so the full copy is not shown twice.
    streamed: HashSet<String>,
    message_id: String,
    /// The `initialize` request: its answer lists the models and the account.
    hello_id: String,
    /// The user stopped this turn, so its error result is expected, not news.
    interrupted: bool,
    /// The mode claude last said is in force, and our switch requests it hasn't answered yet.
    mode: Option<ChatMode>,
    mode_reqs: HashSet<String>,
    /// When each background task was first seen, so its age survives list updates.
    task_since: HashMap<String, i64>,
    /// Tasks that ran in the background. claude also reports a long foreground command as a
    /// task (`is_backgrounded: false`), and its end is just the tool finishing (#51).
    backgrounded: HashSet<String>,
    now: Box<dyn Fn() -> i64 + Send>,
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self::with_clock(Box::new(now_ms))
    }

    /// With its own clock (ms), for tests.
    pub fn with_clock(now: Box<dyn Fn() -> i64 + Send>) -> Self {
        let run = format!("{}{}", to_base36(now().max(0) as u64), random_token());
        ClaudeAdapter {
            seq: 0,
            run,
            pending: HashMap::new(),
            streamed: HashSet::new(),
            message_id: String::new(),
            hello_id: String::new(),
            interrupted: false,
            mode: None,
            mode_reqs: HashSet::new(),
            task_since: HashMap::new(),
            backgrounded: HashSet::new(),
            now,
        }
    }

    fn req_id(&mut self) -> String {
        self.seq += 1;
        format!("wg-{}", self.seq)
    }

    fn system(&mut self, msg: &Value, push: &mut Vec<ChatEvent>) {
        // claude says which mode is in force at start and after every switch (`status`).
        if let Some(m) = ChatMode::parse(s(msg, "permissionMode")) {
            self.mode = Some(m);
            push.push(ChatEvent::Mode { mode: m });
        }
        let subtype = s(msg, "subtype");
        if subtype == "init" && !s(msg, "session_id").is_empty() {
            let terminal_only = match get(msg, "terminal_slash_commands") {
                Value::Array(list) => list
                    .iter()
                    .map(str_of)
                    .filter(|c| !c.is_empty())
                    .take(200)
                    .map(String::from)
                    .collect(),
                _ => Vec::new(),
            };
            let model = s(msg, "model");
            push.push(ChatEvent::Session {
                id: s(msg, "session_id").to_string(),
                model: (!model.is_empty()).then(|| model.to_string()),
                terminal_only: Some(terminal_only),
            });
        }
        // Background work (#39): claude 2.1.306 sends the whole list whenever it changes.
        if subtype == "background_tasks_changed" && get(msg, "tasks").is_array() {
            let now = (self.now)();
            let tasks: Vec<ChatBackgroundTask> = objects(msg, "tasks")
                .into_iter()
                .filter(|x| !s(x, "task_id").is_empty())
                .take(50)
                .map(|x| {
                    let id = s(x, "task_id").to_string();
                    let kind = match s(x, "task_type") {
                        "local_bash" => BackgroundKind::Shell,
                        t if t.contains("agent") => BackgroundKind::Agent,
                        _ => BackgroundKind::Other,
                    };
                    let label = take_chars(s(x, "description"), 200);
                    ChatBackgroundTask {
                        since: self.task_since.get(&id).copied().unwrap_or(now),
                        id,
                        kind,
                        label: or(label, "Background task").to_string(),
                    }
                })
                .collect();
            self.task_since.clear();
            for x in &tasks {
                self.task_since.insert(x.id.clone(), x.since);
                self.backgrounded.insert(x.id.clone());
            }
            push.push(ChatEvent::Background { tasks });
        }
        if subtype == "task_started" && get(msg, "is_backgrounded") == &Value::Bool(true) {
            self.backgrounded.insert(s(msg, "task_id").to_string());
        }
        // A background task ended: claude reads its result in a turn of its own, unasked. A
        // foreground one (no output file, never backgrounded) ended with its tool: nothing to say.
        if subtype == "task_notification"
            && (self.backgrounded.remove(s(msg, "task_id")) || !s(msg, "output_file").is_empty())
        {
            let ok = s(msg, "status") == "completed";
            let tool_id = s(msg, "tool_use_id");
            let max = if tool_id.is_empty() { 300 } else { 20_000 };
            let text = take_chars(s(msg, "summary"), max).to_string();
            // Shown under the tool that started it (#63); a notice only when there is none.
            if !text.is_empty() && !tool_id.is_empty() {
                push.push(ChatEvent::TaskResult {
                    tool_id: tool_id.to_string(),
                    ok,
                    text,
                });
            } else if !text.is_empty() {
                let level = if ok {
                    NoticeLevel::Info
                } else {
                    NoticeLevel::Error
                };
                push.push(ChatEvent::Notice { level, text });
            }
            push.push(ChatEvent::Busy { busy: true });
        }
    }

    fn stream_event(&mut self, msg: &Value, push: &mut Vec<ChatEvent>) {
        let ev = get(msg, "event");
        let ty = s(ev, "type");
        if ty == "message_start" && get(ev, "message").is_object() {
            self.message_id = s(get(ev, "message"), "id").to_string();
        }
        let delta = get(ev, "delta");
        if ty != "content_block_delta" || !delta.is_object() {
            return;
        }
        let id = format!(
            "{}:{}:{}",
            self.run,
            self.message_id,
            js_string(ev.get("index"))
        );
        match s(delta, "type") {
            "text_delta" => {
                self.streamed.insert(self.message_id.clone());
                push.push(ChatEvent::Text {
                    id,
                    delta: s(delta, "text").to_string(),
                });
            }
            "thinking_delta" => {
                self.streamed.insert(self.message_id.clone());
                push.push(ChatEvent::Thinking {
                    id,
                    delta: s(delta, "thinking").to_string(),
                });
            }
            _ => {}
        }
    }

    fn assistant(&mut self, msg: &Value, push: &mut Vec<ChatEvent>) {
        let m = get(msg, "message");
        let mid = s(m, "id");
        for (n, b) in objects(m, "content").into_iter().enumerate() {
            match s(b, "type") {
                "tool_use" => {
                    let name = s(b, "name");
                    let d = describe_claude_tool(name, get(b, "input"));
                    push.push(ChatEvent::Tool {
                        id: s(b, "id").to_string(),
                        name: name.to_string(),
                        kind: d.kind,
                        subtitle: d.subtitle,
                        preview: d.preview,
                    });
                }
                "text" if !self.streamed.contains(mid) && !s(b, "text").is_empty() => {
                    push.push(ChatEvent::Text {
                        id: format!("{}:{mid}:full:{n}", self.run),
                        delta: s(b, "text").to_string(),
                    });
                }
                _ => {}
            }
        }
    }

    fn user_message(&mut self, msg: &Value, push: &mut Vec<ChatEvent>) {
        for b in objects(get(msg, "message"), "content") {
            match s(b, "type") {
                "tool_result" => push.push(ChatEvent::ToolResult {
                    id: s(b, "tool_use_id").to_string(),
                    output: clip(&result_text(get(b, "content")), CLIP_MAX),
                    is_error: get(b, "is_error") == &Value::Bool(true),
                }),
                "text" if s(b, "text").starts_with(INTERRUPTED) => {
                    self.interrupted = true;
                    push.push(ChatEvent::Notice {
                        level: NoticeLevel::Info,
                        text: "Interrupted".into(),
                    });
                }
                _ => {}
            }
        }
    }

    fn rate_limit(&mut self, msg: &Value, push: &mut Vec<ChatEvent>) {
        // claude 2.1.306: rate_limit_info.unifiedWindows.{five_hour,seven_day} =
        // { utilization: 0–1, resetsAt: unix seconds }.
        let w = get(get(msg, "rate_limit_info"), "unifiedWindows");
        let win = |x: &Value| match (get(x, "utilization").as_f64(), get(x, "resetsAt").as_f64()) {
            (Some(u), Some(r)) => Some(UsageWindow {
                utilization: u.max(0.0),
                resets_at: r * 1000.0,
            }),
            _ => None,
        };
        let five_hour = win(get(w, "five_hour"));
        let seven_day = win(get(w, "seven_day"));
        if five_hour.is_some() || seven_day.is_some() {
            push.push(ChatEvent::Limits {
                limits: AccountLimits {
                    five_hour,
                    seven_day,
                    at: (self.now)(),
                },
            });
        }
    }

    fn result(&mut self, msg: &Value, push: &mut Vec<ChatEvent>) {
        // A turn's messages are all in: their ids are not needed again (#141).
        self.streamed.clear();
        let failed = get(msg, "is_error") == &Value::Bool(true) && !self.interrupted;
        self.interrupted = false;
        let cost = get(msg, "total_cost_usd").as_f64();
        let u = get(msg, "usage");
        let n = |key: &str| match get(u, key).as_f64() {
            Some(v) if v > 0.0 => v as u64,
            _ => 0,
        };
        let model = obj(get(msg, "modelUsage")).and_then(|m| m.keys().next().cloned());
        let error = failed.then(|| {
            let text = s(msg, "result");
            if text.is_empty() {
                format!("The turn failed ({}).", or(s(msg, "subtype"), "error"))
            } else {
                text.to_string()
            }
        });
        let usage = (cost.is_some() || u.is_object()).then(|| TurnUsage {
            model,
            cost_usd: Some(cost.unwrap_or(0.0)),
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
        });
        push.push(ChatEvent::TurnEnd {
            cost_usd: cost,
            duration_ms: get(msg, "duration_ms").as_f64(),
            error,
            usage,
        });
        push.push(ChatEvent::Busy { busy: false });
    }

    fn control_response(&mut self, msg: &Value, push: &mut Vec<ChatEvent>) {
        let r = get(msg, "response");
        // A refused switch (e.g. bypass in a session not allowed it): say why, show the mode in
        // force.
        if self.mode_reqs.remove(s(r, "request_id")) && s(r, "subtype") == "error" {
            push.push(ChatEvent::Notice {
                level: NoticeLevel::Error,
                text: format!(
                    "Couldn't change mode: {}",
                    or(s(r, "error"), "claude refused it")
                ),
            });
            if let Some(mode) = self.mode {
                push.push(ChatEvent::Mode { mode });
            }
            return;
        }
        let info = get(r, "response");
        if get(r, "request_id").as_str() != Some(self.hello_id.as_str())
            || s(r, "subtype") != "success"
            || !info.is_object()
        {
            return;
        }
        let models = objects(info, "models")
            .into_iter()
            .filter(|m| !s(m, "value").is_empty())
            .map(|m| ChatModel {
                value: s(m, "value").to_string(),
                label: or(s(m, "displayName"), s(m, "value")).to_string(),
                description: s(m, "description").to_string(),
                efforts: match get(m, "supportedEffortLevels") {
                    Value::Array(list) if get(m, "supportsEffort") == &Value::Bool(true) => list
                        .iter()
                        .filter_map(|e| e.as_str().map(String::from))
                        .collect(),
                    _ => Vec::new(),
                },
                auto_mode: get(m, "supportsAutoMode") == &Value::Bool(true),
            })
            .collect();
        // Names are plain tokens (`plugin:skill`); anything else is not a command we can type.
        let commands = objects(info, "commands")
            .into_iter()
            .filter(|c| command_token(s(c, "name")) && !s(c, "name").starts_with('_'))
            .take(500)
            .map(|c| ChatCommand {
                name: s(c, "name").to_string(),
                description: take_chars(s(c, "description"), 400).to_string(),
                argument_hint: take_chars(s(c, "argumentHint"), 120).to_string(),
                aliases: match get(c, "aliases") {
                    Value::Array(list) => list
                        .iter()
                        .map(str_of)
                        .filter(|a| command_token(a))
                        .map(String::from)
                        .collect(),
                    _ => Vec::new(),
                },
                builtin: get(c, "builtin") == &Value::Bool(true),
                mention: false,
            })
            .collect();
        let acc = get(info, "account");
        let account = acc.is_object().then(|| {
            let some = |v: &str| (!v.is_empty()).then(|| v.to_string());
            ChatAccountInfo {
                email: some(s(acc, "email")),
                plan: some(s(acc, "subscriptionType")),
            }
        });
        push.push(ChatEvent::Capabilities {
            models,
            account,
            commands: Some(commands),
        });
    }

    fn control_request(&mut self, msg: &Value, out: &mut Decoded) {
        let id = s(msg, "request_id").to_string();
        let r = get(msg, "request");
        if s(r, "subtype") != "can_use_tool" {
            // Hooks or MCP calls meant for an SDK host: say no, or claude waits for ever.
            out.replies.push(json!({
                "type": "control_response",
                "response": { "subtype": "error", "request_id": id, "error": "Not supported by Wraithgrid" }
            }));
            return;
        }
        let name = s(r, "tool_name");
        let suggestions = match get(r, "permission_suggestions") {
            Value::Array(list) => list.clone(),
            _ => Vec::new(),
        };
        let can_always = !suggestions.is_empty();
        let input = r.get("input").cloned();
        let d = describe_claude_tool(name, get(r, "input"));
        let questions = if name == "AskUserQuestion" {
            questions_of(get(r, "input"))
        } else {
            Vec::new()
        };
        self.pending
            .insert(id.clone(), Pending { input, suggestions });
        let tool_id = s(r, "tool_use_id");
        out.events.push(ChatEvent::Approval {
            req_id: id,
            tool_id: (!tool_id.is_empty()).then(|| tool_id.to_string()),
            name: or(s(r, "display_name"), name).to_string(),
            subtitle: d.subtitle,
            preview: d.preview,
            can_always,
            questions: (!questions.is_empty()).then_some(questions),
        });
    }
}

impl ChatAdapter for ClaudeAdapter {
    fn config_dir_var(&self) -> Option<&'static str> {
        Some("CLAUDE_CONFIG_DIR")
    }

    fn args(&mut self, o: &ArgsOptions) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            // Only makes Full access (bypassPermissions) a mode the user can pick mid-chat;
            // without it claude refuses the switch.
            "--allow-dangerously-skip-permissions",
        ]
        .map(String::from)
        .to_vec();
        if let Some(id) = &o.session_id {
            args.extend(["--resume".into(), id.clone()]);
        } else if o.continue_last {
            args.push("--continue".into());
        }
        if let Some(model) = &o.model {
            args.extend(["--model".into(), model.clone()]);
        }
        if let Some(effort) = &o.effort {
            args.extend(["--effort".into(), effort.clone()]);
        }
        if let Some(mode) = o.mode.filter(|m| *m != ChatMode::Default) {
            args.extend(["--permission-mode".into(), mode.as_str().into()]);
        }
        args
    }

    fn hello(&mut self) -> Vec<Value> {
        self.hello_id = self.req_id();
        vec![json!({
            "type": "control_request",
            "request_id": self.hello_id,
            "request": { "subtype": "initialize" }
        })]
    }

    fn set_mode(&mut self, mode: ChatMode) -> Option<Value> {
        let id = self.req_id();
        self.mode_reqs.insert(id.clone());
        Some(json!({
            "type": "control_request",
            "request_id": id,
            "request": { "subtype": "set_permission_mode", "mode": mode.as_str() }
        }))
    }

    fn set_model(&mut self, model: &str) -> Option<Value> {
        Some(json!({
            "type": "control_request",
            "request_id": self.req_id(),
            "request": { "subtype": "set_model", "model": model }
        }))
    }

    fn user(&mut self, text: &str, images: &[ChatImage]) -> Option<Value> {
        let content = if images.is_empty() {
            Value::String(text.to_string())
        } else {
            let mut blocks: Vec<Value> = images
                .iter()
                .map(|img| {
                    json!({
                        "type": "image",
                        "source": { "type": "base64", "media_type": img.media_type.as_str(), "data": img.data }
                    })
                })
                .collect();
            if !text.is_empty() {
                blocks.push(json!({ "type": "text", "text": text }));
            }
            Value::Array(blocks)
        };
        Some(json!({ "type": "user", "message": { "role": "user", "content": content } }))
    }

    fn interrupt(&mut self) -> Option<Value> {
        Some(json!({
            "type": "control_request",
            "request_id": self.req_id(),
            "request": { "subtype": "interrupt" }
        }))
    }

    fn answer(
        &mut self,
        req_id: &str,
        decision: ApprovalDecision,
        answers: Option<&ChatAnswers>,
    ) -> Option<Value> {
        let req = self.pending.remove(req_id)?;
        let response = if decision == ApprovalDecision::Deny {
            json!({ "behavior": "deny", "message": "The user denied this in Wraithgrid." })
        } else {
            let mut r = Map::new();
            r.insert("behavior".into(), "allow".into());
            // AskUserQuestion reads the picks from its input's `answers`.
            let input = match (answers, req.input) {
                (Some(a), Some(Value::Object(mut input))) => {
                    input.insert("answers".into(), json!(a));
                    Some(Value::Object(input))
                }
                (_, input) => input,
            };
            if let Some(input) = input {
                r.insert("updatedInput".into(), input);
            }
            if decision == ApprovalDecision::Always {
                r.insert("updatedPermissions".into(), Value::Array(req.suggestions));
            }
            Value::Object(r)
        };
        Some(json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": req_id, "response": response }
        }))
    }

    fn decode(&mut self, msg: &Value) -> Decoded {
        let mut out = Decoded::default();
        if !msg.is_object() {
            return out;
        }
        // A subagent's own steps: its Task tool call stands for all of them.
        if truthy(get(msg, "parent_tool_use_id")) {
            return out;
        }
        let ev = &mut out.events;
        match s(msg, "type") {
            "system" => self.system(msg, ev),
            "stream_event" => self.stream_event(msg, ev),
            "assistant" => self.assistant(msg, ev),
            "user" => self.user_message(msg, ev),
            "rate_limit_event" => self.rate_limit(msg, ev),
            "result" => self.result(msg, ev),
            "control_response" => self.control_response(msg, ev),
            "control_cancel_request" => {
                // claude no longer wants the answer (the turn was stopped): the bar goes (#140).
                let id = s(msg, "request_id");
                if self.pending.remove(id).is_some() {
                    ev.push(ChatEvent::ApprovalDone {
                        req_id: id.to_string(),
                        decision: ApprovalDecision::Deny,
                        answers: None,
                    });
                }
            }
            "control_request" => self.control_request(msg, &mut out),
            _ => {}
        }
        out
    }
}
