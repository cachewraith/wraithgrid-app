//! agy's headless mode for the chat view (#41), ported from `src/main/chat/agy.ts`: `--print=`
//! with stream-json in and out. Shapes checked against agy 1.4.4 output
//! (tests/fixtures/agy-chat.ndjson, agy-denied.ndjson): input is
//! `{"event":"user","message":{…}}` (text only); output is `init`, `step_update` (one per step:
//! user_input, agent_response with `text_delta`, tool with `tool_info`) and `result`. Over the
//! pipe agy can't ask before a tool (it refuses it and lists it in `denied_actions`), can't stop
//! a turn (no event for it, SIGINT is ignored) and can't switch model or mode: the view
//! restarts it for those, resuming by `--conversation <id>` (the `controls` event says so).

use std::collections::{BTreeSet, HashMap};
use std::sync::{LazyLock, Mutex, PoisonError};

use regex::Regex;
use serde_json::{json, Value};

use crate::adapter::{ArgsOptions, ChatAdapter, Decoded, ListModels};
use crate::attach::{chat_images_dir, save_chat_images};
use crate::claude::ToolDescription;
use crate::json::{get, js_string, now_ms, random_token, s, to_base36};
use crate::types::{
    clip, ApprovalDecision, ChatAnswers, ChatControls, ChatEvent, ChatImage, ChatMode, ChatModel,
    LiveControls, NoticeLevel, ToolKind, ToolPreview, TurnUsage, CHAT_EFFORTS, CLIP_MAX,
};

/// The modes agy has (`--mode`, `--dangerously-skip-permissions`); auto depends on the plan.
pub const AGY_MODES: [ChatMode; 4] = [
    ChatMode::Default,
    ChatMode::AcceptEdits,
    ChatMode::Plan,
    ChatMode::BypassPermissions,
];

fn mode_args(mode: ChatMode) -> &'static [&'static str] {
    match mode {
        ChatMode::AcceptEdits => &["--mode", "accept-edits"],
        ChatMode::Plan => &["--mode", "plan"],
        ChatMode::Auto => &["--mode", "auto"],
        ChatMode::BypassPermissions => &["--dangerously-skip-permissions"],
        ChatMode::Default | ChatMode::DontAsk => &[],
    }
}

/// Models that take `--effort`, from the last `agy models` list: agy names one model per level
/// (`gemini-3.8-flash-low`), and the base name takes `--effort`; other models refuse the flag.
/// Shared by every agy chat, like the module-level set in the TS.
static EFFORT_MODELS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

static MODEL_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]{1,100}$").expect("valid regex"));
static LEVEL_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+)-(low|medium|high|xhigh|max)$").expect("valid regex"));
static LEVEL_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*\((Low|Medium|High|Extra high|Max)\)\s*$").expect("valid regex")
});

/// `agy models` output (`id<TAB>Label` lines) → the picker's models, effort levels folded.
pub fn parse_agy_models(out: &str) -> Vec<ChatModel> {
    let mut models: Vec<ChatModel> = Vec::new();
    let mut by_base: HashMap<String, usize> = HashMap::new();
    for line in out.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut parts = line.split('\t').map(str::trim);
        let id = parts.next().unwrap_or("");
        let label = parts.next().unwrap_or("");
        if !MODEL_ID.is_match(id) {
            continue;
        }
        if let Some(m) = LEVEL_ID.captures(id) {
            let base = &m[1];
            let at = *by_base.entry(base.to_string()).or_insert_with(|| {
                let name = LEVEL_LABEL.replace(label, "");
                let name = if name.is_empty() { base } else { &name };
                models.push(ChatModel {
                    value: base.to_string(),
                    label: name.to_string(),
                    description: String::new(),
                    efforts: Vec::new(),
                    auto_mode: false,
                });
                models.len() - 1
            });
            models[at].efforts.push(m[2].to_string());
        } else {
            models.push(ChatModel {
                value: id.to_string(),
                label: if label.is_empty() { id } else { label }.to_string(),
                description: String::new(),
                efforts: Vec::new(),
                auto_mode: false,
            });
        }
    }
    for m in &mut models {
        m.efforts
            .sort_by_key(|e| CHAT_EFFORTS.iter().position(|x| x == e));
    }
    let mut effort_models = EFFORT_MODELS.lock().unwrap_or_else(PoisonError::into_inner);
    effort_models.clear();
    effort_models.extend(
        models
            .iter()
            .filter(|m| !m.efforts.is_empty())
            .map(|m| m.value.clone()),
    );
    models.truncate(100);
    models
}

/// `a || b` for strings.
fn or<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.is_empty() {
        b
    } else {
        a
    }
}

