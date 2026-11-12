//! A chat pane's conversation as the view draws it, folded from ChatEvents (ADR 0002), ported
//! from `src/renderer/lib/chat-thread.ts` and the pure parts of `chat-bus.ts`. Pure, so it is
//! tested without a window; the app keeps one per pane and saves it to disk (`views.rs`).
//! The TS returned new objects; here the thread is updated in place.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::types::{
    take_chars, ApprovalDecision, ChatAccountInfo, ChatAnswers, ChatBackgroundTask, ChatCommand,
    ChatControls, ChatEvent, ChatMode, ChatModel, ChatQuestion, NoticeLevel, ToolKind, ToolPreview,
};

/// The CLIs a handoff can name (`AGENT_CLIS` of `src/shared/agent-clis.ts`).
pub const AGENT_CLIS: [&str; 7] = [
    "claude", "gemini", "agy", "codex", "opencode", "cursor", "copilot",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApproval {
    pub req_id: String,
    pub can_always: bool,
    /// None while it waits for the user.
    pub decided: Option<ApprovalDecision>,
    /// AskUserQuestion: the questions, answered in the chat rather than allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<ChatQuestion>>,
    /// What was picked, once answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<ChatAnswers>,
}

/// What the task a tool started reported when it ended (#63), e.g. a sub-agent's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResult {
    pub ok: bool,
    pub text: String,
}

/// A notice's look; `Divider` is a line across the chat, e.g. "Continued as home · 14:32".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemLevel {
    Info,
    Error,
    Divider,
}

impl From<NoticeLevel> for ItemLevel {
    fn from(l: NoticeLevel) -> Self {
        match l {
            NoticeLevel::Info => ItemLevel::Info,
            NoticeLevel::Error => ItemLevel::Error,
        }
    }
}

/// A tool call card: what it ran, its result, and the approval it waited on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolItem {
    pub id: String,
    pub name: String,
    pub tool_kind: ToolKind,
    pub subtitle: String,
    pub preview: Option<ToolPreview>,
    /// None until the tool returns.
    pub output: Option<String>,
    pub is_error: bool,
    pub approval: Option<ToolApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
}

/// One row of the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChatItem {
    /// The first prompt of a cross-CLI chat (#78), shown collapsed.
    Handoff {
        id: String,
        text: String,
        messages: u64,
        from_cli: String,
        to_cli: String,
        divider: String,
    },
    /// `thumbs`: the images as small data URLs (#50); views saved before have only the count.
    User {
        id: String,
        text: String,
        images: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thumbs: Option<Vec<String>>,
    },
    /// Answer text, markdown, growing while it streams.
    Assistant {
        id: String,
        text: String,
    },
    Thinking {
        id: String,
        text: String,
    },
    Tool(ToolItem),
    Notice {
        id: String,
        level: ItemLevel,
        text: String,
    },
    /// A turn's footer: what it cost and took, or why it failed.
    Turn {
        id: String,
        cost_usd: Option<f64>,
        duration_ms: Option<f64>,
        error: Option<String>,
    },
}

impl ChatItem {
    pub fn id(&self) -> &str {
        match self {
            ChatItem::Handoff { id, .. }
            | ChatItem::User { id, .. }
            | ChatItem::Assistant { id, .. }
            | ChatItem::Thinking { id, .. }
            | ChatItem::Notice { id, .. }
            | ChatItem::Turn { id, .. } => id,
            ChatItem::Tool(t) => &t.id,
        }
    }

    fn waiting(&self) -> Option<&ToolItem> {
        match self {
            ChatItem::Tool(t) if t.approval.as_ref().is_some_and(|a| a.decided.is_none()) => {
                Some(t)
            }
            _ => None,
        }
    }
}

