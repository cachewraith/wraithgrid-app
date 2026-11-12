//! The chat view's own language (ADR 0002), ported from `src/shared/chat.ts`. Every agent CLI
//! that can run headless is translated into these events by its adapter (`adapter.rs`), so the
//! chat UI never knows which CLI it is talking to. The serde names are the TS ones, so views
//! saved by the Electron app read back here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How a tool call is drawn: a shell command, a file edit, a read, a search, …
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    Shell,
    Edit,
    Read,
    Search,
    Web,
    Agent,
    Todo,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TodoStatus {
    Pending,
    Active,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolPreview {
    Code {
        text: String,
    },
    Diff {
        path: String,
        before: String,
        after: String,
    },
    Todos {
        items: Vec<TodoItem>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    Allow,
    Always,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatQuestionOption {
    pub label: String,
    pub description: String,
}

/// One question of claude's AskUserQuestion tool, answered in the chat instead of allowed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatQuestion {
    pub question: String,
    pub header: String,
    pub multi_select: bool,
    pub options: Vec<ChatQuestionOption>,
}

/// The picked answers by question text; several picks joined with ", " (as claude reads them).
pub type ChatAnswers = BTreeMap<String, String>;

/// A model the CLI offers, as its own list names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatModel {
    /// What is passed back to choose it (an alias like `sonnet`).
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    /// Effort levels it takes; empty when it has no effort setting.
    #[serde(default)]
    pub efforts: Vec<String>,
    /// It can run in auto mode (claude: `supportsAutoMode`).
    #[serde(default)]
    pub auto_mode: bool,
}

/// A slash command or skill the CLI offers (claude: the `initialize` answer's `commands`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatCommand {
    pub name: String,
    pub description: String,
    /// What goes after the name, e.g. `<topic or idea>`; '' when it takes nothing.
    pub argument_hint: String,
    pub aliases: Vec<String>,
    /// Part of the CLI itself rather than a plugin, skill or the user's own file.
    pub builtin: bool,
    /// Named in text as `$name` rather than run as `/name` (codex skills).
    #[serde(default, skip_serializing_if = "is_false")]
    pub mention: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAccountInfo {
    pub email: Option<String>,
    /// The plan, e.g. "Claude Pro"; None when the CLI does not say.
    pub plan: Option<String>,
}

/// How much the agent may do without asking (claude's permission modes, as
/// `set_permission_mode` takes them; checked against claude 2.1.306).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatMode {
    Default,
    AcceptEdits,
    Plan,
    Auto,
    DontAsk,
    BypassPermissions,
}

pub const CHAT_MODES: [ChatMode; 6] = [
    ChatMode::Default,
    ChatMode::AcceptEdits,
    ChatMode::Plan,
    ChatMode::Auto,
    ChatMode::DontAsk,
    ChatMode::BypassPermissions,
];

impl ChatMode {
    /// The name claude uses (`acceptEdits`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            ChatMode::Default => "default",
            ChatMode::AcceptEdits => "acceptEdits",
            ChatMode::Plan => "plan",
            ChatMode::Auto => "auto",
            ChatMode::DontAsk => "dontAsk",
            ChatMode::BypassPermissions => "bypassPermissions",
        }
    }

    pub fn parse(s: &str) -> Option<ChatMode> {
        CHAT_MODES.into_iter().find(|m| m.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            ChatMode::Default => "Ask before edits",
            ChatMode::AcceptEdits => "Accept edits",
            ChatMode::Plan => "Plan",
            ChatMode::Auto => "Auto",
            ChatMode::DontAsk => "Don't ask",
            ChatMode::BypassPermissions => "Full access",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            ChatMode::Default => "Asks before it edits files or runs commands",
            ChatMode::AcceptEdits => "Edits files on its own; still asks before commands",
            ChatMode::Plan => "Reads and plans only; changes nothing",
            ChatMode::Auto => "Decides on its own what is safe to run",
            ChatMode::DontAsk => "Refuses anything that is not already allowed",
            ChatMode::BypassPermissions => "Runs everything without asking",
        }
    }
}

