//! A chat pane (`ChatView.tsx`, ADR 0002): the conversation drawn from the `wraithgrid-chat`
//! thread, a composer, and the agent CLI running headless behind it. The saved view in
//! `<userData>/chats/<paneId>.json` is the Electron app's, so existing chats open as they were.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::component::input::{self, InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_chat::files::{rank_paths, system_git, FileLister};
use wraithgrid_chat::slash::{
    build_slash_items, file_item, file_query, leading_command, rank_slash, slash_query,
    split_mentions, SlashItem,
};
use wraithgrid_chat::views::{load_view, save_view};
use wraithgrid_chat::{
    chat_adapter_for, now_ms, ApprovalDecision, ArgsOptions, ChatEvent, ChatItem, ChatLaunch,
    ChatSession, ChatStatus, ChatThread, ItemLevel, ToolItem, ToolPreview,
};
use wraithgrid_chat::{cycle_modes, ChatImage, ChatImageType, ChatMode, ChatModel};

mod extras;

pub use extras::Queued;

const SAVE_DELAY: Duration = Duration::from_millis(800);

/// Everything needed to start the pane's CLI, resolved by the window.
#[derive(Clone)]
pub struct ChatSetup {
    pub cli: String,
    pub program: Option<PathBuf>,
    pub cwd: PathBuf,
    pub env: std::collections::BTreeMap<String, String>,
    pub config_dir: Option<PathBuf>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<wraithgrid_chat::ChatMode>,
}

pub enum ChatViewEvent {
    /// A message was sent: the pane has a conversation (and a title, from the first one).
    Submitted {
        text: String,
    },
    /// The CLI named its conversation: resumed on the next start.
    SessionId(String),
    Status(ChatStatus),
    /// The chat's model, effort or mode changed: saved on the pane for its next start.
    Choices {
        model: Option<String>,
        effort: Option<String>,
        mode: Option<ChatMode>,
    },
    /// `/usage`.
    OpenUsage,
    /// Limits and turn costs for usage.json, in the usage crate's terms.
    Usage(wraithgrid_usage::store::UsageEvent),
    /// A turn ended (`error` set when it failed): the window decides on switching at the limit.
    TurnEnded {
        error: Option<String>,
    },
    /// "Send later" (#77): queue the text for this chat.
    Queue {
        text: String,
        when: wraithgrid_schedule::SendLater,
    },
    /// A queued message: cancelled, or sent now (job id).
    CancelQueued(String),
    SendQueuedNow(String),
}

/// The `/` or `@` menu over the composer: its items and where the word it completes starts.
struct Menu {
    items: Vec<SlashItem>,
    selected: usize,
    start: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Model,
    Effort,
    Mode,
}

/// claude's own aliases, offered before the CLI has listed its models.
fn fallback_models() -> Vec<ChatModel> {
    [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]
        .iter()
        .map(|(v, l)| ChatModel {
            value: v.to_string(),
            label: l.to_string(),
            description: String::new(),
            efforts: ["low", "medium", "high", "xhigh", "max"]
                .iter()
                .map(|e| e.to_string())
                .collect(),
            auto_mode: false,
        })
        .collect()
}

/// Files for the `@` menu, one lister for every chat (each folder's list is cached).
fn file_lister() -> &'static FileLister {
    static LISTER: std::sync::OnceLock<FileLister> = std::sync::OnceLock::new();
    LISTER.get_or_init(|| FileLister::new(system_git(4 * 1024 * 1024)))
}

pub struct ChatView {
    pane_id: String,
    focus: FocusHandle,
    thread: ChatThread,
    setup: ChatSetup,
    session: Option<ChatSession>,
    composer: Entity<TextareaState>,
    markdown: HashMap<String, Entity<TextViewState>>,
    views_dir: PathBuf,
    scroll: ScrollHandle,
    error: Option<String>,
    menu: Option<Menu>,
    picker: Option<Picker>,
    /// Pasted images waiting to be sent, and their thumbnails.
    images: Vec<(ChatImage, Arc<Image>)>,
    /// Quotes, history, questions, image viewer and send-later state (`extras`).
    x: extras::Extras,
    _events: Option<Task<()>>,
    _save: Option<Task<()>>,
    _subscription: Subscription,
}