/// One agy tool call, drawn like the others: kind, one line, and a preview where it helps.
pub fn describe_agy_tool(name: &str, params: &Value) -> ToolDescription {
    let empty = json!({});
    let p = if params.is_object() { params } else { &empty };
    let file = or(
        or(s(p, "Path"), s(p, "AbsolutePath")),
        or(s(p, "TargetFile"), s(p, "File")),
    );
    let d = |kind, subtitle: &str| ToolDescription {
        kind,
        subtitle: subtitle.to_string(),
        preview: None,
    };
    match name {
        "run_command" | "send_command_input" | "command_status" => {
            let cmd = or(or(s(p, "CommandLine"), s(p, "Input")), s(p, "CommandId"));
            ToolDescription {
                kind: ToolKind::Shell,
                subtitle: cmd.to_string(),
                preview: (!cmd.is_empty()).then(|| ToolPreview::Code {
                    text: cmd.to_string(),
                }),
            }
        }
        "write_to_file"
        | "write_file"
        | "replace_file_content"
        | "multi_replace_file_content"
        | "sed_file"
        | "notebook_edit" => d(ToolKind::Edit, file),
        "view_file" | "read_file" | "read_resource" => d(ToolKind::Read, file),
        "grep_search" | "find_by_name" | "list_dir" => d(
            ToolKind::Search,
            or(
                or(or(s(p, "Query"), s(p, "Pattern")), s(p, "SearchPath")),
                or(s(p, "DirectoryPath"), file),
            ),
        ),
        "search_web" | "read_url_content" => d(
            ToolKind::Web,
            or(or(s(p, "query"), s(p, "Query")), s(p, "Url")),
        ),
        "invoke_subagent" | "browser_subagent" => {
            d(ToolKind::Agent, or(s(p, "Task"), s(p, "Description")))
        }
        _ => d(ToolKind::Other, file),
    }
}

static SIGN_IN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)not authenticated|sign[ -]?in|log ?in|unauthenticated|credentials")
        .expect("valid regex")
});

/// agy's `result.usage` → one turn's tokens (agy 1.4.4 reports no cost).
pub fn usage_of(u: &Value, model: Option<&str>) -> Option<TurnUsage> {
    if !u.is_object() {
        return None;
    }
    let n = |key: &str| match get(u, key).as_f64() {
        Some(v) if v > 0.0 => v as u64,
        _ => 0,
    };
    Some(TurnUsage {
        model: model.map(String::from),
        cost_usd: None,
        input: n("input_tokens"),
        // Thinking is output the model generated, like claude's.
        output: n("output_tokens") + n("thinking_tokens"),
        cache_read: n("cache_read_tokens"),
        cache_write: 0,
    })
}

/// The agy adapter for one process.
pub struct AgyAdapter {
    /// Step ids are this process's own: a view can hold several processes' turns.
    run: String,
    /// The mode it was started in: agy's init doesn't say.
    mode: ChatMode,
    /// Messages agy took since the last result: one result can answer several queued ones.
    taken: usize,
    /// The model init named, for the usage record.
    model: Option<String>,
}

impl Default for AgyAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgyAdapter {
    pub fn new() -> Self {
        AgyAdapter {
            run: format!("{}{}", to_base36(now_ms().max(0) as u64), random_token()),
            mode: ChatMode::Default,
            taken: 0,
            model: None,
        }
    }

