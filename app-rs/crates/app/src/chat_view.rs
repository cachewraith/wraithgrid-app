//! A chat pane (`ChatView.tsx`, ADR 0002): the conversation drawn from the `wraithgrid-chat`
//! thread, a composer, and the agent CLI running headless behind it. The saved view in
//! `<userData>/chats/<paneId>.json` is the Electron app's, so existing chats open as they were.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::a11y::A11y as _;
use futures::StreamExt as _;
use gpui_kit::component::input::{self, InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::{TextViewMotion, TextViewState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_chat::files::{rank_paths, system_git, FileLister};
use wraithgrid_chat::slash::{
    build_slash_items, file_item, file_query, leading_command, mention_query, rank_slash,
    slash_query, split_mentions, SlashGroup, SlashItem, SLASH_LIMIT,
};
use wraithgrid_chat::views::{load_view, save_view};
use wraithgrid_chat::{
    chat_adapter_for, now_ms, ApprovalDecision, ArgsOptions, ChatEvent, ChatItem, ChatLaunch,
    ChatSession, ChatStatus, ChatThread, ItemLevel,
};
use wraithgrid_chat::{
    ChatImage, ChatImageType, ChatMode, ChatModel, CHAT_IMAGES_MAX, CHAT_IMAGE_MAX,
};

mod accounts;
mod extras;
mod pickers;
mod transcript;
pub use accounts::AccountChoice;

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
    /// The account chip: carry the chat on as this account.
    ContinueAs(String),
    /// The account chip picked an account that is not signed in.
    SignIn(String),
}

/// The `/`, `$` or `@` menu over the composer (`SlashMenu`): its items, the sigil that opened
/// it, what was typed after it, and where the word it completes starts.
struct Menu {
    items: Vec<SlashItem>,
    selected: usize,
    sigil: char,
    query: String,
    start: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Model,
    Effort,
    Mode,
}

/// Why the CLI isn't running (`chat-bar err`): it could not start, or it stopped.
struct Stopped {
    message: Option<String>,
    spawn_error: bool,
}

/// claude's own aliases, offered before the CLI has listed its models. Other CLIs list theirs
/// once started; until then the picker says so.
fn fallback_models(cli: &str) -> Vec<ChatModel> {
    if cli != "claude" {
        return Vec::new();
    }
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
    /// Set while the CLI is not running because it failed to start or stopped.
    exit: Option<Stopped>,
    composer: Entity<TextareaState>,
    markdown: HashMap<String, Entity<TextViewState>>,
    views_dir: PathBuf,
    scroll: ScrollHandle,
    /// The `/` menu's list, kept so the highlighted row stays in view.
    menu_scroll: ScrollHandle,
    /// Under the box: the folder and branch.
    meta: Vec<String>,
    /// Redraws every second while a turn or background work runs (the elapsed times).
    _tick: Option<Task<()>>,
    error: Option<String>,
    menu: Option<Menu>,
    picker: Option<Picker>,
    /// The account chip's picker (#160), and what it offers.
    account_picker: Option<accounts::AccountPicker>,
    account_choices: Vec<AccountChoice>,
    current_account: Option<String>,
    /// Pasted images waiting to be sent, and their thumbnails.
    images: Vec<(ChatImage, Arc<Image>)>,
    /// Quotes, history, questions, image viewer and send-later state (`extras`).
    x: extras::Extras,
    _events: Option<Task<()>>,
    _save: Option<Task<()>>,
    _files: Option<Task<()>>,
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
                .placeholder("Starting…")
                .submit_on_enter(true)
                .auto_grow(1, 10)
        });
        let sub = Self::composer_events(&composer, window, cx);
        let scroll = ScrollHandle::new();
        scroll.scroll_to_bottom();
        // The CLI starts as the view opens (`requestStart`), so its models and commands are
        // there before the first message. After this update: a handoff made with the view
        // starts it itself.
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| this.start_on_open(cx));
        })
        .detach();
        ChatView {
            pane_id,
            focus: cx.focus_handle(),
            thread,
            setup,
            session: None,
            exit: None,
            composer,
            markdown: HashMap::new(),
            views_dir,
            scroll,
            menu_scroll: ScrollHandle::new(),
            _tick: None,
            meta: Vec::new(),
            error: None,
            menu: None,
            picker: None,
            account_picker: None,
            account_choices: Vec::new(),
            current_account: None,
            images: Vec::new(),
            x: extras::Extras::default(),
            _events: None,
            _save: None,
            _files: None,
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
                    if this.menu.as_ref().is_some_and(|m| !m.items.is_empty()) {
                        this.accept_menu(window, cx);
                    } else {
                        this.send(window, cx);
                    }
                }
                InputEvent::Change => {
                    // Typing ends walking through earlier prompts.
                    this.x.history = Default::default();
                    this.update_menu(cx);
                }
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

    /// The CLI's name for the box's placeholder and the first screen.
    fn cli_label(&self) -> &'static str {
        wraithgrid_core::agent_cli::AgentCli::parse(&self.setup.cli)
            .map_or("the agent", |c| c.spec().label)
    }

    /// The process is up and nothing stopped it: messages go straight to it.
    fn ready(&self) -> bool {
        self.exit.is_none() && self.session.as_ref().is_some_and(|s| s.is_running())
    }

    /// Starts the CLI when the view opens, unless something (a handoff) already did. A CLI
    /// that can't start says why in the bar above the box.
    fn start_on_open(&mut self, cx: &mut Context<Self>) {
        if self.session.as_ref().is_some_and(|s| s.is_running()) || self.exit.is_some() {
            return;
        }
        self.start_or_report(cx);
    }

    /// [`Self::start`], with a failure kept for the stopped bar.
    fn start_or_report(&mut self, cx: &mut Context<Self>) -> bool {
        match self.start(cx) {
            Ok(()) => {
                self.exit = None;
                true
            }
            Err(e) => {
                self.exit = Some(Stopped {
                    message: Some(e),
                    spawn_error: true,
                });
                cx.notify();
                false
            }
        }
    }

    /// The stopped bar's Restart: the CLI again, resuming the conversation.
    fn restart(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.session.take() {
            s.stop();
        }
        self.exit = None;
        self.error = None;
        self.start_or_report(cx);
        cx.notify();
    }

    /// Starts the CLI, resuming the pane's conversation when it has one.
    fn start(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        // Checked first: a CLI without a chat adapter says so whether or not it is installed.
        let adapter = chat_adapter_for(&self.setup.cli).ok_or_else(|| {
            format!(
                "The chat view for {} isn't ported yet. Open its CLI in a terminal.",
                self.setup.cli
            )
        })?;
        // Not found when the view was made (the login-shell PATH may not have been in yet):
        // look again with the PATH as it is now.
        if self.setup.program.is_none() {
            let env = crate::runtime::spawn_env_now();
            let bin = wraithgrid_core::agent_cli::AgentCli::parse(&self.setup.cli)
                .map_or(self.setup.cli.as_str(), |c| c.spec().bin);
            self.setup.program = wraithgrid_core::platform::find_on_path(
                bin,
                |k| env.get(k).cloned(),
                wraithgrid_core::pane_env::Os::current(),
            );
        }
        let s = &self.setup;
        let program = s
            .program
            .clone()
            .ok_or_else(|| format!("{} was not found on PATH. Set its path in Settings.", s.cli))?;
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
        cx.notify();
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
            // A new turn: the last one's error line has had its say.
            ChatEvent::Busy { busy: true } => self.error = None,
            ChatEvent::Text { .. } => self.x.text_at = now_ms(),
            ChatEvent::Exit {
                code,
                signal,
                error,
            } => {
                self.exit = Some(Stopped {
                    message: error.clone().or_else(|| match (code, signal) {
                        (Some(c), _) if *c != 0 => Some(format!("Exit code {c}.")),
                        (_, Some(s)) => Some(format!("Signal {s}.")),
                        _ => None,
                    }),
                    spawn_error: false,
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
        self.ensure_tick(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub fn set_meta(&mut self, meta: Vec<String>, cx: &mut Context<Self>) {
        if self.meta != meta {
            self.meta = meta;
            cx.notify();
        }
    }

    fn ensure_tick(&mut self, cx: &mut Context<Self>) {
        let live = |this: &Self| this.thread.busy || !this.thread.background.is_empty();
        if !live(self) || self._tick.is_some() {
            return;
        }
        self._tick = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            let go = this.update(cx, |this, cx| {
                cx.notify();
                let on = live(this);
                if !on {
                    this._tick = None;
                }
                on
            });
            if !matches!(go, Ok(true)) {
                break;
            }
        }));
    }

    /// Debug builds only: types `text` into the composer and sends it, for tests.
    pub fn send_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(debug_assertions) {
            self.set_box(text.to_string(), None, window, cx);
            self.send(window, cx);
        }
    }

    /// Puts `text` in the box with the caret at `caret` (the end when None). Setting a value
    /// leaves a multi-line box's caret at 0, and sends no change event.
    fn set_box(
        &mut self,
        text: String,
        caret: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at = caret.unwrap_or(text.len()).min(text.len());
        self.composer.update(cx, |c, cx| {
            c.set_value(text, window, cx);
            c.set_selected_range(at..at, cx);
        });
    }

    /// The box's text and caret.
    fn box_state(&self, cx: &App) -> (String, usize) {
        let state = self.composer.read(cx);
        let text = state.value().to_string();
        let caret = state.cursor().min(text.len());
        (text, caret)
    }

    /// Recomputes the `/`, `$` or `@` menu for the word the caret is in. A menu closed with
    /// Esc stays closed until the text changes.
    fn update_menu(&mut self, cx: &mut Context<Self>) {
        let (text, caret) = self.box_state(cx);
        if self.x.dismissed.as_deref() == Some(text.as_str()) {
            self.menu = None;
            return cx.notify();
        }
        self.x.dismissed = None;
        let all = build_slash_items(&self.thread.commands, &self.thread.terminal_only);
        let mentions: Vec<SlashItem> = all.iter().filter(|c| c.sigil == '$').cloned().collect();
        let next = if let Some(q) = slash_query(&text, caret) {
            // Shown even when nothing matches: "No command matches".
            Some(('/', q.to_string(), 0, rank_slash(&all, q, SLASH_LIMIT)))
        } else if let Some(w) = (!mentions.is_empty())
            .then(|| mention_query(&text, caret))
            .flatten()
        {
            let items = rank_slash(&mentions, &w.query, SLASH_LIMIT);
            (!items.is_empty()).then_some(('$', w.query, w.start, items))
        } else if let Some(w) = file_query(&text, caret) {
            self.list_files(cx);
            let items: Vec<SlashItem> = rank_paths(&self.x.paths, &w.query, 12)
                .iter()
                .map(|p| file_item(p))
                .collect();
            (!items.is_empty()).then_some(('@', w.query, w.start, items))
        } else {
            None
        };
        self.menu = next.map(|(sigil, query, start, items)| {
            // The highlight stays while the same word is typed at; a new word starts at the top.
            let selected = self
                .menu
                .as_ref()
                .filter(|m| m.sigil == sigil && m.query == query)
                .map_or(0, |m| m.selected.min(items.len().saturating_sub(1)));
            Menu {
                items,
                selected,
                sigil,
                query,
                start,
            }
        });
        cx.notify();
    }

    /// The pane's file list for the `@` menu, read off the UI thread (`git ls-files`); the
    /// menu is ranked again when it comes. Until then the last list is used.
    fn list_files(&mut self, cx: &mut Context<Self>) {
        if self._files.is_some() {
            return;
        }
        let cwd = self.setup.cwd.clone();
        self._files = Some(cx.spawn(async move |this, cx| {
            let paths = cx
                .background_executor()
                .spawn(async move { file_lister().list(&cwd) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this._files = None;
                if this.x.paths != paths {
                    this.x.paths = paths;
                    if this.menu.as_ref().is_none_or(|m| m.sigil == '@') {
                        this.update_menu(cx);
                    }
                }
            });
        }));
    }

    /// Puts the picked item in place of the word being typed (`pick` of Composer.tsx). One of
    /// Wraithgrid's own commands runs at once.
    fn accept_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        let Some(item) = menu.items.get(menu.selected).cloned() else {
            return;
        };
        let (text, caret) = self.box_state(cx);
        if item.group == SlashGroup::App {
            self.set_box(String::new(), None, window, cx);
            self.app_command(&item.name, window, cx);
            return;
        }
        let start = menu.start.min(caret);
        let rest = &text[caret..];
        let (next, at) = match item.sigil {
            '@' => {
                // A file replaces the `@word`; a folder stays open (`@src/`) to go on into it.
                let dir = item.name.ends_with('/');
                let head = format!(
                    "{}@{}{}",
                    &text[..start],
                    item.name,
                    if dir { "" } else { " " }
                );
                let tail = if dir { rest } else { rest.trim_start() };
                (format!("{head}{tail}"), head.len())
            }
            '$' => {
                let head = format!("{}${} ", &text[..start], item.name);
                (format!("{head}{}", rest.trim_start()), head.len())
            }
            // A `/` command is the text.
            _ => {
                let head = format!("/{} ", item.name);
                (head.clone(), head.len())
            }
        };
        self.set_box(next, Some(at), window, cx);
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        if item.sigil == '@' && item.name.ends_with('/') {
            self.update_menu(cx);
        }
        cx.notify();
    }

    /// Keys that reach the composer as keys (not bound by the text box): Ctrl+Shift+R.
    fn composer_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.extras_key(e, window, cx) {
            cx.stop_propagation();
        }
    }

    /// Any of the chips' lists is open.
    fn popover_open(&self) -> bool {
        self.picker.is_some() || self.account_picker.is_some() || self.x.later_open()
    }

    /// Closes the chips' lists (a click outside, Esc).
    fn close_popovers(&mut self, cx: &mut Context<Self>) {
        self.picker = None;
        self.account_picker = None;
        self.x.close_later();
        self.x.bypass_confirm = false;
        cx.notify();
    }

    /// ↑ / ↓ / Tab / Esc as the text box's actions: the `/` and `@` menu takes them first, then
    /// ↑ / ↓ walk earlier prompts; Esc closes a list, else stops a running turn. True when
    /// taken (the box doesn't move its caret).
    fn composer_nav(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(menu) = &mut self.menu {
            let n = menu.items.len();
            match key {
                "down" if n > 0 => menu.selected = (menu.selected + 1) % n,
                "up" if n > 0 => menu.selected = (menu.selected + n - 1) % n,
                "tab" if n > 0 => self.accept_menu(window, cx),
                "escape" => {
                    self.x.dismissed = Some(self.box_state(cx).0);
                    self.menu = None;
                }
                _ => return false,
            }
            cx.notify();
            return true;
        }
        match key {
            "escape" if self.popover_open() => {
                self.close_popovers(cx);
                true
            }
            "escape" if self.thread.busy => {
                self.stop(cx);
                true
            }
            "up" | "down" if !self.popover_open() => self.history_key(key == "up", window, cx),
            _ => false,
        }
    }

    /// An image on the clipboard is attached rather than pasted as text: all of them, up to
    /// the limits.
    fn paste_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        let images: Vec<Image> = item
            .entries()
            .iter()
            .filter_map(|e| match e {
                ClipboardEntry::Image(image) => Some(image.clone()),
                _ => None,
            })
            .collect();
        if images.is_empty() {
            return false;
        }
        for image in images {
            self.attach_image(image, cx);
        }
        true
    }

    /// Debug builds only: attaches files as if dropped, for tests.
    pub fn attach_dropped(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if cfg!(debug_assertions) {
            self.attach_paths(paths, cx);
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
        if self.images.len() >= CHAT_IMAGES_MAX {
            self.error = Some(format!("A message takes {CHAT_IMAGES_MAX} images at most."));
            return cx.notify();
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&image.bytes);
        if data.len() > CHAT_IMAGE_MAX {
            self.error = Some("That image is too big to send (about 5 MB at most).".into());
            return cx.notify();
        }
        self.images
            .push((ChatImage { media_type, data }, Arc::new(image)));
        cx.notify();
    }

    /// The models to pick from: the CLI's list, or claude's aliases before it has said.
    fn models(&self) -> Vec<ChatModel> {
        if self.thread.models.is_empty() {
            fallback_models(&self.setup.cli)
        } else {
            self.thread.models.clone()
        }
    }

    /// The chosen model as the list names it: the pick, else the CLI's `default` entry, else
    /// the first (what effort and auto mode go by).
    fn current_model(&self) -> Option<ChatModel> {
        let models = self.models();
        let want = self.setup.model.as_deref().unwrap_or("default");
        models
            .iter()
            .find(|m| m.value == want)
            .or_else(|| models.first())
            .cloned()
    }

    /// The model chip's label: the pick shows at once, else what the CLI runs.
    fn model_label(&self) -> String {
        let models = self.models();
        let want = self.setup.model.as_deref().unwrap_or("default");
        match models.iter().find(|m| m.value == want) {
            // claude's "Default (recommended)" names the model in its description.
            Some(m) if m.value == "default" => m
                .description
                .split('·')
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("Default")
                .to_string(),
            Some(m) => m.label.clone(),
            None => self
                .setup
                .model
                .clone()
                .or_else(|| self.thread.model.clone())
                .unwrap_or_else(|| "Model".into()),
        }
    }

    /// The permission mode as picked (the CLI's report follows it, and puts back a refusal).
    fn current_mode(&self) -> ChatMode {
        self.thread
            .mode
            .or(self.setup.mode)
            .unwrap_or(ChatMode::Default)
    }

    fn emit_choices(&mut self, cx: &mut Context<Self>) {
        cx.emit(ChatViewEvent::Choices {
            model: self.setup.model.clone(),
            effort: self.setup.effort.clone(),
            mode: self.setup.mode,
        });
    }

    /// The CLI couldn't switch over its pipe (agy, #41): an idle chat restarts with the new
    /// choice and resumes its conversation.
    fn restart_if_idle(&mut self, cx: &mut Context<Self>) {
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.is_running() && !s.is_busy())
        {
            if let Some(s) = self.session.take() {
                s.stop();
            }
            self.start_or_report(cx);
        }
    }

    /// `default` is the CLI's own "Default (recommended)" entry: picking it clears the choice.
    fn choose_model(&mut self, value: String, cx: &mut Context<Self>) {
        let live = self
            .session
            .as_ref()
            .filter(|s| s.is_running())
            .map(|s| s.set_model(&value));
        self.setup.model = (value != "default").then_some(value);
        self.picker = None;
        if live == Some(false) {
            self.restart_if_idle(cx);
        }
        self.emit_choices(cx);
        cx.notify();
    }

    /// Effort is a start option: an idle chat restarts with it and carries on.
    fn choose_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        if self.thread.busy {
            return;
        }
        self.setup.effort = effort;
        self.picker = None;
        self.restart_if_idle(cx);
        self.emit_choices(cx);
        cx.notify();
    }

    fn choose_mode(&mut self, mode: ChatMode, cx: &mut Context<Self>) {
        let live = self
            .session
            .as_ref()
            .filter(|s| s.is_running())
            .map(|s| s.set_mode(mode));
        self.setup.mode = Some(mode);
        self.thread.mode = Some(mode);
        self.picker = None;
        self.x.bypass_confirm = false;
        if live == Some(false) {
            self.restart_if_idle(cx);
        }
        self.emit_choices(cx);
        cx.notify();
    }

    /// `/new`, `/model`, `/effort`, `/mode`, `/usage`: Wraithgrid's own commands.
    fn app_command(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.account_picker = None;
        self.x.close_later();
        match name {
            "new" | "clear" => {
                if let Some(s) = self.session.take() {
                    s.stop();
                }
                self.thread = ChatThread::new();
                self.setup.session_id = None;
                self.markdown.clear();
                // Ids start over with the new thread: nothing kept by id may carry over.
                self.x.sent.clear();
                self.x.thumbs.clear();
                self.x.opened.clear();
                self.exit = None;
                self.error = None;
                self.schedule_save(cx);
                self.start_or_report(cx);
            }
            "model" => self.picker = Some(Picker::Model),
            "effort" => {
                if self.efforts().is_empty() {
                    self.error = Some("This model has no effort setting.".into());
                } else {
                    self.picker = Some(Picker::Effort);
                }
            }
            "mode" | "permissions" => self.picker = Some(Picker::Mode),
            "usage" => cx.emit(ChatViewEvent::OpenUsage),
            _ => return false,
        }
        self.set_box(String::new(), None, window, cx);
        self.menu = None;
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
        let running = self.session.as_ref().is_some_and(|s| s.is_running());
        if running || self.start_or_report(cx) {
            if let Some(session) = &self.session {
                session.send(text, &[], &[]);
                self.thread.busy = true;
                self.thread.turn_started_at = Some(now_ms());
            }
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// "New conversation" from the pane's menu: as `/new`.
    pub fn new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.app_command("new", window, cx);
    }

    /// A report handed to the chat ("Attach to chat" from a preview, #74): a removable chip
    /// over the box, sent after the message.
    pub fn add_to_draft(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let first = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("Report");
        let label = first.trim_start_matches(['#', ' ']);
        let label = if label.chars().count() > 60 {
            format!("{}…", wraithgrid_chat::take_chars(label, 60))
        } else {
            label.to_string()
        };
        self.x.notes.push((label, text.to_string()));
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }

    /// Debug builds only: types `text` without sending, so the menus can be looked at.
    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(debug_assertions) {
            self.set_box(text.to_string(), None, window, cx);
            self.update_menu(cx);
        }
    }

    /// Something to send: text, images, files, reports or quotes.
    fn can_send(&self, cx: &App) -> bool {
        !self.composer.read(cx).value().trim().is_empty()
            || !self.images.is_empty()
            || !self.x.quotes.is_empty()
            || !self.x.files.is_empty()
            || !self.x.notes.is_empty()
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let typed = self.composer.read(cx).value().trim().to_string();
        if self.x.quotes.is_empty() && self.x.notes.is_empty() {
            if let Some(cmd) = leading_command(&typed).filter(|c| c.rest.trim().is_empty()) {
                if self.app_command(&cmd.name, window, cx) {
                    return;
                }
            }
        }
        // Reply quotes go first, as markdown blockquotes (#164); handed-in reports after.
        let text = self.x.take_message(&typed);
        if !self.ready() {
            if let Some(s) = self.session.take() {
                s.stop();
            }
            if !self.start_or_report(cx) {
                return cx.notify();
            }
        }
        self.error = None;
        let (images, shown): (Vec<ChatImage>, Vec<Arc<Image>>) = self.images.drain(..).unzip();
        // Attached files, then `@path` mentions, go to the CLI as files, as the Electron chat
        // sends them.
        let mut files: Vec<String> = std::mem::take(&mut self.x.files);
        for m in split_mentions(&text).into_iter().filter(|p| p.mention) {
            let path = m.text.trim_start_matches('@').to_string();
            if !files.contains(&path) {
                files.push(path);
            }
        }
        files.truncate(wraithgrid_chat::CHAT_FILES_MAX);
        let id = self.thread.add_user_message(&text, images.len(), now_ms());
        if !shown.is_empty() {
            self.x.sent.insert(id.clone(), shown);
            self.make_thumbs(id, images.clone(), cx);
        }
        if let Some(session) = &self.session {
            if !session.send(&text, &images, &files) {
                self.error = Some("The message is too long to send.".into());
            }
        }
        self.set_box(String::new(), None, window, cx);
        self.menu = None;
        cx.emit(ChatViewEvent::Submitted { text });
        self.x.unstuck = false;
        self.ensure_tick(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// The sent images' thumbnails for the saved view (#50), made off the UI thread.
    fn make_thumbs(&mut self, id: String, images: Vec<ChatImage>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let thumbs: Vec<String> = cx
                .background_executor()
                .spawn(async move {
                    images
                        .iter()
                        .filter_map(wraithgrid_chat::attach::thumb_data_url)
                        .collect()
                })
                .await;
            if thumbs.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                if this.thread.set_user_thumbs(&id, thumbs) {
                    this.schedule_save(cx);
                }
            });
        })
        .detach();
    }

    /// Sends `text` again (a message the limit refused, after a switch).
    pub fn resend(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_box(text.to_string(), None, window, cx);
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
        // Streamed words fade in as they come (#62); text already there shows at once.
        let state = cx.new(|cx| {
            TextViewState::markdown(text, cx).motion(
                TextViewMotion::default()
                    .with_stream_fade(Duration::from_millis(240))
                    .with_stream_fade_stagger(Duration::from_millis(16)),
            )
        });
        self.markdown.insert(id.to_string(), state.clone());
        state
    }

    /// The box's placeholder for the chat's state (`Message Claude Code…`, `Starting…`).
    fn sync_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let want = if self.asking() {
            "Answer the questions above, or Skip to type instead…".to_string()
        } else if self.exit.is_none() && !self.ready() {
            "Starting…".to_string()
        } else {
            format!("Message {}…", self.cli_label())
        };
        if self.x.placeholder != want {
            self.x.placeholder = want.clone();
            self.composer
                .update(cx, |c, cx| c.set_placeholder(want, window, cx));
        }
    }

    /// The thread's scroll: an upward wheel stops following the answer; reaching the bottom
    /// again follows it. A selection's Reply button goes away (it would point elsewhere).
    fn thread_scrolled(&mut self, e: &ScrollEvent, cx: &mut Context<Self>) {
        let dy = e.delta.pixel_delta(px(16.)).y;
        if dy > px(0.) && self.scroll.max_offset().y > px(0.) {
            self.x.unstuck = true;
        } else if dy < px(0.) {
            let off = -self.scroll.offset().y;
            if off - dy >= self.scroll.max_offset().y - px(24.) {
                self.x.unstuck = false;
            }
        }
        self.x.reply_at = None;
        cx.notify();
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
        self.sync_placeholder(window, cx);
        // Following the answer: the end stays in view as it grows, and as the box, a question
        // or an approval takes room under it.
        if self.x.unstuck && self.scroll.max_offset().y <= px(0.) {
            self.x.unstuck = false;
        }
        if !self.x.unstuck {
            self.scroll.scroll_to_bottom();
        }
        let question = self.question_card(window, cx);
        let viewer = self.image_viewer(window, cx);
        let rows = self.transcript_rows(cx);
        let empty = rows.is_empty();
        let hello = empty.then(|| self.hello(cx));
        let working = self.working_line(cx);
        let jump = self.jump_button(cx);
        let footer = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .children(self.error_bar(cx))
            .when(!self.x.queued.is_empty(), |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("queued-{}", self.pane_id)))
                        .max_h(px(120.))
                        .overflow_y_scroll()
                        .children(self.queued_rows(cx)),
                )
            })
            .children(question)
            .children(self.approval_bar(cx))
            .children(self.stopped_bar(cx))
            .child(self.composer_box(cx))
            .children(self.folder_line(cx));
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::viewer_key))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id(SharedString::from(format!("thread-{}", self.pane_id)))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::thread_mouse_up))
                            .on_scroll(cx.listener(|this, e: &ScrollEvent, _, cx| {
                                this.thread_scrolled(e, cx)
                            }))
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .children(hello)
                            .children(rows)
                            .children(working)
                            .children(self.background_tasks(cx)),
                    )
                    .children(jump),
            )
            .child(footer)
            .children(self.popover_scrim(window, cx))
            .children(self.reply_button(cx))
            .children(viewer)
    }
}

