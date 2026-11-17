//! The chat view's engine without its UI (ADR 0002, ported for the GPUI rewrite, ADR 0005):
//! the chat's own event language, the per-CLI adapters that translate a headless CLI into it,
//! the process runner, the thread reducer the view draws from, and the helpers around them
//! (the `/` and `@` menus, the message box's quotes, history and questions, attachments, saved
//! views, moving a transcript between accounts).
//!
//! Driving a chat: build a [`ChatLaunch`], take an adapter from [`chat_adapter_for`], call
//! [`ChatSession::start`], and fold each received [`ChatEvent`] into a [`ChatThread`] with
//! [`ChatThread::update`]; the session's methods send messages, answer approvals, interrupt
//! and stop.

pub mod adapter;
pub mod agy;
pub mod attach;
pub mod claude;
pub mod codex;
pub mod compose;
pub mod files;
mod json;
pub mod present;
pub mod session;
pub mod slash;
mod text;
pub mod thread;
pub mod transcript;
pub mod types;
pub mod views;

pub use adapter::{chat_adapter_for, ArgsOptions, ChatAdapter, Decoded, ListModels};
pub use agy::{describe_agy_tool, parse_agy_models, AgyAdapter};
pub use claude::{describe_claude_tool, ClaudeAdapter, ToolDescription};
pub use codex::{
    describe_codex_item, parse_codex_models, parse_codex_skills, CodexAdapter, CodexSkill,
};
pub use json::now_ms;
pub use session::{ChatLaunch, ChatSession, StartError, TurnState};
pub use text::locale_cmp;
pub use thread::{ChatItem, ChatStatus, ChatThread, ItemLevel, Thread, ToolApproval, ToolItem};
pub use types::*;