/// What the pane's status dot says about a chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatStatus {
    Running,
    Approval,
    Background,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatThread {
    pub items: Vec<ChatItem>,
    pub busy: bool,
    pub session_id: Option<String>,
    pub model: Option<String>,
    /// When the running turn started (ms, for the elapsed timer); None while idle.
    pub turn_started_at: Option<i64>,
    /// Ids for items made here (user messages, notices).
    pub seq: u64,
    /// How many leading items came from the saved view (drawn without an entrance).
    pub restored: usize,
    /// The models the CLI offered and whose account it runs as (empty until it said).
    pub models: Vec<ChatModel>,
    pub account: Option<ChatAccountInfo>,
    /// Slash commands and skills the CLI offers, and the ones that need its own screen.
    pub commands: Vec<ChatCommand>,
    pub terminal_only: Vec<String>,
    /// The permission mode the CLI says is in force; None until it said.
    pub mode: Option<ChatMode>,
    /// Background work still running after its turn (#39); never saved, it dies with the CLI.
    pub background: Vec<ChatBackgroundTask>,
    /// What the CLI can't do live (agy, #41); None = everything live, like claude.
    pub controls: Option<ChatControls>,
}

/// The thread reducer under its short name.
pub type Thread = ChatThread;

/// The saved view keeps this many items; older ones are dropped from the view only.
const KEEP_ITEMS: usize = 400;
/// A live view grows to LIVE_MAX items, then drops its oldest down to LIVE_KEEP (#141).
pub const LIVE_MAX: usize = 1000;
pub const LIVE_KEEP: usize = 800;

const SAVE_VERSION: u64 = 1;

/// Saved thumbnails go into an image view: only base64 data URLs of the image kinds agents take.
static THUMB_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^data:image/(?:png|jpeg|gif|webp);base64,[A-Za-z0-9+/]+=*$").expect("valid regex")
});
const THUMB_MAX: usize = 400_000;
const MAX_THUMBS: usize = 20;

/// The well-formed thumbnails of a saved list (at most 20); None when there are none.
pub fn valid_thumbs(v: &Value) -> Option<Vec<String>> {
    let list = v.as_array()?;
    let ok: Vec<String> = list
        .iter()
        .filter_map(Value::as_str)
        .filter(|s| s.len() <= THUMB_MAX && THUMB_RE.is_match(s))
        .take(MAX_THUMBS)
        .map(String::from)
        .collect();
    (!ok.is_empty()).then_some(ok)
}

