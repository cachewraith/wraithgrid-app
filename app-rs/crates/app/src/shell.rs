//! The window (`src/renderer/app/App.tsx`, `Sidebar.tsx`, `TopBar.tsx`, `PaneGrid.tsx`): title
//! bar, sidebar with projects and accounts, and the active project's panes in its saved split
//! layout. The config is the state: every change goes through the store and is saved shortly
//! after, in the same file and shape the Electron app uses.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::config::{Config, LayoutNode, Pane};
use wraithgrid_core::host::client::PaneHost;
use wraithgrid_core::ids::new_id;
use wraithgrid_core::layout::{pane_ids, place_new_pane, remove_pane, set_sizes_at, shown_layout};
use wraithgrid_core::pane_env::Os;
use wraithgrid_core::paths::{base_name, home_dir};
use wraithgrid_core::platform::{detect_desktop, Chrome};
use wraithgrid_core::shortcuts::{bindings, match_shortcut, Action, Direction, Press};
use wraithgrid_core::spawn::{resolve_spawn, ResolveContext, SpawnSpec};
use wraithgrid_core::store::ConfigStore;
use wraithgrid_core::types::{step_zoom, CloseMode, PaneView, ShortcutId, SplitDir};
use wraithgrid_core::update;

use crate::accounts_view::{AccountsEvent, AccountsView};
use crate::changes::ChangesPanel;
use crate::chat_view::{ChatSetup, ChatView, ChatViewEvent};
use crate::dialogs::{
    ChoiceDialog, ChoiceEvent, NewPaneDialog, NewPaneEvent, NewProjectDialog, NewProjectEvent,
    Palette, PaletteAction, PaletteEvent, ProjectDialog, ProjectEvent, QuitDialog, QuitEvent,
};
use crate::mods_view::{ModsView, ModsViewEvent};
use crate::project_view::{ProjectPageEvent, ProjectView};
use crate::runtime::{spawn_env, Router};
use crate::scheduled_view::{ScheduledEvent, ScheduledView};
use crate::settings::{SettingsEvent, SettingsView};
use crate::terminal::{
    PaneResizer, PaneStarter, PaneWriter, TermCore, TerminalEvent, TerminalStyle, TerminalView,
};
use crate::usage_view::{UsageView, UsageViewEvent};

const TITLE_BAR: f32 = 36.0;
const SAVE_DELAY: Duration = Duration::from_millis(300);

pub struct Shell {
    store: ConfigStore,
    host: Arc<PaneHost>,
    router: Arc<Router>,
    chrome: Chrome,
    terminals: HashMap<String, Entity<TerminalView>>,
    chats: HashMap<String, Entity<ChatView>>,
    /// Each chat's status, for its header dot.
    chat_states: HashMap<String, wraithgrid_chat::ChatStatus>,
    /// Scheduled runs started by this launch (`live_key`); others left running are stale.
    live_runs: std::collections::HashSet<String>,
    /// Chat panes opened as terminals this session (until the chat view is ported).
    as_terminal: Vec<String>,
    focused: Option<String>,
    /// The pane shown alone (Zoom pane), until toggled back to the grid.
    zoomed: Option<String>,
    /// Where each split and pane was drawn last frame, for divider drags and focus moves.
    split_bounds: Rc<RefCell<HashMap<Vec<usize>, Bounds<Pixels>>>>,
    pane_bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    drag: Option<DividerDrag>,
    /// Each pane's state in the host (running, idle, blocked, exited), polled.
    states: HashMap<String, String>,
    /// When each pane started its current run, for "done" notifications.
    running_since: HashMap<String, std::time::Instant>,
    window_active: bool,
    _poll: Option<Task<()>>,
    save_task: Option<Task<()>>,
    gui_task: Option<Task<()>>,
    /// The overlay on top of the window, if any.
    modal: Option<Modal>,
    /// Settings, shown in place of the panes while open.
    settings_view: Option<Entity<SettingsView>>,
    accounts_view: Option<Entity<AccountsView>>,
    usage_view: Option<Entity<UsageView>>,
    mods_view: Option<Entity<ModsView>>,
    scheduled_view: Option<Entity<ScheduledView>>,
    project_view: Option<Entity<ProjectView>>,
    update: UpdateState,
    /// The Changes panel beside the panes, for one folder.
    changes: Option<Entity<ChangesPanel>>,
    /// Each shown pane folder's branch and changed count, polled.
    git_heads: HashMap<String, wraithgrid_git::GitStatus>,
    /// usage.json, shared with the poll; and the latest limits per account.
    usage: Arc<std::sync::Mutex<wraithgrid_usage::store::UsageStore>>,
    limits: std::collections::BTreeMap<String, wraithgrid_usage::model::AccountLimits>,
    /// Who each pane belongs to, for the usage store's turn records.
    owners: Arc<std::sync::Mutex<HashMap<String, wraithgrid_usage::store::PaneOwner>>>,
    _usage_save: Option<Task<()>>,
    _usage_task: Option<Task<()>>,
    _schedule_task: Option<Task<()>>,
    _git_task: Option<Task<()>>,
    _update_task: Option<Task<()>>,
    /// Set when the Electron app runs on the same config: nothing may start or be saved.
    blocked: Option<String>,
}

impl Shell {
    pub fn new(
        store: ConfigStore,
        host: Arc<PaneHost>,
        router: Arc<Router>,
        blocked: Option<String>,
    ) -> Self {
        let chrome = detect_desktop(Os::current(), |k| std::env::var(k).ok()).chrome;
        let owners: Arc<std::sync::Mutex<HashMap<String, wraithgrid_usage::store::PaneOwner>>> =
            Arc::new(std::sync::Mutex::new(pane_owners(store.get())));
        Shell {
            store,
            host,
            router,
            chrome,
            terminals: HashMap::new(),
            chats: HashMap::new(),
            chat_states: HashMap::new(),
            live_runs: Default::default(),
            as_terminal: Vec::new(),
            focused: None,
            zoomed: None,
            split_bounds: Rc::default(),
            pane_bounds: Rc::default(),
            drag: None,
            states: HashMap::new(),
            running_since: HashMap::new(),
            window_active: true,
            _poll: None,
            save_task: None,
            gui_task: None,
            modal: None,
            settings_view: None,
            accounts_view: None,
            usage_view: None,
            mods_view: None,
            scheduled_view: None,
            project_view: None,
            update: UpdateState::Idle,
            changes: None,
            git_heads: HashMap::new(),
            usage: Arc::new(std::sync::Mutex::new(
                wraithgrid_usage::store::UsageStore::new(
                    wraithgrid_usage::store::UsageStoreDeps::new(
                        wraithgrid_core::paths::current_user_data_dir().join("usage.json"),
                        Box::new({
                            let owners = owners.clone();
                            move |pane| owners.lock().unwrap().get(pane).cloned()
                        }),
                    ),
                ),
            )),
            owners,
            _usage_save: None,
            limits: Default::default(),
            _usage_task: None,
            _schedule_task: None,
            _git_task: None,
            _update_task: None,
            blocked,
        }
    }

    /// Polls the host for pane states once a second (it matches each CLI's approval prompt).
    pub fn start_polling(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        cx.observe_window_activation(window, |this, window, _| {
            this.window_active = window.is_window_active()
        })
        .detach();
        cx.observe_window_appearance(window, |this, window, cx| {
            let resolved = crate::theme::resolve(this.config(), crate::theme::system_dark(window));
            crate::theme::apply(&resolved, Some(window), cx);
        })
        .detach();
        self._git_task = Some(self.start_git_poll(cx));
        let host = self.host.clone();
        self._poll = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let host = host.clone();
            let panes = cx.background_spawn(async move { host.list() }).await;
            let states: HashMap<String, String> =
                panes.into_iter().map(|p| (p.pane_id, p.state)).collect();
            if this
                .update(cx, |this, cx| {
                    if this.states != states {
                        this.state_changes(&states, cx);
                        this.states = states;
                        this.refresh_project_view(cx);
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        }));
    }

    /// Notifies when a pane the user isn't looking at finishes a run of real work (8 s or
    /// more) or starts asking for approval, as the Electron app does.
    fn state_changes(&mut self, next: &HashMap<String, String>, cx: &mut Context<Self>) {
        const MIN_RUN: Duration = Duration::from_secs(8);
        let now = std::time::Instant::now();
        for (id, to) in next {
            let from = self.states.get(id).map(String::as_str);
            if to == "running" && from != Some("running") {
                self.running_since.insert(id.clone(), now);
            }
            let asks = to == "blocked" && from != Some("blocked");
            let ran = self
                .running_since
                .get(id)
                .is_some_and(|t| now.duration_since(*t) >= MIN_RUN);
            let done = to == "idle" && from == Some("running") && ran;
            if to != "running" {
                self.running_since.remove(id);
            }
            if (asks || done) && self.config().settings.notify_panes && !self.attending(id) {
                self.notify_pane(id, asks, cx);
            }
        }
    }

    /// Is the user looking at this pane right now? Then no notification is needed.
    fn attending(&self, pane_id: &str) -> bool {
        let ws = self.config().active();
        let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
        let visible = match &self.zoomed {
            Some(z) => z == pane_id,
            None => pane_ids(shown_layout(ws.layout.as_ref(), &ids).as_ref())
                .iter()
                .any(|p| p == pane_id),
        };
        self.window_active && self.settings_view.is_none() && visible
    }

    fn notify_pane(&self, pane_id: &str, asks: bool, cx: &mut Context<Self>) {
        let c = self.config();
        let Some((ws, pane)) = c
            .workspaces
            .iter()
            .find_map(|w| w.panes.iter().find(|p| p.id == pane_id).map(|p| (w, p)))
        else {
            return;
        };
        if pane.shell {
            return;
        }
        let bin = pane
            .account_id
            .as_deref()
            .and_then(|id| c.account(id))
            .map_or(AgentCli::Claude, |a| a.cli)
            .spec()
            .bin;
        let place = format!("{} · {}", pane_label(c, pane), ws.name);
        let (title, body) = if asks {
            (
                format!("{place} needs approval"),
                format!("{bin} is waiting for your answer."),
            )
        } else {
            (
                format!("{place} is done"),
                format!("{bin} finished and is waiting for you."),
            )
        };
        cx.show_system_notification(SystemNotification {
            tag: pane_id.to_string().into(),
            title: title.into(),
            body: body.into(),
            actions: vec![],
        });
    }

    fn config(&self) -> &Config {
        self.store.get()
    }