/// The modes Shift+Tab steps through, as in Claude Code (auto only where the model has it).
pub fn cycle_modes(auto_mode: bool) -> Vec<ChatMode> {
    let mut modes = vec![ChatMode::Default, ChatMode::AcceptEdits, ChatMode::Plan];
    if auto_mode {
        modes.push(ChatMode::Auto);
    }
    modes
}

/// Effort levels a chat can ask for (`--effort`); the model's own list narrows them.
pub const CHAT_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The label of an effort level; None for one this list doesn't know.
pub fn effort_label(effort: &str) -> Option<&'static str> {
    Some(match effort {
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra high",
        "max" => "Max",
        _ => return None,
    })
}

/// A model alias passed as an argument: a plain token (`CHAT_MODEL_PATTERN`).
pub fn is_valid_model(model: &str) -> bool {
    (1..=100).contains(&model.len())
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._[]-".contains(&b))
}

/// A conversation id is passed to the CLI as an argument: keep it to a plain token
/// (`chatSessionIdSchema`).
pub fn is_valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundKind {
    Shell,
    Agent,
    Other,
}

/// Work the agent started in the background that outlives its turn (a command, an agent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatBackgroundTask {
    pub id: String,
    pub kind: BackgroundKind,
    /// What it is doing, as the CLI describes it (or the command).
    pub label: String,
    /// When it was first seen running (ms).
    pub since: i64,
}

/// Can stop a turn / switch model / switch mode without a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveControls {
    pub interrupt: bool,
    pub model: bool,
    pub mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatControls {
    pub live: LiveControls,
    /// The modes it has, in menu order.
    pub modes: Vec<ChatMode>,
    /// Asks before running a tool (else a tool that needs approval is refused).
    pub approvals: bool,
}

/// One limit window: how much of it is used (0–1) and when it starts over (ms).
/// (`src/shared/usage.ts`; kept here so this crate needs no other workspace crate.)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub utilization: f64,
    pub resets_at: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLimits {
    pub five_hour: Option<UsageWindow>,
    pub seven_day: Option<UsageWindow>,
    /// When the CLI said so (ms).
    pub at: i64,
}

/// What one turn used, from the CLI's `result`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnUsage {
    pub model: Option<String>,
    /// None: the CLI reports no cost (codex, agy); never guessed.
    pub cost_usd: Option<f64>,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    Info,
    Error,
}

/// One thing the agent did, as the adapter tells the view about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ChatEvent {
    /// The CLI's conversation id, used to resume it later; `model` when it says.
    Session {
        id: String,
        model: Option<String>,
        /// Commands that only work in the CLI's own screen (claude: `terminal_slash_commands`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_only: Option<Vec<String>>,
    },
    /// What the CLI said it offers once it started: models to pick from, and whose plan.
    Capabilities {
        models: Vec<ChatModel>,
        account: Option<ChatAccountInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commands: Option<Vec<ChatCommand>>,
    },
    /// What the CLI can't do over the pipe (agy, #41): the view then restarts it to stop a
    /// turn or switch model/mode (resuming the conversation), offers only `modes`, and says it
    /// can't ask. Not sent: everything works live, all modes, approvals asked.
    Controls {
        controls: ChatControls,
    },
    /// The permission mode now in force (after a switch, or when the agent changed it).
    Mode {
        mode: ChatMode,
    },
    /// A turn started or ended.
    Busy {
        busy: bool,
    },
    /// The background work still running, as a whole list (empty when all of it ended). A
    /// turn can end while this is not empty: the chat is then waiting on it, not stopped (#39).
    Background {
        tasks: Vec<ChatBackgroundTask>,
    },
    /// Streamed answer text for the text block `id`.
    Text {
        id: String,
        delta: String,
    },
    /// Streamed reasoning for block `id`; the delta may be empty (hidden reasoning).
    Thinking {
        id: String,
        delta: String,
    },
    Tool {
        id: String,
        name: String,
        kind: ToolKind,
        /// One line: the command, the file, the pattern.
        subtitle: String,
        preview: Option<ToolPreview>,
    },
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    /// The CLI asks before running a tool; `tool_id` is the tool call it belongs to.
    Approval {
        req_id: String,
        tool_id: Option<String>,
        name: String,
        subtitle: String,
        preview: Option<ToolPreview>,
        can_always: bool,
        /// AskUserQuestion: what to ask; the answer goes back with the approval.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        questions: Option<Vec<ChatQuestion>>,
    },
    ApprovalDone {
        req_id: String,
        decision: ApprovalDecision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<ChatAnswers>,
    },
    TurnEnd {
        cost_usd: Option<f64>,
        duration_ms: Option<f64>,
        error: Option<String>,
        /// Tokens and cost for usage; None when the CLI does not report them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TurnUsage>,
    },
    /// The account's plan limits as the CLI reports them (claude: `rate_limit_event`).
    Limits {
        limits: AccountLimits,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
    /// A task started by a tool call ended (#63): its result belongs under that tool's row.
    TaskResult {
        tool_id: String,
        ok: bool,
        text: String,
    },
    /// The CLI process ended; the conversation can be resumed by starting again.
    Exit {
        code: Option<i32>,
        signal: Option<i32>,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChatImageType {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/gif")]
    Gif,
    #[serde(rename = "image/webp")]
    Webp,
}

impl ChatImageType {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatImageType::Png => "image/png",
            ChatImageType::Jpeg => "image/jpeg",
            ChatImageType::Gif => "image/gif",
            ChatImageType::Webp => "image/webp",
        }
    }

    /// Image files attached from disk, by extension (`.png`, lower case, with the dot);
    /// anything else is sent as a path (`CHAT_IMAGE_EXT`).
    pub fn from_ext(ext: &str) -> Option<ChatImageType> {
        Some(match ext {
            ".png" => ChatImageType::Png,
            ".jpg" | ".jpeg" => ChatImageType::Jpeg,
            ".gif" => ChatImageType::Gif,
            ".webp" => ChatImageType::Webp,
            _ => return None,
        })
    }
}