impl ChatThread {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}-{}", self.seq)
    }

    /// The newest item with `id` (updates hit recent items, so it searches from the end).
    fn find(&mut self, id: &str) -> Option<&mut ChatItem> {
        self.items.iter_mut().rev().find(|it| it.id() == id)
    }

    /// Marks undecided approvals denied.
    fn settle_approvals(&mut self) {
        for it in &mut self.items {
            if let ChatItem::Tool(t) = it {
                if let Some(a) = t.approval.as_mut().filter(|a| a.decided.is_none()) {
                    a.decided = Some(ApprovalDecision::Deny);
                }
            }
        }
    }

    /// The user sent a message (with `images` attached): the turn starts. Returns its id.
    pub fn add_user_message(&mut self, text: &str, images: usize, now: i64) -> String {
        let id = self.next_id("u");
        self.busy = true;
        self.turn_started_at.get_or_insert(now);
        self.items.push(ChatItem::User {
            id: id.clone(),
            text: text.to_string(),
            images: images as u64,
            thumbs: None,
        });
        id
    }

    /// A sent message's thumbnails, made after it was added; false when it is gone.
    pub fn set_user_thumbs(&mut self, id: &str, new_thumbs: Vec<String>) -> bool {
        match self.find(id) {
            Some(ChatItem::User { thumbs, .. }) => {
                *thumbs = Some(new_thumbs);
                true
            }
            _ => false,
        }
    }

    /// A line of the app's own (a restart, an account switch). Returns its id.
    pub fn add_notice(&mut self, level: ItemLevel, text: &str) -> String {
        let id = self.next_id("n");
        self.items.push(ChatItem::Notice {
            id: id.clone(),
            level,
            text: text.to_string(),
        });
        id
    }

    /// Keeps a long-running chat's memory and per-render work bounded.
    pub fn cap_items(&mut self) {
        if self.items.len() <= LIVE_MAX {
            return;
        }
        let drop = self.items.len() - LIVE_KEEP;
        self.items.drain(..drop);
        self.restored = self.restored.saturating_sub(drop);
    }

    /// One event from the session, then the cap: what the view does with each (ChatBus).
    pub fn update(&mut self, e: &ChatEvent, now: i64) {
        self.apply(e, now);
        self.cap_items();
    }

    /// Folds one event into the thread. `now` is ms.
    pub fn apply(&mut self, e: &ChatEvent, now: i64) {
        match e {
            ChatEvent::Session {
                id,
                model,
                terminal_only,
            } => {
                self.session_id = Some(id.clone());
                if model.is_some() {
                    self.model = model.clone();
                }
                if let Some(t) = terminal_only {
                    self.terminal_only = t.clone();
                }
            }
            ChatEvent::Capabilities {
                models,
                account,
                commands,
            } => {
                self.models = models.clone();
                self.account = account.clone();
                if let Some(c) = commands {
                    self.commands = c.clone();
                }
            }
            ChatEvent::Busy { busy } => {
                // Nothing waits on a question once its turn is over (Stop, #140): not asked again.
                if !busy {
                    self.settle_approvals();
                }
                self.busy = *busy;
                self.turn_started_at = if *busy {
                    Some(self.turn_started_at.unwrap_or(now))
                } else {
                    None
                };
            }
            ChatEvent::Text { id, delta } | ChatEvent::Thinking { id, delta } => {
                let assistant = matches!(e, ChatEvent::Text { .. });
                match self.find(id) {
                    Some(ChatItem::Assistant { text, .. }) if assistant => text.push_str(delta),
                    Some(ChatItem::Thinking { text, .. }) if !assistant => text.push_str(delta),
                    Some(_) => {}
                    None => {
                        let (id, text) = (id.clone(), delta.clone());
                        self.items.push(if assistant {
                            ChatItem::Assistant { id, text }
                        } else {
                            ChatItem::Thinking { id, text }
                        });
                    }
                }
            }
            ChatEvent::Tool {
                id,
                name,
                kind,
                subtitle,
                preview,
            } => match self.find(id) {
                Some(ChatItem::Tool(t)) => {
                    t.name = name.clone();
                    t.tool_kind = *kind;
                    t.subtitle = subtitle.clone();
                    t.preview = preview.clone();
                }
                Some(_) => {}
                None => self.items.push(ChatItem::Tool(ToolItem {
                    id: id.clone(),
                    name: name.clone(),
                    tool_kind: *kind,
                    subtitle: subtitle.clone(),
                    preview: preview.clone(),
                    output: None,
                    is_error: false,
                    approval: None,
                    result: None,
                })),
            },
            ChatEvent::ToolResult {
                id,
                output,
                is_error,
            } => {
                if let Some(ChatItem::Tool(t)) = self.find(id) {
                    t.output = Some(output.clone());
                    t.is_error = *is_error;
                }
            }
            ChatEvent::TaskResult { tool_id, ok, text } => match self.find(tool_id) {
                Some(ChatItem::Tool(t)) => {
                    t.result = Some(TaskResult {
                        ok: *ok,
                        text: text.clone(),
                    });
                }
                Some(_) => {}
                None => {
                    let level = if *ok {
                        ItemLevel::Info
                    } else {
                        ItemLevel::Error
                    };
                    self.add_notice(level, take_chars(text, 300));
                }
            },
            ChatEvent::Approval {
                req_id,
                tool_id,
                name,
                subtitle,
                preview,
                can_always,
                questions,
            } => {
                let approval = ToolApproval {
                    req_id: req_id.clone(),
                    can_always: *can_always,
                    decided: None,
                    questions: questions.clone().filter(|q| !q.is_empty()),
                    answers: None,
                };
                let id = tool_id
                    .clone()
                    .unwrap_or_else(|| format!("approval-{req_id}"));
                match self.find(&id) {
                    Some(ChatItem::Tool(t)) => t.approval = Some(approval),
                    Some(_) => {}
                    None => self.items.push(ChatItem::Tool(ToolItem {
                        id,
                        name: name.clone(),
                        tool_kind: ToolKind::Other,
                        subtitle: subtitle.clone(),
                        preview: preview.clone(),
                        output: None,
                        is_error: false,
                        approval: Some(approval),
                        result: None,
                    })),
                }
            }
            ChatEvent::ApprovalDone {
                req_id,
                decision,
                answers,
            } => {
                for it in &mut self.items {
                    if let ChatItem::Tool(t) = it {
                        if let Some(a) = t.approval.as_mut().filter(|a| &a.req_id == req_id) {
                            a.decided = Some(*decision);
                            if answers.is_some() {
                                a.answers = answers.clone();
                            }
                        }
                    }
                }
            }
            ChatEvent::TurnEnd {
                cost_usd,
                duration_ms,
                error,
                ..
            } => {
                let id = self.next_id("t");
                self.items.push(ChatItem::Turn {
                    id,
                    cost_usd: *cost_usd,
                    duration_ms: *duration_ms,
                    error: error.clone(),
                });
            }
            ChatEvent::Limits { .. } => {}
            ChatEvent::Controls { controls } => self.controls = Some(controls.clone()),
            ChatEvent::Background { tasks } => self.background = tasks.clone(),
            ChatEvent::Mode { mode } => self.mode = Some(*mode),
            ChatEvent::Notice { level, text } => {
                self.add_notice((*level).into(), text);
            }
            ChatEvent::Exit { error, .. } => {
                self.busy = false;
                self.turn_started_at = None;
                self.background.clear();
                if let Some(err) = error {
                    self.add_notice(ItemLevel::Error, err);
                }
            }
        }
    }

    /// The tool call waiting for an answer, if any.
    pub fn pending_approval(&self) -> Option<&ToolItem> {
        self.items.iter().rev().find_map(ChatItem::waiting)
    }

    /// What the pane's status dot says about the chat.
    pub fn status(&self) -> ChatStatus {
        if self.busy && self.pending_approval().is_some() {
            ChatStatus::Approval
        } else if self.busy {
            ChatStatus::Running
        } else if !self.background.is_empty() {
            ChatStatus::Background
        } else {
            ChatStatus::Idle
        }
    }

    /// Ends the running turn here (its process was stopped), with a note why.
    pub fn settle(&mut self, text: &str, now: i64) {
        self.apply(&ChatEvent::Busy { busy: false }, now);
        self.background.clear();
        self.add_notice(ItemLevel::Info, text);
    }

    /// Puts the saved view before what arrived live while it was read (ChatBus `load`).
    pub fn merge_saved(&mut self, saved: ChatThread) {
        self.restored = saved.items.len();
        let mut items = saved.items;
        items.append(&mut self.items);
        self.items = items;
        if self.session_id.is_none() {
            self.session_id = saved.session_id;
        }
        if self.model.is_none() {
            self.model = saved.model;
        }
        self.seq += saved.seq;
        if self.models.is_empty() {
            self.models = saved.models;
        }
    }

    /// The saved view: the newest items, the conversation id and the models.
    pub fn serialize(&self) -> String {
        let from = self.items.len().saturating_sub(KEEP_ITEMS);
        json!({
            "v": SAVE_VERSION,
            "sessionId": self.session_id,
            "model": self.model,
            "seq": self.seq,
            "models": self.models,
            "items": &self.items[from..],
        })
        .to_string()
    }

    /// A saved view, or an empty thread when the file is missing, old or damaged. An item
    /// that doesn't read as one is dropped (the rest of the view stays).
    pub fn parse(raw: Option<&str>) -> ChatThread {
        let Some(d) = raw.and_then(|r| serde_json::from_str::<Value>(r).ok()) else {
            return ChatThread::default();
        };
        let Some(list) = d.get("items").and_then(Value::as_array) else {
            return ChatThread::default();
        };
        if d.get("v").and_then(Value::as_f64) != Some(SAVE_VERSION as f64) {
            return ChatThread::default();
        }
        let shaped: Vec<&Map<String, Value>> = list
            .iter()
            .filter_map(Value::as_object)
            .filter(|it| well_shaped(it))
            .collect();
        let mut items: Vec<ChatItem> = Vec::new();
        for it in &shaped {
            let Some(item) = read_item(it) else { continue };
            // Views saved before #51 hold a notice repeating the tool's description after each
            // long command: drop it.
            if let (ChatItem::Notice { level, text, .. }, Some(ChatItem::Tool(prev))) =
                (&item, items.last())
            {
                if *level == ItemLevel::Info && prev.subtitle == *text {
                    continue;
                }
            }
            items.push(item);
        }
        let seq = match d.get("seq") {
            Some(Value::Number(n)) => n.as_f64().map_or(0, |f| f.max(0.0) as u64),
            _ => shaped.len() as u64,
        };
        ChatThread {
            restored: items.len(),
            items,
            session_id: d.get("sessionId").and_then(Value::as_str).map(String::from),
            model: d.get("model").and_then(Value::as_str).map(String::from),
            seq,
            models: d
                .get("models")
                .and_then(|m| serde_json::from_value(m.clone()).ok())
                .unwrap_or_default(),
            ..ChatThread::default()
        }
    }
}