    /// Changes the config and saves it shortly after (the Electron store's debounce).
    fn update(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Config)) {
        if self.blocked.is_some() {
            return;
        }
        self.store.update(change);
        *self.owners.lock().unwrap() = pane_owners(self.store.get());
        if let Some(view) = self.scheduled_view.clone() {
            let config = self.store.get().clone();
            view.update(cx, |v, cx| v.set_config(config, cx));
        }
        self.refresh_project_view(cx);
        cx.notify();
        if self.store.is_dirty() {
            self.save_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SAVE_DELAY).await;
                let _ = this.update(cx, |this, _| this.store.save());
            }));
        }
    }

    /// Writes a pending change now, and every open chat's view. Called on quit.
    pub fn flush(&mut self) {
        if self.blocked.is_none() {
            self.store.save();
        }
    }

    pub fn flush_chats(&self, cx: &App) {
        for chat in self.chats.values() {
            chat.read(cx).save();
        }
    }

    /// The pane's chat view, made the first time it is shown from its saved view.
    fn chat(
        &mut self,
        pane: &Pane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ChatView> {
        if let Some(view) = self.chats.get(&pane.id) {
            return view.clone();
        }
        let config = self.config().clone();
        let account = pane.account_id.as_deref().and_then(|id| config.account(id));
        let env = crate::runtime::spawn_env_now();
        let home = home_dir();
        let cli = account.map_or(AgentCli::Claude, |a| a.cli);
        let mut pane_env =
            wraithgrid_core::pane_env::build_pane_env(wraithgrid_core::pane_env::PaneEnvInput {
                base: &env,
                shell: false,
                cli,
                config_dir: account.map(|a| a.config_dir.as_str()),
                home: &home,
                mod_dirs: &config.settings.mod_dirs,
                os: Os::current(),
            });
        pane_env.insert("WRAITHGRID_PANE".into(), pane.id.clone());
        let setup = ChatSetup {
            cli: cli.as_str().into(),
            // Settings' path for the CLI, as `resolveSpawn` picks it; else found on PATH.
            program: if cli == AgentCli::Claude {
                crate::mods_glue::claude_binary(&config, &env)
            } else {
                config
                    .settings
                    .cli_paths
                    .iter()
                    .find(|(c, p)| *c == cli && !p.trim().is_empty())
                    .map(|(_, p)| wraithgrid_core::paths::resolve_user_path(p.trim(), &home))
            },
            cwd: wraithgrid_core::paths::resolve_user_path(&pane.cwd, &home),
            env: pane_env,
            config_dir: account
                .map(|a| wraithgrid_core::paths::resolve_user_path(&a.config_dir, &home)),
            session_id: pane.chat_session_id.clone(),
            model: pane.model.clone(),
            effort: pane.effort.map(|e| e.as_str().to_string()),
            mode: pane
                .chat_mode
                .and_then(|m| serde_json::from_value(serde_json::Value::from(m.as_str())).ok()),
        };
        if let Some(account) = account {
            crate::mods_glue::sync_before_start(&config, &account.id);
        }
        let views_dir = wraithgrid_core::paths::current_user_data_dir().join("chats");
        let pane_id = pane.id.clone();
        let view = cx.new(|cx| ChatView::new(pane_id.clone(), setup, views_dir, window, cx));
        cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &ChatViewEvent, window, cx| match event {
                ChatViewEvent::Submitted { text } => {
                    let title = title_from_message(text);
                    let id = pane_id.clone();
                    this.update(cx, |c| {
                        for p in c
                            .workspaces
                            .iter_mut()
                            .flat_map(|w| w.panes.iter_mut())
                            .filter(|p| p.id == id)
                        {
                            // A new chat is named after its first message, like a chat app's history.
                            if !p.has_conversation {
                                p.title = title.clone();
                            }
                            p.has_conversation = true;
                        }
                    });
                }
                ChatViewEvent::SessionId(sid) => {
                    let (id, sid) = (pane_id.clone(), sid.clone());
                    this.update(cx, |c| {
                        for p in c
                            .workspaces
                            .iter_mut()
                            .flat_map(|w| w.panes.iter_mut())
                            .filter(|p| p.id == id)
                        {
                            p.chat_session_id = Some(sid.clone());
                        }
                    });
                }
                ChatViewEvent::Status(status) => {
                    this.chat_states.insert(pane_id.clone(), *status);
                    cx.notify();
                }
                ChatViewEvent::Choices {
                    model,
                    effort,
                    mode,
                } => {
                    let id = pane_id.clone();
                    let model = model
                        .clone()
                        .filter(|m| wraithgrid_core::config::is_chat_model(m));
                    let effort = effort
                        .as_deref()
                        .and_then(wraithgrid_core::types::ChatEffort::parse);
                    let mode =
                        mode.and_then(|m| wraithgrid_core::types::ChatMode::parse(m.as_str()));
                    this.update(cx, |c| {
                        for p in c
                            .workspaces
                            .iter_mut()
                            .flat_map(|w| w.panes.iter_mut())
                            .filter(|p| p.id == id)
                        {
                            p.model = model.clone();
                            p.effort = effort;
                            p.chat_mode = mode;
                        }
                    });
                }
                ChatViewEvent::OpenUsage => this.open_usage(window, cx),
                ChatViewEvent::Usage(e) => this.record_usage(&pane_id, e, cx),
                ChatViewEvent::TurnEnded { error } => {
                    this.settle_runs(cx);
                    this.maybe_switch(&pane_id, error.clone(), window, cx)
                }
            },
        )
        .detach();
        self.chats.insert(pane.id.clone(), view.clone());
        view
    }

    fn select_workspace(&mut self, id: String, cx: &mut Context<Self>) {
        self.update(cx, |c| c.active_workspace = id);
    }

    /// A new pane in the active project: a shell (`account` None) or an agent's terminal.
    fn new_pane(&mut self, account: Option<String>, cx: &mut Context<Self>) {
        let ws = self.config().active();
        let cwd = if ws.folder.is_empty() {
            self.config().settings.default_cwd.clone()
        } else {
            ws.folder.clone()
        };
        let shell = account.is_none();
        self.add_pane(None, account, cwd, vec![], shell, false, cx);
    }

    fn open_new_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.config().clone();
        let dialog = cx.new(|cx| NewPaneDialog::new(&config, window, cx));
        cx.subscribe_in(
            &dialog,
            window,
            |this, _, event: &NewPaneEvent, window, cx| {
                if let NewPaneEvent::Start { account, cwd, args } = event {
                    let shell = account.is_none();
                    let id = this.add_pane(
                        None,
                        account.clone(),
                        cwd.clone(),
                        args.clone(),
                        shell,
                        true,
                        cx,
                    );
                    this.focused = Some(id);
                }
                this.close_modal(window, cx);
            },
        )
        .detach();
        self.modal = Some(Modal::NewPane(dialog));
        cx.notify();
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.config().clone();
        let palette = cx.new(|cx| Palette::new(&config, window, cx));
        cx.subscribe_in(
            &palette,
            window,
            |this, _, event: &PaletteEvent, window, cx| {
                if let PaletteEvent::Run(action) = event {
                    match action.clone() {
                        PaletteAction::FocusPane { workspace, pane } => {
                            this.select_workspace(workspace, cx);
                            this.focused = Some(pane);
                        }
                        PaletteAction::Project(id) => this.select_workspace(id, cx),
                        PaletteAction::NewPane => return this.open_new_pane(window, cx),
                        PaletteAction::NewTerminal => this.new_pane(None, cx),
                        PaletteAction::ToggleZoom => {
                            this.zoomed = if this.zoomed.is_some() {
                                None
                            } else {
                                this.focused.clone()
                            };
                        }
                        PaletteAction::Settings => return this.open_settings(window, cx),
                        PaletteAction::Accounts => return this.open_accounts(window, cx),
                        PaletteAction::Changes => this.toggle_changes(window, cx),
                        PaletteAction::Usage => return this.open_usage(window, cx),
                        PaletteAction::Mods => return this.open_mods(window, cx),
                        PaletteAction::Scheduled => return this.open_scheduled(window, cx),
                        PaletteAction::ClosePane => {
                            if let Some(id) = this.focused.clone() {
                                this.close_pane(id, cx);
                            }
                        }
                    }
                }
                this.close_modal(window, cx);
            },
        )
        .detach();
        self.modal = Some(Modal::Palette(palette));
        cx.notify();
    }

    /// Debug builds only: `WRAITHGRID_OPEN=new-pane|palette` opens an overlay at start, so
    /// screenshots of it can be taken without driving the keyboard.
    pub fn open_from_env(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cfg!(debug_assertions) {
            return;
        }
        // `WRAITHGRID_SEND=<text>` sends a message in the first chat pane once it is drawn.
        if let Ok(text) = std::env::var("WRAITHGRID_SEND") {
            cx.on_next_frame(window, move |this, window, cx| {
                if let Some(chat) = this.chats.values().next().cloned() {
                    chat.update(cx, |c, cx| c.send_text(&text, window, cx));
                }
            });
        }
        // `WRAITHGRID_TYPE=<text>` types into the first chat's box a moment after start.
        if let Ok(text) = std::env::var("WRAITHGRID_TYPE") {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if let Some(chat) = this.chats.values().next().cloned() {
                        chat.update(cx, |c, cx| c.type_text(&text, window, cx));
                    }
                });
            })
            .detach();
        }
        match std::env::var("WRAITHGRID_OPEN").as_deref() {
            Ok("new-pane") => self.open_new_pane(window, cx),
            Ok("palette") => self.open_palette(window, cx),
            Ok("settings") => self.open_settings(window, cx),
            Ok("quit") => self.request_close(window, cx),
            Ok("new-project") => self.open_new_project(window, cx),
            Ok("accounts") => self.open_accounts(window, cx),
            Ok("changes") => self.toggle_changes(window, cx),
            Ok("history") => {
                self.toggle_changes(window, cx);
                if let Some(panel) = self.changes.clone() {
                    panel.update(cx, |p, cx| p.show_history(cx));
                }
            }
            Ok("usage") => self.open_usage(window, cx),
            Ok("mods") => self.open_mods(window, cx),
            Ok("scheduled") => self.open_scheduled(window, cx),
            Ok("projectpage") => {
                let ws = self.config().active_workspace.clone();
                self.open_project_page(ws, window, cx)
            }
            Ok("continue") => {
                if let Some(id) = self.config().active().panes.first().map(|p| p.id.clone()) {
                    self.ask_continue_as(id, window, cx);
                }
            }
            Ok("project") => {
                let id = self.config().active_workspace.clone();
                self.open_project(id, window, cx)
            }
            _ => {}
        }
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        self.accounts_view = None;
        self.usage_view = None;
        self.mods_view = None;
        self.scheduled_view = None;
        self.project_view = None;
        let config = self.config().clone();
        let view = cx.new(|cx| SettingsView::new(&config, window, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::Changed(settings, claude_path) => {
                    let (settings, claude_path) = ((**settings).clone(), claude_path.clone());
                    let s0 = &this.config().settings;
                    let look_changed = (
                        s0.theme,
                        &s0.theme_dark,
                        &s0.theme_light,
                        s0.accent,
                        s0.terminal_palette,
                    ) != (
                        settings.theme,
                        &settings.theme_dark,
                        &settings.theme_light,
                        settings.accent,
                        settings.terminal_palette,
                    );
                    this.host.set_cli_access(settings.cli_access);
                    if settings.shared_mode != this.config().settings.shared_mode {
                        // Links every account to the shared brain, or takes the links out.
                        let mut next = this.config().clone();
                        next.settings.shared_mode = settings.shared_mode;
                        cx.background_spawn(async move {
                            if let Err(e) = crate::mods_glue::apply_shared_mode(&next) {
                                eprintln!("[wraithgrid] shared mode: {e}");
                            }
                        })
                        .detach();
                    }
                    this.update(cx, |c| {
                        c.settings = settings;
                        c.claude_path = claude_path;
                    });
                    if look_changed {
                        let resolved =
                            crate::theme::resolve(this.config(), crate::theme::system_dark(window));
                        crate::theme::apply(&resolved, Some(window), cx);
                    }
                }
                SettingsEvent::Close => {
                    this.settings_view = None;
                    this.close_modal(window, cx);
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.settings_view = Some(view);
        cx.notify();
    }

    fn open_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        let config = self.config().clone();
        let dialog = cx.new(|cx| NewProjectDialog::new(&config, window, cx));
        cx.subscribe_in(
            &dialog,
            window,
            |this, dialog, event: &NewProjectEvent, window, cx| match event {
                NewProjectEvent::Create {
                    name,
                    folder,
                    account,
                    start_chat,
                } => match this.create_project(name, folder, account.clone(), *start_chat, cx) {
                    Ok(()) => this.close_modal(window, cx),
                    Err(e) => dialog.update(cx, |d, cx| {
                        d.error = Some(e);
                        cx.notify();
                    }),
                },
                NewProjectEvent::Cancel => this.close_modal(window, cx),
            },
        )
        .detach();
        self.modal = Some(Modal::NewProject(dialog));
        cx.notify();
    }

    /// `createProject`: a unique name, then the project, switched to; a first chat on request.
    fn create_project(
        &mut self,
        name: &str,
        folder: &str,
        account: Option<String>,
        start_chat: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let name: String = name.trim().chars().take(64).collect();
        if name.is_empty() {
            return Err("Give the project a name.".into());
        }
        if self
            .config()
            .workspaces
            .iter()
            .any(|w| w.name.to_lowercase() == name.to_lowercase())
        {
            return Err(format!("A project named {name} exists."));
        }
        if self.config().workspaces.len() >= 32 {
            return Err("Wraithgrid holds up to 32 projects.".into());
        }
        let account = account.filter(|id| self.config().account(id).is_some());
        let folder: String = folder.trim().chars().take(4096).collect();
        let ws = wraithgrid_core::config::Workspace {
            id: new_id("ws"),
            name,
            folder: folder.clone(),
            account_id: account.clone(),
            ..wraithgrid_core::config::default_workspace()
        };
        let id = ws.id.clone();
        self.update(cx, |c| {
            c.workspaces.push(ws);
            c.active_workspace = id;
        });
        if let (true, false, Some(account)) = (start_chat, folder.is_empty(), account) {
            self.add_pane(None, Some(account), folder, vec![], false, true, cx);
        }
        Ok(())
    }

    fn open_accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        self.settings_view = None;
        self.usage_view = None;
        self.mods_view = None;
        self.scheduled_view = None;
        self.project_view = None;
        let config = self.config().clone();
        let view = cx.new(|cx| AccountsView::new(&config, window, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, view, event: &AccountsEvent, window, cx| {
                let home = home_dir();
                let mut next = this.config().clone();
                let result = match event {
                    AccountsEvent::Add { name, cli, color } => {
                        wraithgrid_core::accounts::add_account(&mut next, name, color, *cli, &home)
                            .map(|_| ())
                    }
                    AccountsEvent::Rename { id, name } => {
                        if next.account(id).is_some_and(|a| &a.name == name) {
                            return;
                        }
                        wraithgrid_core::accounts::rename_account(&mut next, id, name)
                    }
                    AccountsEvent::Remove { id } => {
                        // Panes first: a running agent must not keep writing into its account.
                        for pane in wraithgrid_core::accounts::remove_account(&mut next, id) {
                            this.close_pane(pane.clone(), cx);
                            for w in &mut next.workspaces {
                                w.panes.retain(|p| p.id != pane);
                                w.layout = remove_pane(w.layout.as_ref(), &pane);
                            }
                        }
                        Ok(())
                    }
                    AccountsEvent::MarkSignedIn { id } => {
                        next.accounts
                            .iter_mut()
                            .filter(|a| &a.id == id)
                            .for_each(|a| a.signed_in = true);
                        Ok(())
                    }
                    AccountsEvent::Login { id } => {
                        // Signing in happens in the CLI's own screen: a terminal running it.
                        this.accounts_view = None;
                        this.add_pane(None, Some(id.clone()), "~".into(), vec![], false, false, cx);
                        return this.close_modal(window, cx);
                    }
                    AccountsEvent::Close => {
                        this.accounts_view = None;
                        return this.close_modal(window, cx);
                    }
                };
                match result {
                    Ok(()) => {
                        this.update(cx, |c| *c = next);
                        // Shown again from the saved config, rows and all.
                        this.open_accounts(window, cx);
                    }
                    Err(e) => view.update(cx, |v, cx| {
                        v.error = Some(e);
                        cx.notify();
                    }),
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.accounts_view = Some(view);
        cx.notify();
    }

    fn open_project(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        let config = self.config().clone();
        if !config.workspaces.iter().any(|w| w.id == id) {
            return;
        }
        let dialog =
            cx.new(|cx| ProjectDialog::new(&config, &id, window, cx).expect("project exists"));
        cx.subscribe_in(
            &dialog,
            window,
            |this, _, event: &ProjectEvent, window, cx| {
                match event {
                    ProjectEvent::Save {
                        id,
                        name,
                        folder,
                        account,
                    } => {
                        let name: String = name.trim().chars().take(64).collect();
                        let folder: String = folder.trim().chars().take(4096).collect();
                        let account = account.clone();
                        this.update(cx, |c| {
                            if let Some(w) = c.workspaces.iter_mut().find(|w| &w.id == id) {
                                if !name.is_empty() {
                                    w.name = name;
                                }
                                w.folder = folder;
                                w.account_id = account;
                            }
                        });
                    }
                    ProjectEvent::Delete { id } => this.delete_project(id.clone(), cx),
                    ProjectEvent::Cancel => {}
                }
                this.close_modal(window, cx);
            },
        )
        .detach();
        self.modal = Some(Modal::Project(dialog));
        cx.notify();
    }

    /// `deleteWorkspace`: its panes stop; never the last project.
    fn delete_project(&mut self, id: String, cx: &mut Context<Self>) {
        if self.config().workspaces.len() <= 1 {
            return;
        }
        let panes: Vec<String> = self
            .config()
            .workspaces
            .iter()
            .filter(|w| w.id == id)
            .flat_map(|w| w.panes.iter().map(|p| p.id.clone()))
            .collect();
        for pane in panes {
            self.close_pane(pane, cx);
        }
        self.update(cx, |c| {
            c.workspaces.retain(|w| w.id != id);
            if c.active_workspace == id {
                c.active_workspace = c.workspaces[0].id.clone();
            }
        });
    }

    /// Live limits (DECISIONS 2026-11-02): reads usage.json, then asks Anthropic for each
    /// signed-in claude account's 5-hour and weekly use at launch and every 2 minutes while
    /// the window is open.
    pub fn start_usage(&mut self, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        let usage = self.usage.clone();
        self._usage_task = Some(cx.spawn(async move |this, cx| {
            let mut poller = wraithgrid_usage::poll::UsagePoller::new(
                wraithgrid_usage::poll::UsagePollDeps::live(),
            );
            {
                let usage = usage.clone();
                let limits = cx
                    .background_spawn(async move {
                        let mut store = usage.lock().unwrap();
                        store.load();
                        store.get().limits
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.limits = limits;
                    if let Some(view) = this.usage_view.clone() {
                        let data = this.usage.lock().unwrap().get();
                        view.update(cx, |v, cx| v.set_data(data, cx));
                    }
                    cx.notify();
                });
            }
            loop {
                let Ok(accounts) = this.update(cx, |this, _| {
                    let home = home_dir();
                    this.config()
                        .accounts
                        .iter()
                        .map(|a| wraithgrid_usage::UsageAccount {
                            id: a.id.clone(),
                            name: a.name.clone(),
                            cli: a.cli.as_str().into(),
                            signed_in: a.signed_in,
                            config_dir: wraithgrid_core::paths::resolve_user_path(
                                &a.config_dir,
                                &home,
                            ),
                        })
                        .collect::<Vec<_>>()
                }) else {
                    break;
                };
                let usage = usage.clone();
                let (limits, back) = cx
                    .background_spawn(async move {
                        let found = poller.poll_once(&accounts);
                        let mut store = usage.lock().unwrap();
                        for (id, l) in found {
                            store.set_limits(&id, l);
                        }
                        store.flush();
                        (store.get().limits, poller)
                    })
                    .await;
                poller = back;
                let _ = this.update(cx, |this, cx| {
                    this.limits = limits;
                    if let Some(view) = this.usage_view.clone() {
                        let data = this.usage.lock().unwrap().get();
                        view.update(cx, |v, cx| v.set_data(data, cx));
                    }
                    cx.notify();
                });
                cx.background_executor()
                    .timer(Duration::from_millis(
                        wraithgrid_usage::poll::POLL_MS as u64,
                    ))
                    .await;
            }
        }));
    }

    /// An account's live use as `5h 34% · wk 12%`, and the worse of the two as a fraction.
    fn limits_label(&self, account_id: &str) -> Option<(String, f64)> {
        let l = self.limits.get(account_id)?;
        let now = wraithgrid_usage::now_ms();
        let five = wraithgrid_usage::model::live_window(l.five_hour, now);
        let week = wraithgrid_usage::model::live_window(l.seven_day, now);
        let pct = |w: Option<wraithgrid_usage::model::UsageWindow>| {
            w.map(|w| (w.utilization * 100.0).round() as i64)
        };
        let parts: Vec<String> = [("5h", pct(five)), ("wk", pct(week))]
            .iter()
            .filter_map(|(k, p)| p.map(|p| format!("{k} {p}%")))
            .collect();
        if parts.is_empty() {
            return None;
        }
        let worst = five
            .map_or(0.0, |w| w.utilization)
            .max(week.map_or(0.0, |w| w.utilization));
        Some((parts.join(" · "), worst))
    }

    fn open_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        self.settings_view = None;
        self.accounts_view = None;
        self.mods_view = None;
        self.scheduled_view = None;
        self.project_view = None;
        let data = self.usage.lock().unwrap().get();
        let config = self.config().clone();
        let view = cx.new(|cx| UsageView::new(data, config, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, event: &UsageViewEvent, window, cx| match event {
                UsageViewEvent::Close => {
                    this.usage_view = None;
                    this.close_modal(window, cx);
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.usage_view = Some(view);
        cx.notify();
    }

    fn open_mods(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        self.settings_view = None;
        self.accounts_view = None;
        self.usage_view = None;
        self.scheduled_view = None;
        self.project_view = None;
        let config = self.config().clone();
        let view = cx.new(|cx| ModsView::new(config, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ModsViewEvent, window, cx| match event {
                ModsViewEvent::Close => {
                    this.mods_view = None;
                    this.close_modal(window, cx);
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.mods_view = Some(view);
        cx.notify();
    }

    fn open_scheduled(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        self.settings_view = None;
        self.accounts_view = None;
        self.usage_view = None;
        self.mods_view = None;
        self.project_view = None;
        let config = self.config().clone();
        let view = cx.new(|cx| ScheduledView::new(config, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ScheduledEvent, window, cx| {
                let tz = chrono::Local;
                let now = wraithgrid_usage::now_ms();
                match event {
                    ScheduledEvent::Close => {
                        this.scheduled_view = None;
                        this.close_modal(window, cx);
                    }
                    ScheduledEvent::Save(job, folder) => {
                        let limits = this.job_limits(&job.account_id);
                        this.update(cx, |c| {
                            if let Some(folder) = folder {
                                if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == job.ws_id)
                                {
                                    w.folder = folder.clone();
                                }
                            }
                            wraithgrid_schedule::save_job(
                                &tz,
                                &mut c.schedules,
                                (**job).clone(),
                                now,
                                limits.as_ref(),
                            );
                        });
                    }
                    ScheduledEvent::OpenChat(ws, pane) => {
                        this.scheduled_view = None;
                        this.reveal_chat(ws, pane, cx);
                        this.close_modal(window, cx);
                    }
                    ScheduledEvent::Toggle(id, on) => {
                        if let Some(job) = this
                            .config()
                            .schedules
                            .iter()
                            .find(|j| &j.id == id)
                            .cloned()
                        {
                            let limits = this.job_limits(&job.account_id);
                            let job = wraithgrid_schedule::set_enabled(
                                &tz,
                                job,
                                *on,
                                now,
                                limits.as_ref(),
                            );
                            this.replace_job(job, cx);
                        }
                    }
                    ScheduledEvent::Delete(id) => {
                        let id = id.clone();
                        this.update(cx, |c| c.schedules.retain(|j| j.id != id));
                    }
                    ScheduledEvent::RunNow(id) => this.run_job_now(&id.clone(), window, cx),
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.scheduled_view = Some(view);
        cx.notify();
    }

    /// A pane's status as Electron's `PaneStatus` names it.
    fn pane_status(&self, pane_id: &str) -> Option<&'static str> {
        use wraithgrid_chat::ChatStatus;
        match self.chat_states.get(pane_id) {
            Some(ChatStatus::Running) => Some("running"),
            Some(ChatStatus::Background) => Some("background"),
            Some(ChatStatus::Approval) => Some("approval"),
            Some(ChatStatus::Idle) => Some("idle"),
            None => self.states.get(pane_id).map(|s| match s.as_str() {
                "running" => "running",
                "blocked" => "approval",
                "exited" => "exited",
                _ => "idle",
            }),
        }
    }

    fn pane_statuses(&self) -> HashMap<String, &'static str> {
        self.config()
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .filter_map(|p| self.pane_status(&p.id).map(|s| (p.id.clone(), s)))
            .collect()
    }

    fn refresh_project_view(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.project_view.clone() {
            let (config, statuses) = (self.store.get().clone(), self.pane_statuses());
            view.update(cx, |v, cx| v.set_data(config, statuses, cx));
        }
    }

    /// `openProject`: switches to the project and shows its page (open chats and History).
    fn open_project_page(&mut self, ws_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        self.settings_view = None;
        self.accounts_view = None;
        self.usage_view = None;
        self.mods_view = None;
        self.scheduled_view = None;
        self.select_workspace(ws_id.clone(), cx);
        let (config, statuses) = (self.config().clone(), self.pane_statuses());
        let view = cx.new(|cx| ProjectView::new(config, ws_id, statuses, window, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ProjectPageEvent, window, cx| match event {
                ProjectPageEvent::Close => {
                    this.project_view = None;
                    this.close_modal(window, cx);
                }
                ProjectPageEvent::Reveal(pane) => {
                    this.project_view = None;
                    let ws = this.config().active_workspace.clone();
                    this.reveal_chat(&ws, pane, cx);
                    this.close_modal(window, cx);
                }
                ProjectPageEvent::Reopen(ws, pane) => {
                    this.project_view = None;
                    this.reopen_chat(ws, pane, true, cx);
                    this.close_modal(window, cx);
                }
                ProjectPageEvent::Delete(ws, pane) => this.delete_chat(ws, pane, cx),
                ProjectPageEvent::NewChat => {
                    this.project_view = None;
                    this.open_new_pane(window, cx);
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.project_view = Some(view);
        cx.notify();
    }

    /// `deleteChat`: an open chat is stopped and not archived; a History one is removed. Its
    /// saved view goes either way; the CLI's own session files stay.
    fn delete_chat(&mut self, ws_id: &str, pane_id: &str, cx: &mut Context<Self>) {
        let open = self
            .config()
            .workspaces
            .iter()
            .any(|w| w.panes.iter().any(|p| p.id == pane_id));
        if open {
            return self.drop_pane(pane_id, false, cx);
        }
        let (ws, id) = (ws_id.to_string(), pane_id.to_string());
        self.update(cx, |c| {
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == ws) {
                w.history.retain(|h| h.pane.id != id);
            }
        });
        let views = wraithgrid_core::paths::current_user_data_dir().join("chats");
        let _ = wraithgrid_chat::views::delete_view(&views, pane_id);
    }

    /// Shows or hides the Changes panel for the focused pane's folder (else the project's).
    fn toggle_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.changes.take().is_some() || self.blocked.is_some() {
            return cx.notify();
        }
        let ws = self.config().active();
        let cwd = self
            .focused
            .as_ref()
            .and_then(|f| ws.panes.iter().find(|p| &p.id == f))
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| {
                if ws.folder.is_empty() {
                    self.config().settings.default_cwd.clone()
                } else {
                    ws.folder.clone()
                }
            });
        let dir = wraithgrid_core::paths::resolve_user_path(&cwd, &home_dir());
        self.changes = Some(cx.new(|cx| ChangesPanel::new(dir, window, cx)));
        cx.notify();
    }

    /// Branch and changed count of every shown pane's folder, every 5 s (`GIT_POLL_MS`).
    fn start_git_poll(&mut self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| loop {
            let Ok(folders) = this.update(cx, |this, _| {
                let mut f: Vec<String> = this
                    .config()
                    .active()
                    .panes
                    .iter()
                    .map(|p| p.cwd.clone())
                    .collect();
                f.sort();
                f.dedup();
                f
            }) else {
                break;
            };
            let heads = cx
                .background_spawn(async move {
                    let git = wraithgrid_git::GitCli::default();
                    let home = home_dir();
                    folders
                        .into_iter()
                        .map(|f| {
                            let status = wraithgrid_git::git_status(
                                &git,
                                &wraithgrid_core::paths::resolve_user_path(&f, &home),
                            );
                            (f, status)
                        })
                        .collect::<HashMap<_, _>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.git_heads != heads {
                    this.git_heads = heads;
                    cx.notify();
                }
            });
            cx.background_executor().timer(Duration::from_secs(5)).await;
        })
    }

    /// Asks GitHub for a newer release once at startup (Settings → check on launch).
    pub fn check_updates_on_launch(&mut self, cx: &mut Context<Self>) {
        if self.blocked.is_none() && self.config().settings.check_updates_on_launch {
            self.check_updates(cx);
        }
    }

    fn check_updates(&mut self, cx: &mut Context<Self>) {
        self.update = UpdateState::Checking;
        self._update_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let ua = format!("Wraithgrid/{}", crate::runtime::APP_VERSION);
                    update::check(crate::runtime::APP_VERSION, &|url| update::fetch(url, &ua))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.update = match result {
                    update::CheckResult::Available { latest, .. } => UpdateState::Available(latest),
                    update::CheckResult::Current { .. } => UpdateState::Idle,
                    update::CheckResult::Error { error, .. } => UpdateState::Error(error),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The title bar's update button: download and verify, then restart into it. Builds that
    /// can't update themselves (macOS, from source) open the release page instead.
    fn run_update(&mut self, cx: &mut Context<Self>) {
        let exe = std::env::current_exe().unwrap_or_default();
        let kind = update::package_kind(&exe, |k| std::env::var(k).ok());
        match std::mem::replace(&mut self.update, UpdateState::Idle) {
            UpdateState::Available(release) if kind == update::PackageKind::Manual => {
                cx.open_url(&release.url);
                self.update = UpdateState::Available(release);
            }
            UpdateState::Available(release) => {
                self.update = UpdateState::Downloading {
                    version: release.version.clone(),
                    percent: 0,
                };
                let (tx, mut rx) = futures::channel::mpsc::unbounded::<u8>();
                let dir = std::env::temp_dir().join("wraithgrid-update");
                let job = cx.background_spawn({
                    let release = release.clone();
                    async move {
                        let ua = format!("Wraithgrid/{}", crate::runtime::APP_VERSION);
                        update::fetch_update(
                            &release,
                            kind,
                            &dir,
                            &ua,
                            &mut |p| {
                                let _ = tx.unbounded_send(p);
                            },
                            &|| false,
                        )
                    }
                });
                self._update_task = Some(cx.spawn(async move |this, cx| {
                    let progress = async {
                        while let Some(p) = rx.next().await {
                            let _ = this.update(cx, |this, cx| {
                                if let UpdateState::Downloading { percent, .. } = &mut this.update {
                                    *percent = p;
                                    cx.notify();
                                }
                            });
                        }
                    };
                    let (result, ()) = futures::join!(job, progress);
                    let _ = this.update(cx, |this, cx| {
                        this.update = match result {
                            Ok(file) => UpdateState::Ready {
                                release,
                                file,
                                kind,
                            },
                            Err(e) => UpdateState::Error(e),
                        };
                        cx.notify();
                    });
                }));
            }
            UpdateState::Ready {
                release,
                file,
                kind,
            } => {
                self.update = UpdateState::Installing(release.version.clone());
                self.flush();
                let host = self.host.clone();
                self._update_task = Some(cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_spawn(async move {
                            let appimage =
                                std::env::var("APPIMAGE").ok().map(std::path::PathBuf::from);
                            let installed = match (kind, &appimage) {
                                (update::PackageKind::AppImage, Some(target)) => {
                                    update::install_appimage(&file, target)
                                        .map_err(|e| e.to_string())
                                }
                                _ => match update::install_command(kind, &file) {
                                    Some((cmd, args)) => match std::process::Command::new(cmd)
                                        .args(args)
                                        .status()
                                    {
                                        Ok(s)
                                            if s.success() || kind == update::PackageKind::Nsis =>
                                        {
                                            Ok(())
                                        }
                                        _ => Err(
                                            "The update was cancelled or could not be installed."
                                                .to_string(),
                                        ),
                                    },
                                    None => Err(update::MANUAL.to_string()),
                                },
                            };
                            if installed.is_ok() && kind != update::PackageKind::Nsis {
                                // Agents stay in the host; the new version reattaches to them.
                                host.detach();
                                let relaunch = appimage
                                    .unwrap_or_else(|| std::env::current_exe().unwrap_or_default());
                                let _ = std::process::Command::new(relaunch).spawn();
                            }
                            installed
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| match result {
                        Ok(()) => cx.quit(),
                        Err(e) => {
                            this.update = UpdateState::Error(e);
                            cx.notify();
                        }
                    });
                }));
            }
            other => self.update = other,
        }
        cx.notify();
    }

    /// The title bar's update button: its label, and whether it does something now.
    fn update_button(&self) -> Option<(String, bool)> {
        match &self.update {
            UpdateState::Available(r) => Some((format!("Update to v{}", r.version), true)),
            UpdateState::Downloading { version, percent } => {
                Some((format!("Downloading v{version}… {percent}%"), false))
            }
            UpdateState::Ready { release, .. } => {
                Some((format!("Restart to update to v{}", release.version), true))
            }
            UpdateState::Installing(v) => Some((format!("Installing v{v}…"), false)),
            UpdateState::Error(e) => Some((format!("Update failed: {e}"), false)),
            UpdateState::Idle | UpdateState::Checking => None,
        }
    }

    /// A chat's limits and turn costs into usage.json (saved shortly after).
    fn record_usage(
        &mut self,
        pane_id: &str,
        e: &wraithgrid_usage::store::UsageEvent,
        cx: &mut Context<Self>,
    ) {
        let mut store = self.usage.lock().unwrap();
        store.on_chat_event(pane_id, e);
        self.limits = store.get().limits;
        drop(store);
        let usage = self.usage.clone();
        self._usage_save = Some(cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1600))
                .await;
            usage.lock().unwrap().tick();
        }));
        cx.notify();
    }

    fn usage_accounts(&self) -> Vec<wraithgrid_usage::UsageAccount> {
        let home = home_dir();
        self.config()
            .accounts
            .iter()
            .map(|a| wraithgrid_usage::UsageAccount {
                id: a.id.clone(),
                name: a.name.clone(),
                cli: a.cli.as_str().into(),
                signed_in: a.signed_in,
                config_dir: wraithgrid_core::paths::resolve_user_path(&a.config_dir, &home),
            })
            .collect()
    }

    /// After a turn: at the account's limit the chat carries on as the next account with room,
    /// asking first, on its own, or not at all (#61, #161).
    fn maybe_switch(
        &mut self,
        pane_id: &str,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let s = &self.config().settings;
        if s.auto_switch_mode == wraithgrid_core::types::AutoSwitchMode::Off {
            return;
        }
        let (mode, at) = (s.auto_switch_mode, s.auto_switch_at);
        let Some(from_id) = self
            .config()
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .find(|p| p.id == pane_id)
            .and_then(|p| p.account_id.clone())
        else {
            return;
        };
        let accounts = self.usage_accounts();
        let Some(from) = accounts.iter().find(|a| a.id == from_id) else {
            return;
        };
        let decision = wraithgrid_usage::auto_switch::decide_switch(
            &accounts,
            from,
            &self.limits,
            wraithgrid_usage::now_ms(),
            at,
            error.as_deref(),
        );
        let chat = self.chats.get(pane_id).cloned();
        match decision {
            wraithgrid_usage::auto_switch::SwitchDecision::Stay => {}
            wraithgrid_usage::auto_switch::SwitchDecision::NoneLeft { reason } => {
                if let Some(chat) = chat {
                    chat.update(cx, |c, cx| {
                        c.add_notice(wraithgrid_chat::ItemLevel::Error, &reason, cx)
                    });
                }
            }
            wraithgrid_usage::auto_switch::SwitchDecision::Switch { to, reason, resend } => {
                let (to_id, to_name) = (to.id.clone(), to.name.clone());
                let resend_text = if resend {
                    chat.as_ref().and_then(|c| c.read(cx).last_user_text())
                } else {
                    None
                };
                if mode == wraithgrid_core::types::AutoSwitchMode::Auto {
                    if let Some(chat) = &chat {
                        chat.update(cx, |c, cx| {
                            c.add_notice(
                                wraithgrid_chat::ItemLevel::Info,
                                &format!("{reason}: switching to {to_name}"),
                                cx,
                            )
                        });
                    }
                    self.continue_as(pane_id, &to_id, resend_text, window, cx);
                    return;
                }
                let dialog = cx.new(|_| {
                    ChoiceDialog::new(
                        reason,
                        "The chat can carry on as your next account with room, with the whole conversation.",
                        vec![(to_id.clone(), format!("Switch to {to_name}"), if resend { "and send the message again".into() } else { String::new() })],
                        "Stay",
                    )
                });
                let pane = pane_id.to_string();
                cx.subscribe_in(
                    &dialog,
                    window,
                    move |this, _, e: &ChoiceEvent, window, cx| {
                        if let ChoiceEvent::Pick(to) = e {
                            this.continue_as(&pane, to, resend_text.clone(), window, cx);
                        }
                        this.close_modal(window, cx);
                    },
                )
                .detach();
                self.modal = Some(Modal::Choice(dialog));
                cx.notify();
            }
        }
    }

    /// "Continue as…" for a chat pane: the other signed-in accounts of its CLI.
    fn ask_continue_as(&mut self, pane_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let c = self.config();
        let Some(pane) = c
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .find(|p| p.id == pane_id)
        else {
            return;
        };
        let Some(from) = pane.account_id.as_deref().and_then(|id| c.account(id)) else {
            return;
        };
        let choices: Vec<(String, String, String)> = c
            .accounts
            .iter()
            .filter(|a| a.id != from.id && a.cli == from.cli)
            .map(|a| {
                let detail = match (a.signed_in, self.limits_label(&a.id)) {
                    (false, _) => "not signed in".to_string(),
                    (true, Some((l, _))) => l,
                    (true, None) => String::new(),
                };
                (a.id.clone(), a.name.clone(), detail)
            })
            .collect();
        let title = format!(
            "Continue as another {} account",
            from.cli.spec().provider_label
        );
        let message = if choices.is_empty() {
            "No other account of this CLI yet.".to_string()
        } else {
            "The whole conversation goes with it.".to_string()
        };
        let dialog = cx.new(|_| ChoiceDialog::new(title, message, choices, "Cancel"));
        cx.subscribe_in(
            &dialog,
            window,
            move |this, _, e: &ChoiceEvent, window, cx| {
                if let ChoiceEvent::Pick(to) = e {
                    this.continue_as(&pane_id, to, None, window, cx);
                }
                this.close_modal(window, cx);
            },
        )
        .detach();
        self.modal = Some(Modal::Choice(dialog));
        cx.notify();
    }

    /// `chatSwitchAccount` for the same CLI: the conversation file is copied into the other
    /// account's folder and the pane carries on as it; `resend` goes again there.
    fn continue_as(
        &mut self,
        pane_id: &str,
        to_id: &str,
        resend: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = (|| -> Result<String, String> {
            let c = self.config();
            let pane = c
                .workspaces
                .iter()
                .flat_map(|w| &w.panes)
                .find(|p| p.id == pane_id)
                .ok_or("That chat is gone.")?;
            let from = pane
                .account_id
                .as_deref()
                .and_then(|id| c.account(id))
                .ok_or("That account is gone.")?;
            let to = c.account(to_id).ok_or("That account is gone.")?;
            if !to.signed_in {
                return Err(format!("{} is not signed in yet.", to.name));
            }
            if from.cli != to.cli || !from.cli.can_continue_as() {
                return Err("This chat can only switch to another account of the same CLI.".into());
            }
            let chat = self.chats.get(pane_id).cloned();
            if chat.as_ref().is_some_and(|c| c.read(cx).is_busy()) && resend.is_none() {
                return Err("Wait for the answer to finish, or stop it first.".into());
            }
            let session = chat
                .as_ref()
                .and_then(|c| c.read(cx).session_id())
                .or_else(|| pane.chat_session_id.clone());
            if let Some(chat) = &chat {
                chat.update(cx, |c, _| c.stop_session());
            }
            if let Some(sid) = session {
                let home = home_dir();
                let (fd, td) = (
                    wraithgrid_core::paths::resolve_user_path(&from.config_dir, &home),
                    wraithgrid_core::paths::resolve_user_path(&to.config_dir, &home),
                );
                let root = wraithgrid_core::paths::accounts_root(&home);
                let copied = if from.cli == AgentCli::Codex {
                    wraithgrid_chat::transcript::copy_codex_rollout(&fd, &td, &sid, &root)
                } else {
                    wraithgrid_chat::transcript::copy_transcript(&fd, &td, &sid, &root)
                };
                copied.map_err(|e| e.to_string())?;
            }
            Ok(to.name.clone())
        })();
        let to_name = match result {
            Ok(n) => n,
            Err(e) => {
                if let Some(chat) = self.chats.get(pane_id).cloned() {
                    chat.update(cx, |c, cx| {
                        c.add_notice(
                            wraithgrid_chat::ItemLevel::Error,
                            &format!("Could not switch: {e}"),
                            cx,
                        )
                    });
                }
                return;
            }
        };
        let at = chrono_like_time();
        if let Some(chat) = self.chats.remove(pane_id) {
            chat.update(cx, |c, cx| {
                c.add_notice(
                    wraithgrid_chat::ItemLevel::Divider,
                    &format!("Continued as {to_name} · {at}"),
                    cx,
                )
            });
        }
        let (id, to) = (pane_id.to_string(), to_id.to_string());
        self.update(cx, |c| {
            for p in c
                .workspaces
                .iter_mut()
                .flat_map(|w| w.panes.iter_mut())
                .filter(|p| p.id == id)
            {
                p.account_id = Some(to.clone());
            }
        });
        // The chat view is made again for the new account, then the refused message goes again.
        if let Some(text) = resend {
            let pane = id.clone();
            cx.on_next_frame(window, move |this, window, cx| {
                if let Some(chat) = this.chats.get(&pane).cloned() {
                    chat.update(cx, |c, cx| c.resend(&text, window, cx));
                }
            });
        }
        cx.notify();
    }

    /// Starts an exited pane again: its old process is cleared from the host first.
    fn restart_pane(&mut self, pane_id: String, cx: &mut Context<Self>) {
        self.host.kill(&pane_id);
        self.router.unregister(&pane_id);
        self.terminals.remove(&pane_id);
        self.focused = Some(pane_id);
        cx.notify();
    }

    /// The close button: asks what to do with running agents, unless there are none or the
    /// choice is saved (`chooseOnClose`). Kept agents show again, screen and all, next launch.
    pub fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return cx.quit();
        }
        let host = self.host.clone();
        cx.spawn_in(window, async move |this, cx| {
            let live = cx.background_spawn(async move { host.live_count() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let c = this.config();
                let mode = if live == 0 {
                    // No agents, but scheduled jobs: keep the host so they still run (#77).
                    if c.schedules.iter().any(|j| j.enabled) {
                        CloseMode::Keep
                    } else {
                        CloseMode::Stop
                    }
                } else {
                    c.settings.on_window_close
                };
                match mode {
                    CloseMode::Keep => this.quit(true, cx),
                    CloseMode::Stop => this.quit(false, cx),
                    CloseMode::Ask => {
                        let dialog = cx.new(|_| QuitDialog::new(live));
                        cx.subscribe_in(
                            &dialog,
                            window,
                            |this, _, event: &QuitEvent, window, cx| match *event {
                                QuitEvent::Choose { keep, remember } => {
                                    if remember {
                                        this.update(cx, |c| {
                                            c.settings.on_window_close = if keep {
                                                CloseMode::Keep
                                            } else {
                                                CloseMode::Stop
                                            }
                                        });
                                    }
                                    this.quit(keep, cx);
                                }
                                QuitEvent::Cancel => this.close_modal(window, cx),
                            },
                        )
                        .detach();
                        this.modal = Some(Modal::Quit(dialog));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Saves, then leaves the agents to the host (`keep`) or stops them all, and quits.
    fn quit(&mut self, keep: bool, cx: &mut Context<Self>) {
        self.flush();
        self.flush_chats(cx);
        let host = self.host.clone();
        cx.spawn(async move |_, cx| {
            cx.background_spawn(async move {
                if keep {
                    host.detach();
                } else {
                    host.stop_all();
                }
            })
            .await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    }

    /// Closes the overlay and gives the keyboard back to the focused pane.
    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        if let Some(pane_id) = self.focused.clone() {
            // A new pane has no view until it is drawn: focus it on the next frame.
            cx.on_next_frame(window, move |this, window, cx| {
                if let Some(view) = this.terminals.get(&pane_id) {
                    window.focus(&view.focus_handle(cx), cx);
                }
            });
        }
        cx.notify();
    }

    /// Adds a pane to the active project next to the focused one (`addPaneToActive`).
    /// Adds a pane to `ws` (None: the active project). `chat` asks for a chat view, which an
    /// agent pane gets when its CLI has one and its account is signed in (signing in happens in
    /// the CLI's own screen, so a sign-in pane is a terminal).
    #[allow(clippy::too_many_arguments)]
    fn add_pane(
        &mut self,
        ws: Option<String>,
        account: Option<String>,
        cwd: String,
        args: Vec<String>,
        shell: bool,
        chat: bool,
        cx: &mut Context<Self>,
    ) -> String {
        let s = &self.config().settings;
        let acct = account.as_deref().and_then(|id| self.config().account(id));
        let view = if !shell && chat && acct.is_some_and(|a| a.cli.spec().chat && a.signed_in) {
            PaneView::Chat
        } else {
            PaneView::Terminal
        };
        let pane = Pane {
            id: new_id("p"),
            account_id: if shell { None } else { account },
            cwd: cwd.clone(),
            args: if shell { vec![] } else { args },
            shell,
            title: auto_title(shell, &cwd),
            resume: true,
            has_conversation: false,
            view,
            chat_session_id: None,
            model: s.chat_model.clone(),
            effort: s.chat_effort,
            chat_mode: s.chat_mode,
            team_agent_id: None,
            extra: Default::default(),
        };
        let pane_id = pane.id.clone();
        let into = ws.unwrap_or_else(|| self.config().active_workspace.clone());
        let here = into == self.config().active_workspace;
        let target = if here { self.focused.clone() } else { None };
        let dir = target
            .as_ref()
            .and_then(|t| self.pane_bounds.borrow().get(t).copied())
            .map_or(SplitDir::Row, |r| {
                if r.size.height > r.size.width {
                    SplitDir::Col
                } else {
                    SplitDir::Row
                }
            });
        self.update(cx, |c| {
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == into) {
                let ids: Vec<String> = w.panes.iter().map(|p| p.id.clone()).collect();
                let shown = shown_layout(w.layout.as_ref(), &ids);
                w.panes.push(pane);
                w.layout = Some(place_new_pane(
                    shown.as_ref(),
                    &pane_id,
                    None,
                    target.as_deref(),
                    dir,
                ));
            }
            c.recent_folders.retain(|f| f != &cwd);
            c.recent_folders.insert(0, cwd);
            c.recent_folders.truncate(8);
        });
        if here {
            self.focused = Some(pane_id.clone());
            self.zoomed = None;
        }
        pane_id
    }

    /// Answers the commands `wraithgrid` relays through the host (H6).
    pub fn listen_for_commands(&mut self, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        self.host.set_cli_access(self.config().settings.cli_access);
        let mut rx = self.router.gui_commands();
        self.gui_task = Some(cx.spawn(async move |this, cx| {
            while let Some((id, action)) = rx.next().await {
                let reply = this
                    .update(cx, |this, cx| this.cli_action(&action, cx))
                    .unwrap_or_else(|_| serde_json::json!({ "ok": false, "error": "The Wraithgrid window is closed. Open it to add panes." }));
                let host = this.update(cx, |this, _| this.host.clone());
                if let Ok(host) = host {
                    host.gui_reply(id, reply);
                }
            }
        }));
    }

    /// `wraithgrid new|split|send` (`runCliAction`): picks the account, project and folder,
    /// then adds the pane; `sent` marks a conversation, as Enter typed in the pane does.
    fn cli_action(
        &mut self,
        action: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        use serde_json::json;
        let fail = |e: String| json!({ "ok": false, "error": e });
        let text = |k: &str| action.get(k).and_then(|v| v.as_str()).map(String::from);
        let kind = text("kind").unwrap_or_default();
        if kind == "sent" {
            let pane_id = text("paneId").unwrap_or_default();
            if !self
                .config()
                .workspaces
                .iter()
                .any(|w| w.panes.iter().any(|p| p.id == pane_id))
            {
                return fail("No such pane.".into());
            }
            self.mark_conversation(&pane_id, cx);
            return json!({ "ok": true, "paneId": pane_id });
        }
        if kind != "new" && kind != "split" {
            return fail("Invalid command".into());
        }
        let home = home_dir();
        if let Some(cwd) = text("cwd") {
            match std::fs::metadata(wraithgrid_core::paths::resolve_user_path(&cwd, &home)) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return fail(format!("Not a folder: {cwd}")),
                Err(_) => return fail(format!("Folder not found: {cwd}")),
            }
        }
        let c = self.config().clone();
        let by_name = |list: Vec<(String, String)>, q: &str| {
            list.iter()
                .find(|(id, _)| id == q)
                .or_else(|| {
                    list.iter()
                        .find(|(_, n)| n.to_lowercase() == q.to_lowercase())
                })
                .map(|(id, _)| id.clone())
        };
        let mut base: Option<Pane> = None;
        if kind == "split" {
            let pane_id = text("paneId").unwrap_or_default();
            let Some((ws, pane)) = c.workspaces.iter().find_map(|w| {
                w.panes
                    .iter()
                    .find(|p| p.id == pane_id)
                    .map(|p| (w.id.clone(), p.clone()))
            }) else {
                return fail(format!("No pane {pane_id}."));
            };
            self.select_workspace(ws, cx);
            self.focused = Some(pane.id.clone());
            base = Some(pane);
        } else if let Some(w) = text("workspace") {
            match by_name(
                c.workspaces
                    .iter()
                    .map(|x| (x.id.clone(), x.name.clone()))
                    .collect(),
                &w,
            ) {
                Some(id) => self.select_workspace(id, cx),
                None => return fail(format!("No workspace \"{w}\".")),
            }
        }
        let shell = action
            .get("shell")
            .and_then(|v| v.as_bool())
            .or(base.as_ref().map(|b| b.shell))
            .unwrap_or(false);
        let mut account_id = None;
        if !shell {
            let focused = self
                .focused
                .as_ref()
                .and_then(|f| {
                    c.workspaces
                        .iter()
                        .flat_map(|w| &w.panes)
                        .find(|p| &p.id == f)
                })
                .cloned();
            let account = match text("account") {
                Some(q) => by_name(
                    c.accounts
                        .iter()
                        .map(|a| (a.id.clone(), a.name.clone()))
                        .collect(),
                    &q,
                ),
                None => base
                    .as_ref()
                    .or(focused.as_ref())
                    .and_then(|p| p.account_id.clone())
                    .filter(|id| c.account(id).is_some())
                    .or_else(|| c.accounts.first().map(|a| a.id.clone())),
            };
            match account {
                Some(id) => account_id = Some(id),
                None => {
                    return fail(text("account").map_or("Add an account first.".into(), |q| {
                        format!("No account \"{q}\".")
                    }))
                }
            }
        }
        let cwd = text("cwd")
            .or_else(|| base.as_ref().map(|b| b.cwd.clone()))
            .unwrap_or_else(|| "~".into());
        let args: Vec<String> = match action.get("args").and_then(|a| a.as_array()) {
            Some(a) => a
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect(),
            None => base.as_ref().map(|b| b.args.clone()).unwrap_or_default(),
        };
        // The command line drives panes through the pty host, where chat panes do not run.
        let pane_id = self.add_pane(None, account_id, cwd, args, shell, false, cx);
        json!({ "ok": true, "paneId": pane_id })
    }

    /// Closes a pane: its process stops and it leaves the project.
    /// `closePane`: a conversation is kept in the project's History; an unused pane or a
    /// shell is not.
    fn close_pane(&mut self, pane_id: String, cx: &mut Context<Self>) {
        let archive = self
            .config()
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .find(|p| p.id == pane_id)
            .is_some_and(|p| !p.shell && p.has_conversation);
        self.drop_pane(&pane_id, archive, cx);
    }

    /// `dropPane`: ends the pane's process (terminal or chat CLI) and removes it; `archive`
    /// keeps it, with its saved chat view, at the top of the project's History.
    fn drop_pane(&mut self, pane_id: &str, archive: bool, cx: &mut Context<Self>) {
        self.host.kill(pane_id);
        self.router.unregister(pane_id);
        self.terminals.remove(pane_id);
        if let Some(chat) = self.chats.remove(pane_id) {
            chat.update(cx, |c, _| {
                c.save();
                c.stop_session();
            });
        }
        let views = wraithgrid_core::paths::current_user_data_dir().join("chats");
        let mut forget: Vec<String> = vec![];
        if !archive {
            forget.push(pane_id.to_string());
        }
        let now = wraithgrid_usage::now_ms();
        let id = pane_id.to_string();
        self.update(cx, |c| {
            for w in &mut c.workspaces {
                let Some(at) = w.panes.iter().position(|p| p.id == id) else {
                    continue;
                };
                let pane = w.panes.remove(at);
                w.layout = remove_pane(w.layout.as_ref(), &id);
                if archive {
                    w.history.insert(
                        0,
                        wraithgrid_core::config::ArchivedChat {
                            pane,
                            closed_at: now,
                        },
                    );
                    // Past the limit, the oldest entries go, with their saved views.
                    let max = wraithgrid_core::types::HISTORY_MAX;
                    forget.extend(
                        w.history
                            .drain(max.min(w.history.len())..)
                            .map(|h| h.pane.id),
                    );
                }
            }
        });
        for id in forget {
            let _ = wraithgrid_chat::views::delete_view(&views, &id);
        }
        if self.focused.as_deref() == Some(pane_id) {
            self.focused = pane_ids(self.config().active().layout.as_ref())
                .into_iter()
                .next();
        }
        if self.zoomed.as_deref() == Some(pane_id) {
            self.zoomed = None;
        }
    }

    /// `reopenChat`: a History chat back into its project, same id (its saved view loads and
    /// its first start resumes); `show` switches to it.
    fn reopen_chat(
        &mut self,
        ws_id: &str,
        pane_id: &str,
        show: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let c = self.config();
        let open = c
            .workspaces
            .iter()
            .any(|w| w.panes.iter().any(|p| p.id == pane_id));
        let Some(ws) = c.workspaces.iter().find(|w| w.id == ws_id) else {
            return false;
        };
        if open || !ws.history.iter().any(|h| h.pane.id == pane_id) {
            return false;
        }
        let (ws, id) = (ws_id.to_string(), pane_id.to_string());
        self.update(cx, |c| {
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == ws) {
                let ids: Vec<String> = w.panes.iter().map(|p| p.id.clone()).collect();
                let shown = shown_layout(w.layout.as_ref(), &ids);
                if let Some(at) = w.history.iter().position(|h| h.pane.id == id) {
                    let mut pane = w.history.remove(at).pane;
                    pane.resume = true;
                    w.panes.push(pane);
                    w.layout = Some(place_new_pane(
                        shown.as_ref(),
                        &id,
                        None,
                        None,
                        SplitDir::Row,
                    ));
                }
            }
            if show {
                c.active_workspace = ws.clone();
            }
        });
        if show {
            self.focused = Some(pane_id.to_string());
            self.zoomed = None;
        }
        true
    }

    /// Shows a chat: switches to its project and focuses it, reopening it from History.
    fn reveal_chat(&mut self, ws_id: &str, pane_id: &str, cx: &mut Context<Self>) {
        let open = self
            .config()
            .workspaces
            .iter()
            .find(|w| w.panes.iter().any(|p| p.id == pane_id))
            .map(|w| w.id.clone());
        match open {
            Some(ws) => {
                self.select_workspace(ws, cx);
                self.focused = Some(pane_id.to_string());
                self.zoomed = None;
            }
            None => {
                self.reopen_chat(ws_id, pane_id, true, cx);
            }
        }
        cx.notify();
    }

    /// The pane's terminal view, made the first time it is shown: it reattaches to the
    /// pane's process in the host, or starts it there.
    fn terminal(&mut self, pane: &Pane, cx: &mut Context<Self>) -> Entity<TerminalView> {
        if let Some(view) = self.terminals.get(&pane.id) {
            return view.clone();
        }
        let host = self.host.clone();
        let pane_id = pane.id.clone();
        let writer: PaneWriter = {
            let (host, id) = (host.clone(), pane_id.clone());
            Arc::new(move |data: &str| host.write(&id, data))
        };
        let resizer: PaneResizer = {
            let (host, id) = (host.clone(), pane_id.clone());
            Arc::new(move |cols, rows| host.resize(&id, cols, rows))
        };
        let core = TermCore::new(writer.clone());
        self.router.register(&pane_id, core.clone());
        let spec = self.spawn_spec(pane);
        let account_id = pane.account_id.clone();
        let cfg = self.config().clone();
        let starter: PaneStarter = {
            let core = core.clone();
            Box::new(move |cols, rows, cx: &mut App| {
                cx.background_spawn(async move {
                    if let Some(attached) = host.attach(&spec.pane_id, cols, rows) {
                        if let Some(exit) = attached.exit {
                            core.end(format!(
                                "Exited with code {} while Wraithgrid was closed.",
                                exit.exit_code
                            ));
                        }
                        return;
                    }
                    let spec = SpawnSpec { cols, rows, ..spec };
                    // The provider's shared instructions and skills, linked in first (`syncShared`).
                    if let (false, Some(account)) = (spec.shell, account_id.as_deref()) {
                        crate::mods_glue::sync_before_start(&cfg, account);
                    }
                    let env = spawn_env();
                    let home: PathBuf = home_dir();
                    let s = &cfg.settings;
                    let ctx = ResolveContext {
                        os: Os::current(),
                        env: &env,
                        home: &home,
                        claude_path: &cfg.claude_path,
                        cli_paths: &s.cli_paths,
                        shell_path: &s.shell_path,
                        shell_args: &s.shell_args,
                        mod_dirs: &s.mod_dirs,
                    };
                    if let Err(error) = resolve_spawn(&spec, &ctx).and_then(|req| host.create(req))
                    {
                        core.end(error);
                    }
                })
                .detach();
            })
        };
        let command = if pane.shell {
            "shell".to_string()
        } else {
            pane_label(&self.config().clone(), pane)
        };
        let new_line = if pane.shell {
            None
        } else {
            self.spawn_spec(pane).cli.spec().new_line
        };
        let view =
            cx.new(|cx| TerminalView::new(core, writer, resizer, starter, command, new_line, cx));
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        let submitted = pane.id.clone();
        cx.subscribe(
            &view,
            move |this, _, event: &TerminalEvent, cx| match event {
                TerminalEvent::Submitted => this.mark_conversation(&submitted, cx),
            },
        )
        .detach();
        self.terminals.insert(pane_id, view.clone());
        view
    }

    fn spawn_spec(&self, pane: &Pane) -> SpawnSpec {
        let account = pane
            .account_id
            .as_deref()
            .and_then(|id| self.config().account(id));
        SpawnSpec {
            pane_id: pane.id.clone(),
            cli: account.map_or(AgentCli::Claude, |a| a.cli),
            shell: pane.shell || account.is_none(),
            config_dir: account.map(|a| a.config_dir.clone()),
            cwd: pane.cwd.clone(),
            args: pane.args.clone(),
            // Resume only with a conversation to continue, as the Electron app does.
            resume: pane.resume && pane.has_conversation,
            cols: 80,
            rows: 24,
        }
    }

    fn title_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tiled_edge = match window.window_decorations() {
            Decorations::Client { tiling } => {
                tiling.top || tiling.left || tiling.right || tiling.bottom
            }
            Decorations::Server => false,
        };
        let native_frame = matches!(window.window_decorations(), Decorations::Server)
            && self.chrome == Chrome::Custom;
        let (show_close, show_min_max) = match self.chrome {
            Chrome::Mac => (false, false),
            Chrome::Tiling => (true, false),
            Chrome::Overlay => (true, true),
            Chrome::Custom => (!native_frame, !native_frame && !tiled_edge),
        };
        let control = |id: &'static str,
                       glyph: &'static str,
                       area: WindowControlArea,
                       cx: &mut Context<Self>| {
            div()
                .id(id)
                .w(px(40.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|d| d.bg(cx.theme().secondary))
                .window_control_area(area)
                .child(glyph)
                .on_click(cx.listener(move |this, _, window, cx| match area {
                    WindowControlArea::Close => this.request_close(window, cx),
                    WindowControlArea::Max => window.zoom_window(),
                    _ => window.minimize_window(),
                }))
        };
        let project = self.config().active().name.clone();
        div()
            .id("title-bar")
            .h(px(TITLE_BAR))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2()
            .pl_3()
            .when(self.chrome == Chrome::Mac, |d| d.pl(px(80.)))
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.border)
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.zoom_window();
                } else {
                    window.start_window_move();
                }
            })
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Wraithgrid"))
            .child(div().text_color(theme.muted_foreground).child(project))
            .child(div().flex_1())
            .when(self.blocked.is_none(), |d| {
                d.child(
                    button("new-shell", "+ Terminal", cx)
                        .on_click(cx.listener(|this, _, _, cx| this.new_pane(None, cx))),
                )
                .child(
                    button("new-agent", "+ New chat", cx).on_click(
                        cx.listener(|this, _, window, cx| this.open_new_pane(window, cx)),
                    ),
                )
            })
            .when_some(self.update_button(), |d, (label, enabled)| {
                d.child(
                    button("update", "", cx)
                        .child(label)
                        .when(enabled, |b| {
                            b.bg(cx.theme().primary)
                                .text_color(cx.theme().primary_foreground)
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.run_update(cx))),
                )
            })
            .when(show_min_max, |d| {
                d.child(control("min", "–", WindowControlArea::Min, cx))
                    .child(control("max", "□", WindowControlArea::Max, cx))
            })
            .when(show_close, |d| {
                d.child(control("close", "✕", WindowControlArea::Close, cx))
            })
    }

    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let c = self.config().clone();
        let width = c.settings.sidebar_width as f32;
        let section = |label: &'static str| {
            div()
                .px_3()
                .pt_3()
                .pb_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label)
        };
        let projects = c.workspaces.iter().map(|w| {
            let active = w.id == c.active_workspace;
            let id = w.id.clone();
            let tint = parse_hex(&w.color).unwrap_or(theme.muted_foreground);
            div()
                .id(SharedString::from(format!("ws-{}", w.id)))
                .mx_2()
                .px_2()
                .py_1()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .when(active, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
                .child(badge(&w.name, tint))
                .child(div().flex_1().truncate().child(w.name.clone()))
                .child(div().text_xs().text_color(theme.muted_foreground).child(
                    if w.panes.is_empty() {
                        String::new()
                    } else {
                        w.panes.len().to_string()
                    },
                ))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| this.open_project(id.clone(), window, cx)
                    }),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_project_page(id.clone(), window, cx)
                }))
        });
        let accounts = c.accounts.iter().map(|a| {
            let color = parse_hex(&a.color).unwrap_or(theme.primary);
            div()
                .mx_2()
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .child(badge(&a.name, color))
                .child(div().flex_1().truncate().child(a.name.clone()))
                .child(match self.limits_label(&a.id) {
                    Some((text, worst)) => {
                        let at = self.config().settings.auto_switch_at as f64 / 100.0;
                        let color = if worst >= at {
                            theme.danger
                        } else if worst >= 0.8 {
                            theme.warning
                        } else {
                            theme.muted_foreground
                        };
                        div().text_xs().text_color(color).child(text)
                    }
                    None => div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(a.cli.spec().provider_label),
                })
        });
        div()
            .id("sidebar")
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .text_sm()
            .child(
                div()
                    .id("sidebar-list")
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().flex_1().child(section("Projects")))
                            .child(
                                div()
                                    .id("new-project")
                                    .mr_3()
                                    .mt_2()
                                    .px_1()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .text_color(theme.muted_foreground)
                                    .hover(|d| d.bg(theme.secondary))
                                    .child("+")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_new_project(window, cx)
                                    })),
                            ),
                    )
                    .children(projects)
                    .child(
                        div()
                            .id("open-accounts")
                            .cursor_pointer()
                            .hover(|d| d.text_color(theme.foreground))
                            .child(section("Accounts"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_accounts(window, cx)),
                            ),
                    )
                    .children(accounts),
            )
            .child(
                div()
                    .id("open-settings")
                    .m_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|d| d.bg(theme.secondary))
                    .child("⚙ Settings")
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
            )
            .child(
                div()
                    .id("open-usage")
                    .mx_2()
                    .mb_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|d| d.bg(theme.secondary))
                    .child("◔ Usage")
                    .on_click(cx.listener(|this, _, window, cx| this.open_usage(window, cx))),
            )
            .child(
                div()
                    .id("open-mods")
                    .mx_2()
                    .mb_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|d| d.bg(theme.secondary))
                    .child("✦ Mods")
                    .on_click(cx.listener(|this, _, window, cx| this.open_mods(window, cx))),
            )
            .child(
                div()
                    .id("open-scheduled")
                    .mx_2()
                    .mb_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|d| d.bg(theme.secondary))
                    .child("◷ Scheduled")
                    .on_click(cx.listener(|this, _, window, cx| this.open_scheduled(window, cx))),
            )
    }

    fn grid(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(view) = &self.settings_view {
            return view.clone().into_any_element();
        }
        if let Some(view) = &self.accounts_view {
            return view.clone().into_any_element();
        }
        if let Some(view) = &self.usage_view {
            return view.clone().into_any_element();
        }
        if let Some(view) = &self.mods_view {
            return view.clone().into_any_element();
        }
        if let Some(view) = &self.scheduled_view {
            return view.clone().into_any_element();
        }
        if let Some(view) = &self.project_view {
            return view.clone().into_any_element();
        }
        if let Some(message) = &self.blocked {
            return empty_state(message.clone(), cx).into_any_element();
        }
        let ws = self.config().active().clone();
        let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
        if let Some(p) = self
            .zoomed
            .as_ref()
            .and_then(|z| ws.panes.iter().find(|p| &p.id == z))
        {
            return self.pane(&p.clone(), window, cx);
        }
        match shown_layout(ws.layout.as_ref(), &ids) {
            None => empty_state(
                "No panes in this project yet. Start one with + Terminal or + Agent.".into(),
                cx,
            )
            .into_any_element(),
            Some(layout) => self.node(&layout, &ws.panes, vec![], window, cx),
        }
    }

    fn node(
        &mut self,
        node: &LayoutNode,
        panes: &[Pane],
        path: Vec<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            LayoutNode::Pane { pane_id } => match panes.iter().find(|p| &p.id == pane_id) {
                Some(p) => self.pane(&p.clone(), window, cx),
                None => div().into_any_element(),
            },
            LayoutNode::Empty { .. } => div().size_full().into_any_element(),
            LayoutNode::Split {
                dir,
                sizes,
                children,
            } => {
                let border = cx.theme().border;
                let row = *dir == SplitDir::Row;
                let mut container = div().size_full().flex().bg(border);
                container = if row {
                    container.flex_row()
                } else {
                    container.flex_col()
                };
                let bounds = self.split_bounds.clone();
                let key = path.clone();
                container = container.child(
                    canvas(
                        move |b, _, _| bounds.borrow_mut().insert(key, b),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                );
                for (i, (child, size)) in children.iter().zip(sizes).enumerate() {
                    if i > 0 {
                        // A divider: drag it to share the space between its neighbours.
                        let (drag_path, dir) = (path.clone(), *dir);
                        let handle = div()
                            .id(SharedString::from(format!("divider-{path:?}-{i}")))
                            .flex_shrink_0()
                            .when(row, |d| d.w(px(4.)).h_full().cursor_col_resize())
                            .when(!row, |d| d.h(px(4.)).w_full().cursor_row_resize())
                            .hover(|d| d.bg(cx.theme().ring))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                    this.start_drag(drag_path.clone(), i - 1, dir, e.position);
                                    cx.stop_propagation();
                                }),
                            );
                        container = container.child(handle);
                    }
                    let mut child_path = path.clone();
                    child_path.push(i);
                    let cell = div()
                        .flex_basis(relative(*size as f32 / 100.))
                        .flex_grow(1.)
                        .flex_shrink(1.)
                        .min_w(px(0.))
                        .min_h(px(0.))
                        .child(self.node(child, panes, child_path, window, cx));
                    container = container.child(cell);
                }
                container.into_any_element()
            }
        }
    }

    fn start_drag(&mut self, path: Vec<usize>, index: usize, dir: SplitDir, at: Point<Pixels>) {
        let ws = self.config().active();
        let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
        let Some(layout) = shown_layout(ws.layout.as_ref(), &ids) else {
            return;
        };
        let Some(LayoutNode::Split { sizes, .. }) = node_at(&layout, &path) else {
            return;
        };
        let Some(bounds) = self.split_bounds.borrow().get(&path).copied() else {
            return;
        };
        let extent = if dir == SplitDir::Row {
            bounds.size.width
        } else {
            bounds.size.height
        };
        self.drag = Some(DividerDrag {
            path,
            index,
            dir,
            start: at,
            start_sizes: sizes.clone(),
            extent: f32::from(extent),
            layout,
        });
    }

    fn drag_to(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(d) = &self.drag else { return };
        let moved = if d.dir == SplitDir::Row {
            at.x - d.start.x
        } else {
            at.y - d.start.y
        };
        let delta = f64::from(f32::from(moved) / d.extent.max(1.0) * 100.0);
        let (a, b) = (d.start_sizes[d.index], d.start_sizes[d.index + 1]);
        // Neither neighbour goes below 5% of the split.
        let delta = delta.clamp(5.0 - a, b - 5.0);
        let mut sizes = d.start_sizes.clone();
        sizes[d.index] = a + delta;
        sizes[d.index + 1] = b - delta;
        let layout = set_sizes_at(&d.layout, &d.path, &sizes);
        self.update(cx, |c| {
            let active = c.active_workspace.clone();
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == active) {
                w.layout = Some(layout);
            }
        });
    }

    /// Something was sent in the pane: next launch continues its conversation.
    fn mark_conversation(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        let has = self
            .config()
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .any(|p| p.id == pane_id && p.has_conversation);
        if !has {
            self.update(cx, |c| {
                for p in c
                    .workspaces
                    .iter_mut()
                    .flat_map(|w| w.panes.iter_mut())
                    .filter(|p| p.id == pane_id)
                {
                    p.has_conversation = true;
                }
            });
        }
    }

    /// App shortcuts, before the focused terminal sees the key.
    fn shortcut(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        let k = &event.keystroke;
        let m = k.modifiers;
        let press = Press {
            key: &k.key,
            ctrl: m.control,
            shift: m.shift,
            alt: m.alt,
            meta: m.platform,
        };
        let Some(action) = match_shortcut(
            press,
            cfg!(target_os = "macos"),
            &bindings(&self.config().settings.shortcuts),
        ) else {
            return;
        };
        cx.stop_propagation();
        match action {
            Action::Shortcut(ShortcutId::NewPane) => self.open_new_pane(window, cx),
            Action::Shortcut(ShortcutId::Palette) => self.open_palette(window, cx),
            Action::Shortcut(ShortcutId::ClosePane) => {
                if let Some(id) = self.focused.clone() {
                    self.close_pane(id, cx);
                }
            }
            Action::Shortcut(ShortcutId::ToggleZoom) => {
                self.zoomed = if self.zoomed.is_some() {
                    None
                } else {
                    self.focused.clone()
                };
                cx.notify();
            }
            Action::Shortcut(ShortcutId::Diff) => self.toggle_changes(window, cx),
            // The shortcuts sheet comes with its screen.
            Action::Shortcut(_) => {}
            Action::Focus(dir) => self.move_focus(dir, window, cx),
            Action::Workspace(i) => {
                if let Some(id) = self.config().workspaces.get(i).map(|w| w.id.clone()) {
                    self.select_workspace(id, cx);
                }
            }
            Action::UiZoom(delta) => {
                let next = step_zoom(self.config().settings.ui_zoom, delta);
                self.update(cx, |c| c.settings.ui_zoom = next);
            }
        }
    }

    /// Focuses the nearest pane in a direction, by where panes were drawn.
    fn move_focus(&mut self, dir: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.pane_bounds.borrow().clone();
        let Some(from) = self.focused.as_ref().and_then(|f| bounds.get(f)).copied() else {
            return;
        };
        let c = from.center();
        let best = bounds
            .iter()
            .filter(|(id, _)| Some(*id) != self.focused.as_ref())
            .filter_map(|(id, b)| {
                let o = b.center();
                let (along, across) = match dir {
                    Direction::Left => (c.x - o.x, o.y - c.y),
                    Direction::Right => (o.x - c.x, o.y - c.y),
                    Direction::Up => (c.y - o.y, o.x - c.x),
                    Direction::Down => (o.y - c.y, o.x - c.x),
                };
                (along > px(0.))
                    .then(|| (id.clone(), f32::from(along) + f32::from(across).abs() * 2.0))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((id, _)) = best {
            if let Some(view) = self.terminals.get(&id) {
                window.focus(&view.focus_handle(cx), cx);
            }
            self.focused = Some(id);
            cx.notify();
        }
    }

    fn pane(&mut self, pane: &Pane, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let config = self.config().clone();
        let chat = pane.view == PaneView::Chat && !self.as_terminal.contains(&pane.id);
        let label = pane_label(&config, pane);
        let id = pane.id.clone();
        let (body, status, stopped): (AnyElement, String, bool) = if chat {
            let view = self.chat(pane, window, cx);
            let status = match view.read(cx).status() {
                wraithgrid_chat::ChatStatus::Running => "working",
                wraithgrid_chat::ChatStatus::Approval => "needs approval",
                wraithgrid_chat::ChatStatus::Background => "in background",
                wraithgrid_chat::ChatStatus::Idle => "chat",
            };
            if view.focus_handle(cx).contains_focused(window, cx)
                || view
                    .read(cx)
                    .composer_focus(cx)
                    .contains_focused(window, cx)
            {
                self.focused = Some(pane.id.clone());
            }
            (
                div().size_full().child(view).into_any_element(),
                status.to_string(),
                false,
            )
        } else {
            let view = self.terminal(pane, cx);
            let (title, ended) = {
                let v = view.read(cx);
                (v.title(), v.ended())
            };
            let focused = view.focus_handle(cx).contains_focused(window, cx);
            if focused {
                self.focused = Some(pane.id.clone());
            }
            let stopped = ended.is_some();
            let status = ended.unwrap_or(title);
            (
                div().size_full().child(view).into_any_element(),
                status,
                stopped,
            )
        };
        let focused = self.focused.as_deref() == Some(pane.id.as_str());
        let close = id.clone();
        let (bounds, key) = (self.pane_bounds.clone(), pane.id.clone());
        div()
            .relative()
            .child(
                canvas(
                    move |b, _, _| bounds.borrow_mut().insert(key, b),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .border_1()
            .border_color(if focused {
                theme.ring
            } else {
                theme.background
            })
            .child(
                div()
                    .h(px(26.))
                    .flex_shrink_0()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .bg(theme.title_bar)
                    .child(status_dot(
                        match self.chat_states.get(&pane.id) {
                            Some(
                                wraithgrid_chat::ChatStatus::Running
                                | wraithgrid_chat::ChatStatus::Background,
                            ) => Some("running"),
                            Some(wraithgrid_chat::ChatStatus::Approval) => Some("blocked"),
                            Some(wraithgrid_chat::ChatStatus::Idle) => Some("idle"),
                            None => self.states.get(&pane.id).map(String::as_str),
                        },
                        &theme,
                    ))
                    .child(div().font_weight(FontWeight::MEDIUM).child(label))
                    .when_some(self.git_heads.get(&pane.cwd).filter(|g| g.repo), |d, g| {
                        let branch = g.branch.clone().unwrap_or_else(|| "detached".into());
                        let text = if g.changed > 0 {
                            format!("⑂ {branch} · {}", g.changed)
                        } else {
                            format!("⑂ {branch}")
                        };
                        d.child(div().text_color(theme.muted_foreground).child(text))
                    })
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    )
                    .when(
                        chat && pane
                            .account_id
                            .as_deref()
                            .and_then(|a| config.account(a))
                            .is_some_and(|a| a.cli.can_continue_as()),
                        |d| {
                            let switch = id.clone();
                            d.child(
                                div()
                                    .id(SharedString::from(format!("continue-{id}")))
                                    .px_2()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .text_color(theme.muted_foreground)
                                    .hover(|d| d.bg(theme.secondary))
                                    .child("Continue as…")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.ask_continue_as(switch.clone(), window, cx)
                                    })),
                            )
                        },
                    )
                    .when(stopped, |d| {
                        let restart = id.clone();
                        d.child(
                            div()
                                .id(SharedString::from(format!("restart-{id}")))
                                .px_2()
                                .rounded_sm()
                                .border_1()
                                .border_color(theme.border)
                                .cursor_pointer()
                                .hover(|d| d.bg(theme.secondary))
                                .child("Restart")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.restart_pane(restart.clone(), cx)
                                })),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("close-{id}")))
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(theme.secondary))
                            .child("✕")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_pane(close.clone(), cx)
                            })),
                    ),
            )
            .child(div().flex_1().min_h(px(0.)).overflow_hidden().child(body))
            .into_any_element()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let settings = &self.config().settings;
        let zoom = settings.ui_zoom as f32;
        window.set_rem_size(px(16.0 * zoom));
        let style = TerminalStyle {
            family: settings.font_family.as_str().into(),
            size: settings.font_size as f32 * zoom,
        };
        if cx.try_global::<TerminalStyle>() != Some(&style) {
            cx.set_global(style);
        }
        // Panes no longer shown forget where they were drawn.
        let shown = pane_ids(
            shown_layout(
                self.config().active().layout.as_ref(),
                &self
                    .config()
                    .active()
                    .panes
                    .iter()
                    .map(|p| p.id.clone())
                    .collect::<Vec<_>>(),
            )
            .as_ref(),
        );
        self.pane_bounds
            .borrow_mut()
            .retain(|id, _| shown.contains(id));
        let grid = self.grid(window, cx);
        let modal: Option<AnyElement> = self.modal.as_ref().map(|m| match m {
            Modal::NewPane(d) => d.clone().into_any_element(),
            Modal::Palette(p) => p.clone().into_any_element(),
            Modal::Quit(q) => q.clone().into_any_element(),
            Modal::NewProject(d) => d.clone().into_any_element(),
            Modal::Project(d) => d.clone().into_any_element(),
            Modal::Choice(d) => d.clone().into_any_element(),
        });
        div()
            .id("shell")
            .relative()
            .capture_key_down(cx.listener(Self::shortcut))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.drag.is_some() {
                    this.drag_to(e.position, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag = None),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .text_sm()
            .child(self.title_bar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(self.sidebar(cx))
                    .child(div().flex_1().min_w(px(0.)).h_full().child(grid))
                    .when_some(
                        self.changes.clone().filter(|_| {
                            self.settings_view.is_none() && self.accounts_view.is_none()
                        }),
                        |d, panel| {
                            d.child(
                                div()
                                    .w(px(self.config().settings.diff_width as f32))
                                    .h_full()
                                    .flex_shrink_0()
                                    .border_l_1()
                                    .border_color(theme.border)
                                    .child(panel),
                            )
                        },
                    ),
            )
            .when_some(modal, |d, modal| {
                d.child(
                    div()
                        .id("overlay")
                        .absolute()
                        .inset_0()
                        .flex()
                        .justify_center()
                        .pt(px(90.))
                        .bg(hsla(0., 0., 0., 0.45))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| this.close_modal(window, cx)),
                        )
                        .child(
                            div()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(modal),
                        ),
                )
            })
    }
}

