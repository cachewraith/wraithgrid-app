//! What a CLI needs to drive the chat view (ADR 0002), ported from `src/main/chat/adapter.ts`:
//! the args that run it headless, the lines it is fed, and a decoder from its output to
//! ChatEvents. One module per CLI next to this one; the session runner (`sessions.rs`) and the
//! UI never branch on the CLI: claude (stream-json with a control protocol), codex (JSON-RPC
//! app-server: its handshake is `hello` plus the replies to each answer) and agy (no live
//! controls, model list from `agy models`).

use serde_json::Value;

use crate::agy::AgyAdapter;
use crate::claude::ClaudeAdapter;
use crate::codex::CodexAdapter;
use crate::types::{ApprovalDecision, ChatAnswers, ChatEvent, ChatImage, ChatMode, ChatModel};

/// What one output line meant.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Decoded {
    pub events: Vec<ChatEvent>,
    /// Lines to write back at once (an answer the CLI waits for).
    pub replies: Vec<Value>,
}

/// How a chat is started: resume `session_id` (or the folder's last conversation), with the
/// chosen model, effort and mode when set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArgsOptions {
    pub session_id: Option<String>,
    pub continue_last: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<ChatMode>,
}

/// A CLI that doesn't say its models over the pipe: the command (run with the same binary,
/// env and folder) that lists them, and its parser. Sent as `capabilities` after the start.
#[derive(Clone)]
pub struct ListModels {
    pub args: Vec<String>,
    pub parse: fn(&str) -> Vec<ChatModel>,
}

/// One CLI's translation. An adapter keeps per-process state: make a fresh one per start.
pub trait ChatAdapter: Send {
    /// Args after the pane's own ones. Called once, before `hello`: an adapter may keep the
    /// options (codex sends model, effort and mode with each turn; agy reports its mode).
    fn args(&mut self, o: &ArgsOptions) -> Vec<String>;
    /// Lines written right after the process starts.
    fn hello(&mut self) -> Vec<Value>;
    /// The line that sends a message, or None when the adapter holds it until it is ready.
    fn user(&mut self, text: &str, images: &[ChatImage]) -> Option<Value>;
    /// The line that stops the running turn, or None when there is nothing to send now.
    fn interrupt(&mut self) -> Option<Value>;
    /// The line that switches the model of the running session, or None when it can't.
    fn set_model(&mut self, model: &str) -> Option<Value>;
    /// The line that switches the permission mode live, or None when it can't.
    fn set_mode(&mut self, _mode: ChatMode) -> Option<Value> {
        None
    }
    /// The answer to an approval request, or None when it is not pending (any more).
    fn answer(
        &mut self,
        req_id: &str,
        decision: ApprovalDecision,
        answers: Option<&ChatAnswers>,
    ) -> Option<Value>;
    /// One output line, already parsed from JSON.
    fn decode(&mut self, msg: &Value) -> Decoded;
    /// See [`ListModels`]; None for a CLI that lists its models over the pipe.
    fn list_models(&self) -> Option<ListModels> {
        None
    }
    /// The env var that points the CLI at an account's config dir (claude:
    /// `CLAUDE_CONFIG_DIR`, codex: `CODEX_HOME`); None when it has none.
    fn config_dir_var(&self) -> Option<&'static str> {
        None
    }

    /// One raw output line: trimmed, parsed, decoded. A line that is not JSON is a stray log
    /// line and means nothing.
    fn decode_line(&mut self, line: &str) -> Decoded {
        let line = line.trim();
        if line.is_empty() {
            return Decoded::default();
        }
        match serde_json::from_str::<Value>(line) {
            Ok(msg) => self.decode(&msg),
            Err(_) => Decoded::default(),
        }
    }
}

/// A fresh adapter for the CLI (as `AgentCli` names it), or None when the CLI has no chat
/// view.
pub fn chat_adapter_for(cli: &str) -> Option<Box<dyn ChatAdapter>> {
    match cli {
        "claude" => Some(Box::new(ClaudeAdapter::new())),
        "codex" => Some(Box::new(CodexAdapter::new())),
        "agy" => Some(Box::new(AgyAdapter::new())),
        _ => None,
    }
}