impl ChatView {
    pub fn new(
        pane_id: String,
        setup: ChatSetup,
        views_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let thread = ChatThread::parse(load_view(&views_dir, &pane_id).as_deref());
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Message the agent (Enter sends, Shift+Enter for a new line)")
                .submit_on_enter(true)
                .auto_grow(1, 10)
        });
        let sub = Self::composer_events(&composer, window, cx);
        let scroll = ScrollHandle::new();
        scroll.scroll_to_bottom();
        ChatView {
            pane_id,
            focus: cx.focus_handle(),
            thread,
            setup,
            session: None,
            composer,
            markdown: HashMap::new(),
            views_dir,
            scroll,
            error: None,
            menu: None,
            picker: None,
            images: Vec::new(),
            x: extras::Extras::default(),
            _events: None,
            _save: None,
            _subscription: sub,
        }
    }

    /// The composer's Enter and edits, handled in this window.
    fn composer_events(
        composer: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            composer,
            window,
            |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::PressEnter { shift: false, .. } => {
                    if this.menu.is_some() {
                        this.accept_menu(window, cx);
                    } else {
                        this.send(window, cx);
                    }
                }
                InputEvent::Change => this.update_menu(cx),
                _ => {}
            },
        )
    }

    /// The window reopened from the tray: the composer listens in the new one.
    pub fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._subscription = Self::composer_events(&self.composer.clone(), window, cx);
    }

    pub fn status(&self) -> ChatStatus {
        self.thread.status()
    }

    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }

    /// Starts the CLI, resuming the pane's conversation when it has one.
    fn start(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let s = &self.setup;
        let program = s
            .program
            .clone()
            .ok_or_else(|| format!("{} was not found on PATH. Set its path in Settings.", s.cli))?;
        let adapter = chat_adapter_for(&s.cli).ok_or_else(|| {
            format!(
                "The chat view for {} isn't ported yet. Open its CLI in a terminal.",
                s.cli
            )
        })?;
        let mut launch = ChatLaunch::new(program, s.cwd.clone());
        // Codex reads the project's CLAUDE.md when it has no AGENTS.md, as its terminals do.
        if let Some(cli) = wraithgrid_core::agent_cli::AgentCli::parse(&s.cli) {
            launch.args = wraithgrid_core::agent_cli::with_project_doc_args(cli, &[]);
        }
        launch.env = s.env.clone();
        launch.config_dir = s.config_dir.clone();
        launch.options = ArgsOptions {
            session_id: self
                .thread
                .session_id
                .clone()
                .or_else(|| s.session_id.clone()),
            continue_last: false,
            model: s.model.clone(),
            effort: s.effort.clone(),
            mode: s.mode,
        };
        let (session, rx) = ChatSession::start(launch, adapter).map_err(|e| e.to_string())?;
        // The receiver is blocking: a thread hands events to the UI.
        let (tx, mut events) = futures::channel::mpsc::unbounded::<ChatEvent>();
        std::thread::spawn(move || {
            for e in rx {
                if tx.unbounded_send(e).is_err() {
                    break;
                }
            }
        });
        self._events = Some(cx.spawn(async move |this, cx| {
            while let Some(e) = events.next().await {
                if this.update(cx, |this, cx| this.apply(&e, cx)).is_err() {
                    break;
                }
            }
        }));
        self.session = Some(session);
        Ok(())
    }

    fn apply(&mut self, e: &ChatEvent, cx: &mut Context<Self>) {
        match e {
            ChatEvent::Limits { limits } => cx.emit(ChatViewEvent::Usage(
                wraithgrid_usage::store::UsageEvent::Limits(usage_limits(limits)),
            )),
            ChatEvent::TurnEnd { error, usage, .. } => {
                cx.emit(ChatViewEvent::Usage(
                    wraithgrid_usage::store::UsageEvent::TurnEnd(usage.as_ref().map(|u| {
                        wraithgrid_usage::model::TurnUsage {
                            model: u.model.clone(),
                            cost_usd: u.cost_usd,
                            input: u.input,
                            output: u.output,
                            cache_read: u.cache_read,
                            cache_write: u.cache_write,
                        }
                    })),
                ));
                cx.emit(ChatViewEvent::TurnEnded {
                    error: error.clone(),
                });
            }
            _ => {}
        }
        let before = (self.thread.session_id.clone(), self.thread.status());
        self.thread.update(e, now_ms());
        if self.thread.session_id != before.0 {
            if let Some(id) = self.thread.session_id.clone() {
                cx.emit(ChatViewEvent::SessionId(id));
            }
        }
        if self.thread.status() != before.1 {
            cx.emit(ChatViewEvent::Status(self.thread.status()));
        }
        self.scroll.scroll_to_bottom();
        self.schedule_save(cx);
        cx.notify();
    }

    /// Debug builds only: types `text` into the composer and sends it, for tests.
    pub fn send_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(debug_assertions) {
            self.composer
                .update(cx, |c, cx| c.set_value(text.to_string(), window, cx));
            self.send(window, cx);
        }
    }

    /// Recomputes the `/` or `@` menu for the word the caret is in.
    fn update_menu(&mut self, cx: &mut Context<Self>) {
        let state = self.composer.read(cx);
        let text = state.value().to_string();
        // A caret still at 0 with text in the box is one that was set, not typed: the end.
        let caret = match state.cursor().min(text.len()) {
            0 => text.len(),
            c => c,
        };
        self.menu = if let Some(q) = slash_query(&text, caret) {
            let items = rank_slash(
                &build_slash_items(&self.thread.commands, &self.thread.terminal_only),
                q,
                12,
            );
            (!items.is_empty()).then_some(Menu {
                items,
                selected: 0,
                start: 0,
            })
        } else if let Some(w) = file_query(&text, caret) {
            let paths = file_lister().list(&self.setup.cwd);
            let items: Vec<SlashItem> = rank_paths(&paths, &w.query, 12)
                .iter()
                .map(|p| file_item(p))
                .collect();
            (!items.is_empty()).then_some(Menu {
                items,
                selected: 0,
                start: w.start,
            })
        } else {
            None
        };
        cx.notify();
    }

    /// Puts the picked item in place of the word being typed.
    fn accept_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        let Some(item) = menu.items.get(menu.selected) else {
            return;
        };
        let text = self.composer.read(cx).value().to_string();
        let caret = self.composer.read(cx).cursor().min(text.len());
        let next = format!(
            "{}{}{} {}",
            &text[..menu.start],
            item.sigil,
            item.name,
            text[caret..].trim_start()
        );
        self.composer
            .update(cx, |c, cx| c.set_value(next, window, cx));
        cx.notify();
    }

    /// Keys that reach the composer as keys (not bound by the text box): Ctrl+Shift+R.
    fn composer_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.extras_key(e, window, cx) {
            cx.stop_propagation();
        }
    }

    /// ↑ / ↓ / Tab / Esc as the text box's actions: the `/` and `@` menu takes them first, then
    /// ↑ / ↓ walk earlier prompts. True when taken (the box doesn't move its caret).
    fn composer_nav(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(menu) = &mut self.menu {
            let n = menu.items.len().max(1);
            match key {
                "down" => menu.selected = (menu.selected + 1) % n,
                "up" => menu.selected = (menu.selected + n - 1) % n,
                "tab" => self.accept_menu(window, cx),
                "escape" => self.menu = None,
                _ => return false,
            }
            cx.notify();
            return true;
        }
        match key {
            "up" | "down" if self.picker.is_none() => self.history_key(key == "up", window, cx),
            _ => false,
        }
    }

    /// Shift+Tab steps through the modes, as in Claude Code.
    fn cycle_mode(&mut self, cx: &mut Context<Self>) {
        let auto = self.current_model().is_some_and(|m| m.auto_mode);
        let modes = cycle_modes(auto);
        let now = self
            .thread
            .mode
            .or(self.setup.mode)
            .unwrap_or(ChatMode::Default);
        let next = modes
            .iter()
            .position(|x| *x == now)
            .map_or(modes[0], |i| modes[(i + 1) % modes.len()]);
        self.choose_mode(next, cx);
    }

    /// An image on the clipboard is attached rather than pasted as text.
    fn paste_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        for entry in item.entries() {
            if let ClipboardEntry::Image(image) = entry {
                self.attach_image(image.clone(), cx);
                return true;
            }
        }
        false
    }

    /// Debug builds only: attaches a PNG as if pasted, for tests.
    pub fn attach_png(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if cfg!(debug_assertions) {
            self.attach_image(Image::from_bytes(ImageFormat::Png, bytes), cx);
        }
    }

    fn attach_image(&mut self, image: Image, cx: &mut Context<Self>) {
        use base64::Engine as _;
        let media_type = match image.format {
            ImageFormat::Png => ChatImageType::Png,
            ImageFormat::Jpeg => ChatImageType::Jpeg,
            ImageFormat::Gif => ChatImageType::Gif,
            ImageFormat::Webp => ChatImageType::Webp,
            _ => {
                self.error = Some("Only PNG, JPEG, GIF and WebP images can be sent.".into());
                return cx.notify();
            }
        };
        let data = base64::engine::general_purpose::STANDARD.encode(&image.bytes);
        self.images
            .push((ChatImage { media_type, data }, Arc::new(image)));
        cx.notify();
    }

    fn current_model(&self) -> Option<ChatModel> {
        let models = if self.thread.models.is_empty() {
            fallback_models()
        } else {
            self.thread.models.clone()
        };
        let want = self.thread.model.clone().or(self.setup.model.clone());
        match want {
            // A full model id the list doesn't name (claude-opus-…): shown as it is.
            Some(w) => models
                .iter()
                .find(|m| m.value == w || w.contains(&m.value))
                .cloned()
                .or(Some(ChatModel {
                    value: w.clone(),
                    label: w,
                    description: String::new(),
                    efforts: models
                        .first()
                        .map(|m| m.efforts.clone())
                        .unwrap_or_default(),
                    auto_mode: false,
                })),
            None => models.first().cloned(),
        }
    }

    fn emit_choices(&mut self, cx: &mut Context<Self>) {
        cx.emit(ChatViewEvent::Choices {
            model: self.setup.model.clone(),
            effort: self.setup.effort.clone(),
            mode: self.setup.mode,
        });
    }

    fn choose_model(&mut self, value: String, cx: &mut Context<Self>) {
        if let Some(session) = self.session.as_ref().filter(|s| s.is_running()) {
            session.set_model(&value);
        }
        self.setup.model = Some(value);
        self.picker = None;
        self.emit_choices(cx);
        cx.notify();
    }

    /// Effort is a start option: an idle chat restarts with it on the next message.
    fn choose_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        self.setup.effort = effort;
        if self.session.as_ref().is_some_and(|s| !s.is_busy()) {
            if let Some(s) = self.session.take() {
                s.stop();
            }
        }
        self.picker = None;
        self.emit_choices(cx);
        cx.notify();
    }

    fn choose_mode(&mut self, mode: ChatMode, cx: &mut Context<Self>) {
        if let Some(session) = self.session.as_ref().filter(|s| s.is_running()) {
            session.set_mode(mode);
        }
        self.setup.mode = Some(mode);
        self.thread.mode = Some(mode);
        self.picker = None;
        self.emit_choices(cx);
        cx.notify();
    }

    /// `/new`, `/model`, `/effort`, `/mode`, `/usage`: Wraithgrid's own commands.
    fn app_command(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match name {
            "new" | "clear" => {
                if let Some(s) = self.session.take() {
                    s.stop();
                }
                self.thread = ChatThread::new();
                self.setup.session_id = None;
                self.markdown.clear();
                self.schedule_save(cx);
            }
            "model" => self.picker = Some(Picker::Model),
            "effort" => self.picker = Some(Picker::Effort),
            "mode" | "permissions" => self.picker = Some(Picker::Mode),
            "usage" => cx.emit(ChatViewEvent::OpenUsage),
            _ => return false,
        }
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        cx.notify();
        true
    }

    pub fn items(&self) -> &[ChatItem] {
        &self.thread.items
    }

    /// A chat continued from another CLI (#78): the handoff shows (collapsed) and goes to the
    /// new CLI as its first message.
    pub fn start_handoff(
        &mut self,
        text: &str,
        messages: u64,
        from_cli: &str,
        to_cli: &str,
        divider: &str,
        cx: &mut Context<Self>,
    ) {
        self.thread
            .add_handoff(text, messages, from_cli, to_cli, divider);
        if let Err(e) = self.start(cx) {
            self.error = Some(e);
        } else if let Some(session) = &self.session {
            session.send(text, &[], &[]);
            self.thread.busy = true;
            self.thread.turn_started_at = Some(now_ms());
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// "New conversation" from the pane's menu: as `/new`.
    pub fn new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.app_command("new", window, cx);
    }

    /// Puts `text` into the box after what is there, unsent ("Attach to chat" from a preview).
    pub fn add_to_draft(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let cur = self.composer.read(cx).value().trim_end().to_string();
        let next = if cur.is_empty() {
            text.to_string()
        } else {
            format!("{cur}\n\n{text}")
        };
        self.composer.update(cx, |c, cx| {
            c.set_value(next, window, cx);
            c.focus(window, cx);
        });
        cx.notify();
    }

    /// Debug builds only: types `text` without sending, so the menus can be looked at.
    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(debug_assertions) {
            self.composer
                .update(cx, |c, cx| c.set_value(text.to_string(), window, cx));
            self.update_menu(cx);
        }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.composer.read(cx).value().trim().to_string();
        if typed.is_empty() && self.images.is_empty() && self.x.quotes.is_empty() {
            return;
        }
        // Reply quotes go first, as markdown blockquotes (#164).
        let text = wraithgrid_chat::compose::with_quotes(&self.x.quotes, &typed)
            .trim()
            .to_string();
        self.x.quotes.clear();
        self.x.history = Default::default();
        if let Some(cmd) = leading_command(&text).filter(|c| c.rest.trim().is_empty()) {
            if self.app_command(&cmd.name, window, cx) {
                return;
            }
        }
        if self.session.as_ref().is_none_or(|s| !s.is_running()) {
            if let Err(e) = self.start(cx) {
                self.error = Some(e);
                return cx.notify();
            }
        }
        self.error = None;
        let images: Vec<ChatImage> = self.images.drain(..).map(|(i, _)| i).collect();
        // `@path` mentions go to the CLI as files, as the Electron chat sends them.
        let files: Vec<String> = split_mentions(&text)
            .into_iter()
            .filter(|p| p.mention)
            .map(|p| p.text.trim_start_matches('@').to_string())
            .collect();
        self.thread.add_user_message(&text, images.len(), now_ms());
        if let Some(session) = &self.session {
            if !session.send(&text, &images, &files) {
                self.error = Some("The message is too long to send.".into());
            }
        }
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        cx.emit(ChatViewEvent::Submitted { text });
        self.scroll.scroll_to_bottom();
        self.schedule_save(cx);
        cx.notify();
    }

    /// Sends `text` again (a message the limit refused, after a switch).
    pub fn resend(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |c, cx| c.set_value(text.to_string(), window, cx));
        self.send(window, cx);
    }

    pub fn is_busy(&self) -> bool {
        self.thread.busy
    }

    pub fn session_id(&self) -> Option<String> {
        self.thread
            .session_id
            .clone()
            .or_else(|| self.setup.session_id.clone())
    }

    /// The last thing the user sent, to send again after a switch at the limit.
    pub fn last_user_text(&self) -> Option<String> {
        self.thread.items.iter().rev().find_map(|i| match i {
            ChatItem::User { text, .. } => Some(text.clone()),
            _ => None,
        })
    }

    /// Stops the CLI (its conversation stays, to be resumed or moved).
    pub fn stop_session(&mut self) {
        if let Some(s) = self.session.take() {
            s.stop();
        }
    }

    /// A line across the chat ("Continued as home · 14:32") or a notice, saved with the view.
    pub fn add_notice(&mut self, level: ItemLevel, text: &str, cx: &mut Context<Self>) {
        self.thread.add_notice(level, text);
        self.save();
        cx.notify();
    }

    fn approve(&mut self, req_id: String, decision: ApprovalDecision, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            session.approve(&req_id, decision, None);
        }
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            session.interrupt();
        }
        cx.notify();
    }

    /// The view is saved shortly after it changes, as the Electron app does.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self._save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = this.update(cx, |this, _| this.save());
        }));
    }

    pub fn save(&self) {
        if let Err(e) = save_view(&self.views_dir, &self.pane_id, &self.thread.serialize()) {
            eprintln!("[wraithgrid] could not save chat {}: {e}", self.pane_id);
        }
    }

    fn markdown(&mut self, id: &str, text: &str, cx: &mut Context<Self>) -> Entity<TextViewState> {
        if let Some(state) = self.markdown.get(id) {
            let state = state.clone();
            // An extension of the text is parsed as an append, so streaming stays cheap.
            state.update(cx, |s, cx| s.set_text(text, cx));
            return state;
        }
        let state = cx.new(|cx| TextViewState::markdown(text, cx));
        self.markdown.insert(id.to_string(), state.clone());
        state
    }

    fn tool_card(&self, t: &ToolItem, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mono = crate::terminal::mono(cx);
        // Claude's questions (#127): one quiet line; the questions are in the card below.
        if let Some((asked, a)) = t
            .approval
            .as_ref()
            .and_then(|a| a.questions.as_ref().map(|q| (q.len(), a)))
            .filter(|(n, _)| *n > 0)
        {
            let waiting = a.decided.is_none();
            let word = match a.decided {
                None => "waiting for you",
                Some(ApprovalDecision::Deny) => "skipped",
                Some(_) => "answered",
            };
            return div()
                .px_2()
                .py_1()
                .flex()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(div().flex_1().child(format!(
                    "Claude {} {asked} {}",
                    if waiting { "asks" } else { "asked" },
                    if asked == 1 { "question" } else { "questions" }
                )))
                .child(div().text_xs().child(word))
                .into_any_element();
        }
        let preview: Option<AnyElement> = t.preview.as_ref().map(|p| match p {
            ToolPreview::Code { text } => div()
                .p_2()
                .rounded_md()
                .bg(theme.secondary)
                .font_family(mono.clone())
                .text_xs()
                .child(text.lines().take(30).collect::<Vec<_>>().join("\n"))
                .into_any_element(),
            ToolPreview::Diff {
                path,
                before,
                after,
            } => {
                let lines = before
                    .lines()
                    .take(40)
                    .map(|l| (false, l.to_string()))
                    .chain(after.lines().take(40).map(|l| (true, l.to_string())))
                    .map(|(add, l)| {
                        div()
                            .px_2()
                            .whitespace_nowrap()
                            .bg(if add {
                                theme.success.opacity(0.12)
                            } else {
                                theme.danger.opacity(0.12)
                            })
                            .child(format!("{}{l}", if add { "+" } else { "-" }))
                    });
                div()
                    .rounded_md()
                    .overflow_hidden()
                    .font_family(mono.clone())
                    .text_xs()
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_color(theme.muted_foreground)
                            .child(path.clone()),
                    )
                    .children(lines)
                    .into_any_element()
            }
            ToolPreview::Todos { items } => div()
                .flex()
                .flex_col()
                .text_sm()
                .children(items.iter().map(|i| {
                    let mark = match i.status {
                        wraithgrid_chat::TodoStatus::Done => "☑",
                        wraithgrid_chat::TodoStatus::Active => "▸",
                        wraithgrid_chat::TodoStatus::Pending => "☐",
                    };
                    div().child(format!("{mark} {}", i.text))
                }))
                .into_any_element(),
        });
        let output = t.output.as_ref().filter(|o| !o.trim().is_empty()).map(|o| {
            let shown: String = o.lines().take(12).collect::<Vec<_>>().join("\n");
            div()
                .p_2()
                .rounded_md()
                .bg(theme.secondary)
                .font_family(mono.clone())
                .text_xs()
                .when(t.is_error, |d| d.text_color(theme.danger))
                .child(shown)
        });
        let approval = t
            .approval
            .as_ref()
            .filter(|a| a.decided.is_none() && a.questions.is_none())
            .map(|a| {
                let button = |id: String, label: &'static str, primary: bool| {
                    div()
                        .id(SharedString::from(id))
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .text_sm()
                        .cursor_pointer()
                        .when(primary, |d| {
                            d.bg(theme.primary).text_color(theme.primary_foreground)
                        })
                        .when(!primary, |d| d.border_1().border_color(theme.border))
                        .child(label)
                };
                let (allow, always, deny) = (a.req_id.clone(), a.req_id.clone(), a.req_id.clone());
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(format!("allow-{}", a.req_id), "Allow", true).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.approve(allow.clone(), ApprovalDecision::Allow, cx)
                            },
                        )),
                    )
                    .when(a.can_always, |d| {
                        d.child(
                            button(format!("always-{}", a.req_id), "Always allow", false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.approve(always.clone(), ApprovalDecision::Always, cx)
                                }),
                            ),
                        )
                    })
                    .child(
                        button(format!("deny-{}", a.req_id), "Deny", false).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.approve(deny.clone(), ApprovalDecision::Deny, cx)
                            },
                        )),
                    )
            });
        let decided = t
            .approval
            .as_ref()
            .and_then(|a| a.decided)
            .map(|d| match d {
                ApprovalDecision::Allow => "allowed",
                ApprovalDecision::Always => "always allowed",
                ApprovalDecision::Deny => "denied",
            });
        div()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(if approval.is_some() {
                theme.warning
            } else {
                theme.border
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_sm()
                    .child(div().font_weight(FontWeight::MEDIUM).child(t.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(t.subtitle.clone()),
                    )
                    .when_some(decided, |d, s| {
                        d.child(div().text_xs().text_color(theme.muted_foreground).child(s))
                    })
                    .when(
                        t.output.is_none()
                            && t.approval.as_ref().is_none_or(|a| a.decided.is_some()),
                        |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("running…"),
                            )
                        },
                    ),
            )
            .children(preview)
            .children(output)
            .children(t.result.as_ref().map(|r| {
                div()
                    .text_sm()
                    .when(!r.ok, |d| d.text_color(theme.danger))
                    .child(r.text.clone())
            }))
            .children(approval)
            .into_any_element()
    }
}