/// A pane's header label: its title, else its account's name, else its folder.
fn pane_label(config: &Config, pane: &Pane) -> String {
    if !pane.title.is_empty() {
        return pane.title.clone();
    }
    if let Some(account) = pane.account_id.as_deref().and_then(|id| config.account(id)) {
        return account.name.clone();
    }
    base_name(&pane.cwd)
}

/// A chat's name from its first message, like a chat app's history: one line, ~6 words
/// (`titleFromMessage`).
fn title_from_message(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= 48 {
        return line;
    }
    let cut: String = line.chars().take(48).collect();
    let word = cut.rfind(' ').filter(|w| cut[..*w].chars().count() > 24);
    let base = match word {
        Some(w) => &cut[..w],
        None => cut.as_str(),
    };
    format!(
        "{}…",
        base.trim_end_matches(|c: char| c.is_whitespace() || ",.;:!?-".contains(c))
    )
}

/// Who each pane belongs to, for the usage store.
fn pane_owners(c: &Config) -> HashMap<String, wraithgrid_usage::store::PaneOwner> {
    c.workspaces
        .iter()
        .flat_map(|w| {
            w.panes.iter().map(move |p| {
                (
                    p.id.clone(),
                    wraithgrid_usage::store::PaneOwner {
                        account_id: p.account_id.clone(),
                        workspace_id: Some(w.id.clone()),
                        title: p.title.clone(),
                    },
                )
            })
        })
        .collect()
}