/// Base64 length of one pasted image (about 5 MB of image).
pub const CHAT_IMAGE_MAX: usize = 7_000_000;
pub const CHAT_IMAGES_MAX: usize = 4;
pub const CHAT_TEXT_MAX: usize = 200_000;
/// A saved conversation view, as the view serialised it.
pub const CHAT_SAVE_MAX: usize = 4_000_000;
/// Files attached to one message as paths (the agent reads them with its own tools).
pub const CHAT_FILES_MAX: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatImage {
    pub media_type: ChatImageType,
    /// Base64, no `data:` prefix.
    pub data: String,
}

/// Handed to a chat's box from elsewhere (a preview report, #74): added to the draft, not sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDrop {
    pub id: u64,
    /// The chip's text.
    pub label: String,
    /// Sent after the message.
    pub text: String,
    pub images: Vec<ChatImage>,
}

/// What the attach picker gave: images to send as images, other files as paths.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChatPickResult {
    pub images: Vec<ChatImage>,
    pub files: Vec<String>,
}

/// The message with its attached files listed after it, one path a line.
pub fn with_attachments(text: &str, files: &[String]) -> String {
    if files.is_empty() {
        return text.to_string();
    }
    let list = files
        .iter()
        .map(|f| {
            // Line breaks in a path would start a new list entry: one space for each run.
            let mut out = String::with_capacity(f.len() + 2);
            out.push_str("- ");
            let mut in_break = false;
            for ch in f.chars() {
                if ch == '\r' || ch == '\n' {
                    if !in_break {
                        out.push(' ');
                    }
                    in_break = true;
                } else {
                    out.push(ch);
                    in_break = false;
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n");
    let head = if text.is_empty() {
        String::new()
    } else {
        format!("{text}\n\n")
    };
    format!("{head}Attached files:\n{list}")
}

/// Clips tool output so a huge result never floods the view or the saved transcript. Counts
/// characters (the TS counted UTF-16 units; this never cuts a character in two).
pub fn clip(text: &str, max: usize) -> String {
    let total = text.chars().count();
    if total <= max {
        return text.to_string();
    }
    format!(
        "{}\n… ({} more characters)",
        take_chars(text, max),
        total - max
    )
}

/// The default `clip` length.
pub const CLIP_MAX: usize = 8000;

/// The first `n` characters of `s` (JS `slice(0, n)`, by character).
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}