impl EventEmitter<ChatViewEvent> for ChatView {}

impl Focusable for ChatView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let question = self.question_card(window, cx);
        let viewer = self.image_viewer(window, cx);
        let items = self.thread.items.clone();
        let mut rows: Vec<AnyElement> = Vec::with_capacity(items.len());
        for item in &items {
            let row = match item {
                ChatItem::User { text, images, .. } => {
                    // Leading reply quotes are drawn as quotes (#164).
                    let (quotes, body) = wraithgrid_chat::compose::split_quotes(text);
                    div()
                        .flex()
                        .justify_end()
                        .child(
                            div()
                                .max_w(relative(0.8))
                                .px_3()
                                .py_2()
                                .rounded_lg()
                                .bg(theme.secondary)
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(quotes.into_iter().map(|q| {
                                    div()
                                        .pl_2()
                                        .border_l_2()
                                        .border_color(theme.border)
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(q)
                                }))
                                .when(!body.is_empty(), |d| d.child(body))
                                .when(*images > 0, |d| {
                                    d.child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{images} image(s)")),
                                    )
                                }),
                        )
                        .into_any_element()
                }
                ChatItem::Assistant { id, text } => {
                    let state = self.markdown(id, text, cx);
                    div()
                        .child(TextView::new(&state).selectable(true))
                        .into_any_element()
                }
                ChatItem::Thinking { text, .. } => div()
                    .text_sm()
                    .italic()
                    .text_color(theme.muted_foreground)
                    .child(text.lines().take(4).collect::<Vec<_>>().join("\n"))
                    .into_any_element(),
                ChatItem::Tool(t) => self.tool_card(t, cx),
                ChatItem::Notice { level, text, .. } => match level {
                    ItemLevel::Divider => div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(div().flex_1().h(px(1.)).bg(theme.border))
                        .child(text.clone())
                        .child(div().flex_1().h(px(1.)).bg(theme.border))
                        .into_any_element(),
                    ItemLevel::Error => div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(text.clone())
                        .into_any_element(),
                    ItemLevel::Info => div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(text.clone())
                        .into_any_element(),
                },
                ChatItem::Turn {
                    cost_usd,
                    duration_ms,
                    error,
                    ..
                } => {
                    let mut parts = Vec::new();
                    if let Some(c) = cost_usd {
                        parts.push(format!("${c:.2}"));
                    }
                    if let Some(d) = duration_ms {
                        parts.push(format!("{:.1}s", d / 1000.0));
                    }
                    div()
                        .text_xs()
                        .text_color(if error.is_some() {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        })
                        .child(error.clone().unwrap_or_else(|| parts.join(" · ")))
                        .into_any_element()
                }
                ChatItem::Handoff {
                    messages, divider, ..
                } => div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().h(px(1.)).bg(theme.border))
                    .child(format!(
                        "{} · {messages} message{} carried over",
                        if divider.is_empty() {
                            "continued"
                        } else {
                            divider.as_str()
                        },
                        if *messages == 1 { "" } else { "s" }
                    ))
                    .child(div().flex_1().h(px(1.)).bg(theme.border))
                    .into_any_element(),
            };
            rows.push(row);
        }
        let busy = self.thread.busy;
        let elapsed = self.thread.turn_started_at.map(|t| (now_ms() - t) / 1000);
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::viewer_key))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                div()
                    .id(SharedString::from(format!("thread-{}", self.pane_id)))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::thread_mouse_up))
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when(rows.is_empty(), |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Start the conversation below."),
                        )
                    })
                    .children(rows),
            )
            .when_some(self.error.clone(), |d, e| {
                d.child(div().px_3().text_sm().text_color(theme.danger).child(e))
            })
            .children(self.queued_rows(cx))
            .children(question)
            .children(self.menu_list(cx))
            .children(self.picker_list(cx))
            .children(self.later_panel(cx))
            .children(self.quote_cards(cx))
            .when(!self.images.is_empty(), |d| d.child(self.image_row(cx)))
            .child(self.choices_row(cx))
            .child(
                div()
                    .p_2()
                    .flex()
                    .items_end()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .capture_key_down(cx.listener(Self::composer_key))
                            // The box binds these keys as actions; the menus, history, modes
                            // and image paste take them first.
                            .capture_action(cx.listener(|this, _: &input::MoveUp, window, cx| {
                                if this.composer_nav("up", window, cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, _: &input::MoveDown, window, cx| {
                                if this.composer_nav("down", window, cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(
                                |this, _: &input::IndentInline, window, cx| {
                                    if this.composer_nav("tab", window, cx) {
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                            .capture_action(cx.listener(|this, _: &input::Escape, window, cx| {
                                if this.composer_nav("escape", window, cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, _: &input::OutdentInline, _, cx| {
                                this.cycle_mode(cx);
                                cx.stop_propagation();
                            }))
                            .capture_action(cx.listener(|this, _: &input::Paste, _, cx| {
                                if this.paste_image(cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .child(Textarea::new(&self.composer)),
                    )
                    .child(self.later_button(cx))
                    .when(busy, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("working… {}s", elapsed.unwrap_or(0))),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("stop-{}", self.pane_id)))
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(theme.border)
                                .cursor_pointer()
                                .child("Stop")
                                .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                        )
                    })
                    .when(!busy, |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("send-{}", self.pane_id)))
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(theme.primary)
                                .text_color(theme.primary_foreground)
                                .cursor_pointer()
                                .child("Send")
                                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                        )
                    }),
            )
            .children(self.reply_button(cx))
            .children(viewer)
    }
}

impl ChatView {
    fn menu_list(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let theme = cx.theme().clone();
        let rows = menu.items.iter().enumerate().map(|(i, item)| {
            div()
                .id(SharedString::from(format!("menu-{i}")))
                .px_2()
                .py_1()
                .flex()
                .gap_2()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(i == menu.selected, |d| d.bg(theme.secondary))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(format!("{}{}", item.sigil, item.name)),
                )
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(item.description.clone()),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(m) = this.menu.as_mut() {
                        m.selected = i;
                    }
                    this.accept_menu(window, cx);
                }))
        });
        Some(
            div()
                .mx_2()
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .flex()
                .flex_col()
                .children(rows)
                .into_any_element(),
        )
    }

    fn picker_list(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.picker?;
        let theme = cx.theme().clone();
        let row = |id: String, label: String, detail: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .px_2()
                .py_1()
                .flex()
                .gap_2()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(on, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
                .child(div().font_weight(FontWeight::MEDIUM).child(label))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(detail),
                )
        };
        let rows: Vec<AnyElement> =
            match picker {
                Picker::Model => {
                    let models = if self.thread.models.is_empty() {
                        fallback_models()
                    } else {
                        self.thread.models.clone()
                    };
                    let current = self.current_model().map(|m| m.value);
                    models
                        .into_iter()
                        .map(|m| {
                            let value = m.value.clone();
                            row(
                                format!("model-{}", m.value),
                                m.label.clone(),
                                m.description.clone(),
                                current.as_ref() == Some(&m.value),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.choose_model(value.clone(), cx)
                            }))
                            .into_any_element()
                        })
                        .collect()
                }
                Picker::Effort => {
                    let efforts = self.current_model().map(|m| m.efforts).unwrap_or_default();
                    std::iter::once(None)
                        .chain(efforts.into_iter().map(Some))
                        .map(|e| {
                            let label = e.clone().unwrap_or_else(|| "Default".into());
                            let on = self.setup.effort == e;
                            row(format!("effort-{label}"), label, String::new(), on)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.choose_effort(e.clone(), cx)
                                }))
                                .into_any_element()
                        })
                        .collect()
                }
                Picker::Mode => {
                    let current = self
                        .thread
                        .mode
                        .or(self.setup.mode)
                        .unwrap_or(ChatMode::Default);
                    [
                        ChatMode::Default,
                        ChatMode::AcceptEdits,
                        ChatMode::Plan,
                        ChatMode::Auto,
                        ChatMode::DontAsk,
                        ChatMode::BypassPermissions,
                    ]
                    .into_iter()
                    .map(|m| {
                        row(
                            format!("mode-{}", m.as_str()),
                            m.label().into(),
                            m.hint().into(),
                            m == current,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.choose_mode(m, cx)))
                        .into_any_element()
                    })
                    .collect()
                }
            };
        Some(
            div()
                .mx_2()
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .flex()
                .flex_col()
                .children(rows)
                .into_any_element(),
        )
    }

    fn image_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let thumbs = self.images.iter().enumerate().map(|(i, (_, image))| {
            div()
                .id(SharedString::from(format!("thumb-{i}")))
                .relative()
                .size(px(56.))
                .rounded_md()
                .overflow_hidden()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                // A click previews it (#163); × only removes.
                .on_click(cx.listener(move |this, _, window, cx| this.open_viewer(i, window, cx)))
                .child(img(image.clone()).size_full())
                .child(
                    div()
                        .id(SharedString::from(format!("unattach-{i}")))
                        .absolute()
                        .top_0()
                        .right_0()
                        .px_1()
                        .bg(theme.background.opacity(0.8))
                        .text_xs()
                        .cursor_pointer()
                        .child("×")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if i < this.images.len() {
                                this.images.remove(i);
                            }
                            cx.notify();
                        })),
                )
        });
        div()
            .px_2()
            .pt_2()
            .flex()
            .gap_2()
            .children(thumbs)
            .into_any_element()
    }

    /// Model, effort and mode chips under the thread; each opens its picker.
    fn choices_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let chip = |id: &'static str, label: String, which: Picker| {
            div()
                .id(id)
                .px_2()
                .py(px(1.))
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .text_color(theme.muted_foreground)
                .hover(|d| d.bg(theme.secondary))
                .when(self.picker == Some(which), |d| d.bg(theme.secondary))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.picker = if this.picker == Some(which) {
                        None
                    } else {
                        Some(which)
                    };
                    cx.notify();
                }))
        };
        let model = self
            .current_model()
            .map_or_else(|| "Model".into(), |m| m.label);
        let effort = self
            .setup
            .effort
            .clone()
            .map_or_else(|| "Effort: default".into(), |e| format!("Effort: {e}"));
        let mode = self
            .thread
            .mode
            .or(self.setup.mode)
            .unwrap_or(ChatMode::Default)
            .label()
            .to_string();
        div()
            .px_2()
            .pt_1()
            .flex()
            .gap_1()
            .child(chip("pick-model", model, Picker::Model))
            .child(chip("pick-effort", effort, Picker::Effort))
            .child(chip("pick-mode", mode, Picker::Mode))
            .into_any_element()
    }
}

/// The chat crate's limits in the usage crate's terms (the same shape).
fn usage_limits(l: &wraithgrid_chat::AccountLimits) -> wraithgrid_usage::model::AccountLimits {
    let w = |w: Option<wraithgrid_chat::UsageWindow>| {
        w.map(|w| wraithgrid_usage::model::UsageWindow {
            utilization: w.utilization,
            resets_at: w.resets_at as i64,
        })
    };
    wraithgrid_usage::model::AccountLimits {
        five_hour: w(l.five_hour),
        seven_day: w(l.seven_day),
        at: l.at,
    }
}