/// The local time as HH:MM, for dividers.
fn chrono_like_time() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    // The usage crate formats local time already; reuse its clock-free formatter.
    wraithgrid_usage::format::local_hhmm(secs * 1000)
}

/// A new pane's title: `shell`, else its folder's name (`home` for ~).
fn auto_title(shell: bool, cwd: &str) -> String {
    if shell {
        return "shell".into();
    }
    match base_name(cwd).as_str() {
        "~" => "home".into(),
        name => name.into(),
    }
}

pub(crate) fn parse_hex(s: &str) -> Option<Hsla> {
    let hex = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    u32::from_str_radix(hex, 16).ok().map(|v| rgb(v).into())
}

/// A rounded square with the name's first letter (projects and accounts without an icon).
fn badge(name: &str, color: Hsla) -> impl IntoElement {
    let letter = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let mut bg = color;
    bg.a = 0.22;
    div()
        .size(px(20.))
        .flex_shrink_0()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .bg(bg)
        .text_color(color)
        .child(letter)
}

fn button(id: impl Into<ElementId>, label: &'static str, cx: &mut Context<Shell>) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px_2()
        .py(px(3.))
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .text_xs()
        .cursor_pointer()
        .hover(|d| d.bg(theme.secondary))
        .child(label)
}

fn empty_state(message: String, cx: &mut Context<Shell>) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p_6()
        .text_color(cx.theme().muted_foreground)
        .child(message)
}