impl ChatView {
    /// The error line over the box, with ×; it clears on the next turn or a restart.
    fn error_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let e = self.error.clone()?;
        let theme = cx.theme();
        Some(
            div()
                .id(SharedString::from(format!("error-{}", self.pane_id)))
                .role(Role::Alert)
                .aria_label(e.clone())
                .mx_2()
                .mt_2()
                .px_3()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(theme.danger.opacity(0.5))
                .bg(theme.danger.opacity(0.08))
                .text_sm()
                .text_color(theme.danger)
                .child(div().flex_1().min_w_0().child(e))
                .child(
                    div()
                        .id(SharedString::from(format!("error-x-{}", self.pane_id)))
                        .a11y_button("Dismiss")
                        .px_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.secondary))
                        .child("×")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// "The agent stopped." with Restart, or "… is not signed in" with Sign in
    /// (`chat-bar`).
    fn stopped_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let bar = |id: &str, err: bool| {
            div()
                .id(SharedString::from(format!("{id}-{}", self.pane_id)))
                .role(Role::Status)
                .mx_2()
                .mt_2()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(if err {
                    theme.danger.opacity(0.5)
                } else {
                    theme.border
                })
                .bg(if err {
                    theme.danger.opacity(0.08)
                } else {
                    theme.secondary
                })
                .text_sm()
        };
        let button = |id: &str, label: &'static str| {
            div()
                .id(SharedString::from(format!("{id}-{}", self.pane_id)))
                .a11y_button(label)
                .flex_none()
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(label)
        };
        if let Some(stop) = &self.exit {
            let ported = chat_adapter_for(&self.setup.cli).is_some();
            let text = if stop.spawn_error {
                stop.message
                    .clone()
                    .unwrap_or_else(|| "The agent could not start.".into())
            } else {
                "The agent stopped.".into()
            };
            let detail = (!stop.spawn_error).then(|| stop.message.clone()).flatten();
            return Some(
                bar("stopped", true)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .child(text)
                            .when_some(detail, |d, m| {
                                d.child(
                                    div()
                                        .font_family(crate::terminal::mono(cx))
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(m),
                                )
                            }),
                    )
                    .when(ported, |d| {
                        d.child(
                            button("restart", "↻ Restart")
                                .on_click(cx.listener(|this, _, _, cx| this.restart(cx))),
                        )
                    })
                    .into_any_element(),
            );
        }
        let account = self.current_choice().filter(|a| !a.signed_in)?.clone();
        Some(
            bar("signin", false)
                .child(div().flex_1().min_w_0().child(format!(
                    "{} is not signed in. Sign in once, then come back.",
                    account.name
                )))
                .child(
                    button("signin-btn", "Sign in").on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ChatViewEvent::SignIn(account.id.clone()))
                    })),
                )
                .into_any_element(),
        )
    }

    /// The box (`cc`): the `/` menu over it; quotes, reports, files and images; the text;
    /// then the chips, attach, send later, stop and send.
    fn composer_box(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let busy = self.thread.busy;
        let can_send = self.can_send(cx);
        let menu = self.menu_list(cx);
        let images = (!self.images.is_empty()).then(|| self.image_row(cx));
        div()
            .relative()
            .px_2()
            .pt_2()
            .child(
                div()
                    .id(SharedString::from(format!("composer-{}", self.pane_id)))
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    // Claude's questions are open above: the box dims; it still works.
                    .when(self.asking(), |d| d.opacity(0.6))
                    // Files dropped here are attached (like a paste for images).
                    .drag_over::<ExternalPaths>(|d, _, _, cx| d.border_color(cx.theme().ring))
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                        this.attach_paths(paths.paths(), cx)
                    }))
                    .children(self.quote_cards(cx))
                    .children(self.note_chips(cx))
                    .children(self.file_chips(cx))
                    .children(images)
                    .child(
                        div()
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
                                if this.menu.is_none() {
                                    this.cycle_mode(cx);
                                }
                                cx.stop_propagation();
                            }))
                            .capture_action(cx.listener(|this, _: &input::Paste, _, cx| {
                                if this.paste_image(cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .child(Textarea::new(&self.composer)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.choices_row(cx))
                            .child(self.attach_button(cx))
                            .child(self.later_button(cx))
                            .when(busy, |d| {
                                d.child(
                                    div()
                                        .id(SharedString::from(format!("stop-{}", self.pane_id)))
                                        .a11y_button("Stop (Esc)")
                                        .px_3()
                                        .py_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(theme.border)
                                        .cursor_pointer()
                                        .hover(|d| d.bg(theme.secondary))
                                        .tooltip(|window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new("Stop (Esc)")
                                                .build(window, cx)
                                        })
                                        .child("■ Stop")
                                        .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                                )
                            })
                            .child({
                                // While a turn runs, the message waits its turn (Queue).
                                let label = if busy { "Queue" } else { "Send" };
                                div()
                                    .id(SharedString::from(format!("send-{}", self.pane_id)))
                                    .a11y_button(label)
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(theme.primary)
                                    .text_color(theme.primary_foreground)
                                    .when(can_send, |d| d.cursor_pointer())
                                    .when(!can_send, |d| d.opacity(0.5))
                                    .tooltip(move |window, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(if busy {
                                            "Queue message (Enter)"
                                        } else {
                                            "Send (Enter)"
                                        })
                                        .build(window, cx)
                                    })
                                    .child(label)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.send(window, cx)),
                                    )
                            }),
                    ),
            )
            .children(menu)
            .into_any_element()
    }

    /// Under the box: the folder and the branch, each shortened on its own (`cc-folder`).
    fn folder_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.meta.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let n = self.meta.len();
        Some(
            div()
                .px_3()
                .py_1()
                .flex()
                .gap_3()
                .text_xs()
                .text_color(theme.muted_foreground)
                .children(self.meta.iter().enumerate().map(|(i, m)| {
                    div()
                        .min_w_0()
                        .truncate()
                        // The folder gives way first; the branch keeps its name when it can.
                        .when(i + 1 < n, |d| d.flex_shrink(1.))
                        .when(i + 1 == n && n > 1, |d| {
                            d.flex_shrink_0().max_w(relative(0.5))
                        })
                        .child(m.clone())
                }))
                .into_any_element(),
        )
    }

    /// The `/`, `$` or `@` menu, over the box (`SlashMenu`): group headings on a bare `/`,
    /// argument hints and sources, "No command matches" when nothing does.
    fn menu_list(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let theme = cx.theme().clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut selected_row = 0;
        if menu.items.is_empty() {
            let what = match menu.sigil {
                '$' => "skill",
                '@' => "file",
                _ => "command",
            };
            rows.push(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!("No {what} matches “{}{}”.", menu.sigil, menu.query))
                    .into_any_element(),
            );
        }
        for (i, item) in menu.items.iter().enumerate() {
            let head = (menu.query.is_empty() && (i == 0 || menu.items[i - 1].group != item.group))
                .then(|| item.group.label());
            if let Some(head) = head {
                rows.push(
                    div()
                        .px_2()
                        .pt_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(head)
                        .into_any_element(),
                );
            }
            if i == menu.selected {
                selected_row = rows.len();
            }
            let on = i == menu.selected;
            rows.push(
                div()
                    .id(SharedString::from(format!("menu-{i}")))
                    .role(Role::ListItem)
                    .aria_label(format!("{}{}", item.sigil, item.name))
                    .aria_selected(on)
                    .when(on, |d| d.aria_active_descendant())
                    .px_2()
                    .py_1()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme.secondary))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("{}{}", item.sigil, item.name))
                            .when(!item.argument_hint.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .font_weight(FontWeight::NORMAL)
                                        .child(format!(" {}", item.argument_hint)),
                                )
                            }),
                    )
                    .when_some(item.source.clone(), |d, s| {
                        d.child(
                            div()
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(theme.border)
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(s),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(item.description.clone()),
                    )
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if let Some(m) = this.menu.as_mut().filter(|m| m.selected != i) {
                            m.selected = i;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(m) = this.menu.as_mut() {
                            m.selected = i;
                        }
                        this.accept_menu(window, cx);
                    }))
                    .into_any_element(),
            );
        }
        self.menu_scroll.scroll_to_item(selected_row);
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(relative(1.))
                .px_2()
                .child(
                    div()
                        .id(SharedString::from(format!("menu-list-{}", self.pane_id)))
                        .role(Role::List)
                        .aria_label(if menu.sigil == '@' {
                            "Files"
                        } else {
                            "Commands"
                        })
                        .occlude()
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .track_scroll(&self.menu_scroll)
                        .p_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    fn image_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let thumbs = self.images.iter().enumerate().map(|(i, (_, image))| {
            div()
                .id(SharedString::from(format!("thumb-{i}")))
                .a11y_button(format!("Preview image {} of {}", i + 1, self.images.len()))
                .relative()
                .size(px(56.))
                .rounded_md()
                .overflow_hidden()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                // A click previews it (#163); × only removes.
                .on_click(cx.listener(move |this, _, window, cx| {
                    let all = this.images.iter().map(|(_, i)| i.clone()).collect();
                    this.open_viewer(all, i, window, cx)
                }))
                .child(img(image.clone()).size_full())
                .child(
                    div()
                        .id(SharedString::from(format!("unattach-{i}")))
                        .a11y_button(format!("Remove image {}", i + 1))
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
        div().flex().gap_2().children(thumbs).into_any_element()
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