    fn step_update(&mut self, msg: &Value, push: &mut Vec<ChatEvent>) {
        let st = get(msg, "step_update");
        let id = format!(
            "{}:{}:{}",
            self.run,
            s(st, "conversation_id"),
            js_string(st.get("step_index"))
        );
        let state = s(st, "state");
        let delta = s(st, "text_delta");
        match s(st, "step_type") {
            "user_input" => {
                if state == "DONE" {
                    self.taken += 1;
                }
            }
            "agent_response" | "notify_user" => {
                if !delta.is_empty() {
                    push.push(ChatEvent::Text {
                        id,
                        delta: delta.to_string(),
                    });
                }
            }
            "thinking" => push.push(ChatEvent::Thinking {
                id,
                delta: delta.to_string(),
            }),
            "tool" => {
                let info = get(st, "tool_info");
                let name = or(s(info, "name"), s(st, "tool_name"));
                let d = describe_agy_tool(name, get(info, "parameters"));
                push.push(ChatEvent::Tool {
                    id: id.clone(),
                    name: name.to_string(),
                    kind: d.kind,
                    subtitle: d.subtitle,
                    preview: d.preview,
                });
                if state != "ACTIVE" {
                    push.push(ChatEvent::ToolResult {
                        id,
                        output: clip(or(s(info, "output"), s(st, "error")), CLIP_MAX),
                        is_error: state != "DONE",
                    });
                }
            }
            "compaction" => {
                if state == "DONE" {
                    push.push(ChatEvent::Notice {
                        level: NoticeLevel::Info,
                        text: "Context compacted".into(),
                    });
                }
            }
            "error" => {
                let text = or(s(st, "error"), delta);
                if !text.is_empty() {
                    push.push(ChatEvent::Notice {
                        level: NoticeLevel::Error,
                        text: text.to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    fn result(&mut self, msg: &Value, push: &mut Vec<ChatEvent>) {
        let r = get(msg, "result");
        let failed = s(r, "status") != "SUCCESS";
        let error = failed.then(|| or(s(r, "error"), "The turn failed.").to_string());
        if error.as_deref().is_some_and(|e| SIGN_IN.is_match(e)) {
            push.push(ChatEvent::Notice {
                level: NoticeLevel::Error,
                text: "agy is not signed in. Open this account in a terminal pane once and sign in with Google there."
                    .into(),
            });
        }
        let mut denied: Vec<&str> = Vec::new();
        if let Value::Array(list) = get(r, "denied_actions") {
            for d in list.iter().filter(|d| d.is_object()) {
                let name = or(s(d, "display_name"), s(d, "action"));
                if !name.is_empty() && !denied.contains(&name) {
                    denied.push(name);
                }
            }
        }
        if !denied.is_empty() {
            push.push(ChatEvent::Notice {
                level: NoticeLevel::Error,
                text: format!(
                    "agy can't ask for approval in a chat, so it skipped: {}. Pick Full access in the mode menu, or allow it under permissions.allow in agy's settings.json.",
                    denied.join(", ")
                ),
            });
        }
        push.push(ChatEvent::TurnEnd {
            cost_usd: None,
            // Math.round: halves go up.
            duration_ms: get(r, "duration_seconds")
                .as_f64()
                .map(|secs| (secs * 1000.0 + 0.5).floor()),
            error,
            // agy reports tokens, never a cost or limits (#53).
            usage: usage_of(get(r, "usage"), self.model.as_deref()),
        });
        // One turn end per message it answered, so a queued one doesn't leave the chat busy.
        let ends = std::mem::take(&mut self.taken).max(1);
        for _ in 0..ends {
            push.push(ChatEvent::Busy { busy: false });
        }
    }
}

impl ChatAdapter for AgyAdapter {
    fn args(&mut self, o: &ArgsOptions) -> Vec<String> {
        self.mode = o
            .mode
            .filter(|m| AGY_MODES.contains(m))
            .unwrap_or(ChatMode::Default);
        let mut args: Vec<String> = [
            "--print=",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ]
        .map(String::from)
        .to_vec();
        if let Some(id) = &o.session_id {
            args.extend(["--conversation".into(), id.clone()]);
        } else if o.continue_last {
            args.push("--continue".into());
        }
        if let Some(model) = &o.model {
            args.extend(["--model".into(), model.clone()]);
            // The first start comes before `agy models` answered: then the window's pick stands
            // (it leaves effort out for a model its saved list says has none, #140).
            if let Some(effort) = &o.effort {
                let known = EFFORT_MODELS.lock().unwrap_or_else(PoisonError::into_inner);
                if known.is_empty() || known.contains(model) {
                    args.extend(["--effort".into(), effort.clone()]);
                }
            }
        }
        args.extend(mode_args(self.mode).iter().map(|a| a.to_string()));
        args
    }

    fn hello(&mut self) -> Vec<Value> {
        Vec::new()
    }

    fn user(&mut self, text: &str, images: &[ChatImage]) -> Option<Value> {
        // It reads only text: images go as `@path` file references. One that can't be written
        // is left out rather than failing the message.
        let refs = save_chat_images(images, &chat_images_dir(), now_ms())
            .unwrap_or_default()
            .iter()
            .map(|f| {
                let f = f.to_string_lossy();
                if f.chars().any(char::is_whitespace) {
                    format!("@\"{f}\"")
                } else {
                    format!("@{f}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let content = [text, refs.as_str()]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        Some(json!({ "event": "user", "message": { "role": "user", "content": content } }))
    }

    /// Nothing of these goes over agy's pipe: the view restarts it instead.
    fn interrupt(&mut self) -> Option<Value> {
        None
    }

    fn set_model(&mut self, _model: &str) -> Option<Value> {
        None
    }

    fn answer(
        &mut self,
        _req_id: &str,
        _decision: ApprovalDecision,
        _answers: Option<&ChatAnswers>,
    ) -> Option<Value> {
        None
    }

    fn decode(&mut self, msg: &Value) -> Decoded {
        let mut out = Decoded::default();
        let push = &mut out.events;
        match msg.get("event").and_then(Value::as_str) {
            Some("init") => {
                let id = s(msg, "conversation_id");
                let model = s(get(msg, "init"), "model");
                self.model = (!model.is_empty()).then(|| model.to_string());
                if !id.is_empty() {
                    push.push(ChatEvent::Session {
                        id: id.to_string(),
                        model: self.model.clone(),
                        terminal_only: None,
                    });
                }
                push.push(ChatEvent::Controls {
                    controls: ChatControls {
                        live: LiveControls {
                            interrupt: false,
                            model: false,
                            mode: false,
                        },
                        modes: AGY_MODES.to_vec(),
                        approvals: false,
                    },
                });
                push.push(ChatEvent::Mode { mode: self.mode });
            }
            Some("step_update") => self.step_update(msg, push),
            Some("result") => self.result(msg, push),
            _ => {}
        }
        out
    }

    fn list_models(&self) -> Option<ListModels> {
        Some(ListModels {
            args: vec!["models".into()],
            parse: parse_agy_models,
        })
    }
}