struct DividerDrag {
    path: Vec<usize>,
    /// The divider sits after child `index`.
    index: usize,
    dir: SplitDir,
    start: Point<Pixels>,
    start_sizes: Vec<f64>,
    /// The split's length along the drag, in pixels.
    extent: f32,
    layout: LayoutNode,
}

fn node_at<'a>(node: &'a LayoutNode, path: &[usize]) -> Option<&'a LayoutNode> {
    match path.split_first() {
        None => Some(node),
        Some((i, rest)) => match node {
            LayoutNode::Split { children, .. } => children.get(*i).and_then(|c| node_at(c, rest)),
            _ => None,
        },
    }
}

/// The pane's state as a dot: running green, needs approval amber, idle grey, exited red.
fn status_dot(state: Option<&str>, theme: &gpui_kit::component::Theme) -> impl IntoElement {
    let color: Hsla = match state {
        Some("running") => rgb(0x34d399).into(),
        Some("blocked") => rgb(0xf5a524).into(),
        Some("exited") => rgb(0xf43f5e).into(),
        _ => theme.muted_foreground,
    };
    div().size(px(7.)).rounded_full().flex_shrink_0().bg(color)
}

enum Modal {
    NewPane(Entity<NewPaneDialog>),
    Palette(Entity<Palette>),
    Quit(Entity<QuitDialog>),
    NewProject(Entity<NewProjectDialog>),
    Project(Entity<ProjectDialog>),
    Choice(Entity<ChoiceDialog>),
}