const ITEM_KINDS: [&str; 7] = [
    "user",
    "assistant",
    "thinking",
    "tool",
    "notice",
    "turn",
    "handoff",
];

/// The TS filter: a known kind with a string id; a handoff with its fields in order.
fn well_shaped(it: &Map<String, Value>) -> bool {
    let kind = it.get("kind").and_then(Value::as_str).unwrap_or("");
    let cli = |key: &str| {
        it.get(key)
            .and_then(Value::as_str)
            .is_some_and(|c| AGENT_CLIS.contains(&c))
    };
    ITEM_KINDS.contains(&kind)
        && it.get("id").is_some_and(Value::is_string)
        && (kind != "handoff"
            || (it.get("text").is_some_and(Value::is_string)
                && it.get("divider").is_some_and(Value::is_string)
                && it.get("messages").and_then(Value::as_u64).is_some()
                && cli("fromCli")
                && cli("toCli")))
}

/// One saved item: a malformed task result or thumbnail list is dropped rather than the item,
/// and a request the old process asked can never be answered now, so it reads as denied.
fn read_item(raw: &Map<String, Value>) -> Option<ChatItem> {
    let mut it = raw.clone();
    if let Some(result) = it.remove("result") {
        if result.get("ok").is_some_and(Value::is_boolean)
            && result.get("text").is_some_and(Value::is_string)
        {
            it.insert(
                "result".into(),
                json!({ "ok": result["ok"], "text": result["text"] }),
            );
        }
    }
    if let Some(thumbs) = it.remove("thumbs") {
        if let Some(ok) = valid_thumbs(&thumbs) {
            it.insert("thumbs".into(), json!(ok));
        }
    }
    let mut item: ChatItem = serde_json::from_value(Value::Object(it)).ok()?;
    if let ChatItem::Tool(t) = &mut item {
        if let Some(a) = t.approval.as_mut().filter(|a| a.decided.is_none()) {
            a.decided = Some(ApprovalDecision::Deny);
        }
    }
    Some(item)
}