/// Where the in-app update is (`update-install.ts` phases).
enum UpdateState {
    Idle,
    Checking,
    Available(update::ReleaseInfo),
    Downloading {
        version: String,
        percent: u8,
    },
    Ready {
        release: update::ReleaseInfo,
        file: std::path::PathBuf,
        kind: update::PackageKind,
    },
    Installing(String),
    Error(String),
}

// ---- Scheduled chats (#77) -----------------------------------------------------------------

impl Shell {
    /// Ticks the scheduled chats while the window is open (`tickSchedules`): the schedule crate
    /// decides, this does what it says.
    pub fn start_schedules(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        self._schedule_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(wraithgrid_schedule::FIRST_TICK_MS))
                .await;
            loop {
                if this
                    .update_in(cx, |this, window, cx| this.tick_schedules(window, cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(wraithgrid_schedule::SCHEDULE_TICK_MS))
                    .await;
            }
        }));
    }

    fn job_limits(&self, account_id: &str) -> Option<wraithgrid_schedule::Limits> {
        let l = self.limits.get(account_id)?;
        let w = |w: Option<wraithgrid_usage::model::UsageWindow>| {
            w.map(|w| wraithgrid_schedule::WindowLimit {
                utilization: w.utilization,
                resets_at: w.resets_at,
            })
        };
        Some(wraithgrid_schedule::Limits {
            five_hour: w(l.five_hour),
            seven_day: w(l.seven_day),
        })
    }

    fn tick_schedules(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settle_runs(cx);
        let now = wraithgrid_usage::now_ms();
        let tz = chrono::Local;
        for job in self.config().schedules.clone() {
            let limits = self.job_limits(&job.account_id);
            match wraithgrid_schedule::tick_job(&tz, &job, now, limits.as_ref()) {
                wraithgrid_schedule::Tick::Idle => {}
                wraithgrid_schedule::Tick::Update(j) => self.replace_job(j, cx),
                wraithgrid_schedule::Tick::Run => {
                    let run = self.start_job(&job, now, window, cx);
                    self.mark_live(&job.id, &run);
                    let change = wraithgrid_schedule::after_tick_run(
                        &tz,
                        job.clone(),
                        run,
                        now,
                        limits.as_ref(),
                    );
                    self.apply_job_change(&job.id, change, cx);
                }
            }
        }
    }

    pub fn run_job_now(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(job) = self.config().schedules.iter().find(|j| j.id == id).cloned() else {
            return;
        };
        let now = wraithgrid_usage::now_ms();
        let run = self.start_job(&job, now, window, cx);
        self.mark_live(&job.id, &run);
        let change = wraithgrid_schedule::after_run_now(job, run);
        self.apply_job_change(id, change, cx);
    }

    fn replace_job(&mut self, job: wraithgrid_core::config::ScheduledJob, cx: &mut Context<Self>) {
        self.update(cx, |c| {
            if let Some(j) = c.schedules.iter_mut().find(|j| j.id == job.id) {
                *j = job;
            }
        });
    }

    fn apply_job_change(
        &mut self,
        id: &str,
        change: wraithgrid_schedule::JobChange,
        cx: &mut Context<Self>,
    ) {
        match change {
            wraithgrid_schedule::JobChange::Update(j) => self.replace_job(j, cx),
            wraithgrid_schedule::JobChange::Delete => {
                let id = id.to_string();
                self.update(cx, |c| c.schedules.retain(|j| j.id != id));
            }
        }
    }

    /// Starts one run: the prompt into the job's chat, or a new chat in its project.
    fn start_job(
        &mut self,
        job: &wraithgrid_core::config::ScheduledJob,
        now: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> wraithgrid_core::config::ScheduleRun {
        let c = self.config().clone();
        let account = c.account(&job.account_id);
        let accounts = self.usage_accounts();
        let from = accounts.iter().find(|a| a.id == job.account_id);
        let threshold = c.settings.auto_switch_at;
        let switch_to = from.and_then(|f| {
            wraithgrid_usage::auto_switch::next_account(&accounts, f, &self.limits, now, threshold)
        });
        let at_limit =
            wraithgrid_usage::auto_switch::peak_use(self.limits.get(&job.account_id), now)
                .is_some_and(|p| p.pct >= 100);
        let last_pane = job.runs.first().and_then(|r| r.pane_id.clone());
        let last_status = last_pane
            .as_ref()
            .and_then(|p| self.chat_states.get(p))
            .map(|s| match s {
                wraithgrid_chat::ChatStatus::Running => "running",
                wraithgrid_chat::ChatStatus::Approval => "approval",
                wraithgrid_chat::ChatStatus::Background => "background",
                wraithgrid_chat::ChatStatus::Idle => "idle",
            });
        let ctx = wraithgrid_schedule::RunContext {
            now,
            project_exists: c.workspaces.iter().any(|w| w.id == job.ws_id),
            account: account.map(|a| wraithgrid_schedule::AccountRef {
                id: &a.id,
                name: &a.name,
            }),
            last_pane_status: last_status,
            at_limit,
            auto_switch_off: c.settings.auto_switch_mode
                == wraithgrid_core::types::AutoSwitchMode::Off,
            switch_to: switch_to.map(|a| wraithgrid_schedule::AccountRef {
                id: &a.id,
                name: &a.name,
            }),
        };
        match wraithgrid_schedule::start_run(job, &ctx) {
            wraithgrid_schedule::RunStart::Record(run) => run,
            wraithgrid_schedule::RunStart::Chat { pane_id } => match {
                self.reopen_chat(&job.ws_id, &pane_id, false, cx);
                self.config().clone()
            }
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .find(|p| p.id == pane_id)
            .cloned()
            {
                Some(pane) => {
                    let chat = self.chat(&pane, window, cx);
                    let prompt = job.prompt.clone();
                    chat.update(cx, |v, cx| v.resend(&prompt, window, cx));
                    wraithgrid_schedule::running(now, &pane_id, "")
                }
                None => wraithgrid_schedule::chat_gone(now),
            },
            wraithgrid_schedule::RunStart::NewChat { account_id, note } => {
                let Some(ws) = c.workspaces.iter().find(|w| w.id == job.ws_id) else {
                    return wraithgrid_schedule::chat_gone(now);
                };
                let Some(folder) = ws.project_folder() else {
                    return wraithgrid_schedule::no_folder(now);
                };
                let id = self.add_pane(
                    Some(ws.id.clone()),
                    Some(account_id),
                    folder,
                    vec![],
                    false,
                    true,
                    cx,
                );
                let (model, effort, mode) = (job.model.clone(), job.effort, job.chat_mode);
                self.update(cx, |c| {
                    if let Some(p) = c
                        .workspaces
                        .iter_mut()
                        .flat_map(|w| &mut w.panes)
                        .find(|p| p.id == id)
                    {
                        p.model = model;
                        p.effort = effort;
                        p.chat_mode = mode;
                    }
                });
                if let Some(pane) = self
                    .config()
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.panes)
                    .find(|p| p.id == id)
                    .cloned()
                {
                    let chat = self.chat(&pane, window, cx);
                    let prompt = job.prompt.clone();
                    chat.update(cx, |v, cx| v.resend(&prompt, window, cx));
                }
                wraithgrid_schedule::running(now, &id, note)
            }
        }
    }

    fn mark_live(&mut self, job_id: &str, run: &wraithgrid_core::config::ScheduleRun) {
        if run.result == "running" {
            self.live_runs
                .insert(wraithgrid_schedule::live_key(job_id, run.at));
        }
    }

    /// `settleRun` for every job: a running run whose chat went quiet is done; one whose chat
    /// is gone, exited, or left over from an earlier launch ends as the crate says.
    fn settle_runs(&mut self, cx: &mut Context<Self>) {
        let now = wraithgrid_usage::now_ms();
        let mut ends = vec![];
        for job in &self.config().schedules {
            let Some(run) = job.runs.first().filter(|r| r.result == "running") else {
                continue;
            };
            let Some(pane_id) = run.pane_id.as_deref() else {
                continue;
            };
            let c = self.config();
            let open = c
                .workspaces
                .iter()
                .find_map(|w| w.panes.iter().find(|p| p.id == pane_id).map(|p| (w, p)));
            let pane = match open {
                Some(_) => wraithgrid_schedule::PaneWhere::Open,
                None if c
                    .workspaces
                    .iter()
                    .any(|w| w.history.iter().any(|h| h.pane.id == pane_id)) =>
                {
                    wraithgrid_schedule::PaneWhere::History
                }
                None => wraithgrid_schedule::PaneWhere::Gone,
            };
            let chat = self.chats.get(pane_id).map(|v| v.read(cx));
            let quiet = open.is_some_and(|(_, p)| p.has_conversation)
                && chat.is_some_and(|v| !v.is_busy())
                && self
                    .chat_states
                    .get(pane_id)
                    .is_none_or(|s| *s == wraithgrid_chat::ChatStatus::Idle);
            let view = wraithgrid_schedule::RunView {
                live: self
                    .live_runs
                    .contains(&wraithgrid_schedule::live_key(&job.id, run.at)),
                pane,
                exited: false,
                quiet,
                now,
            };
            if let Some(end) = wraithgrid_schedule::settle_of(run, &view) {
                let place = open.map(|(w, _)| w.name.clone());
                ends.push((job.id.clone(), run.at, end, place, job.prompt.clone()));
            }
        }
        for (id, at, end, place, prompt) in ends {
            self.live_runs
                .remove(&wraithgrid_schedule::live_key(&id, at));
            if let Some(job) = self.config().schedules.iter().find(|j| j.id == id).cloned() {
                self.replace_job(wraithgrid_schedule::settled(job, &end), cx);
            }
            if let (true, Some(place)) = (end.notify, place) {
                cx.show_system_notification(SystemNotification {
                    tag: format!("run-{id}").into(),
                    title: format!("Scheduled run done · {place}")
                        .chars()
                        .take(120)
                        .collect::<String>()
                        .into(),
                    body: prompt.chars().take(200).collect::<String>().into(),
                    actions: vec![],
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::title_from_message;

    #[test]
    fn chat_titles() {
        assert_eq!(title_from_message("  fix   the\nbuild "), "fix the build");
        assert_eq!(
            title_from_message(
                "please review the uncommitted changes in the git panel and tell me what breaks"
            ),
            "please review the uncommitted changes in the…"
        );
    }
}
