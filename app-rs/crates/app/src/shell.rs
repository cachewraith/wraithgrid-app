//! The window (`src/renderer/app/App.tsx`, `Sidebar.tsx`, `TopBar.tsx`, `PaneGrid.tsx`): title
//! bar, sidebar with projects and accounts, and the active project's panes in its saved split
//! layout. The config is the state: every change goes through the store and is saved shortly
//! after, in the same file and shape the Electron app uses.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::a11y::A11y as _;
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
    ChoiceDialog, ChoiceEvent, CreateRemoteProject, NewPaneDialog, NewPaneEvent, NewProjectDialog,
    NewProjectEvent, OpenAccounts, Palette, PaletteAction, PaletteCommand, PaletteEvent,
    PinFolders, ProjectDialog, ProjectEvent, QuitDialog, QuitEvent,
};
use crate::mods_view::{ModsRequest, ModsView, ModsViewEvent};
use crate::project_view::{ProjectPageEvent, ProjectView};
use crate::runtime::{spawn_env, Router};
use crate::scheduled_view::{ScheduledEvent, ScheduledView};
use crate::settings::{SettingsEvent, SettingsView};
use crate::shortcuts_sheet::{ShortcutsSheet, ShortcutsSheetEvent};
use crate::terminal::{
    PaneResizer, PaneStarter, PaneWriter, TermCore, TerminalEvent, TerminalStyle, TerminalView,
};
use crate::usage_view::{UsageView, UsageViewEvent};
use icons::{glyph, small, z, Glyph};

mod auto_switch;
mod empty;
mod git_button;
mod handoff;
mod icons;
mod later;
mod machines;
mod menu;
mod pane_bars;
mod pane_menu;
mod preview;
mod sidebar;
mod status_bar;
mod team;
mod top_bar;
mod tray;

pub use machines::{probe_text, remote_host};

/// The window's top strip where the OS draws its buttons (macOS, Windows' overlay).
const TITLE_BAR: f32 = 36.0;
/// The top row over the panes (`.top`), and one pane's header alone in it (#89).
const TOP_ROW: f32 = 48.0;
const SOLO_ROW: f32 = 44.0;
/// The gap between the sidebar, main and Changes cards, and their corner (#88).
const PANEL_GAP: f32 = 6.0;
const PANEL_R: f32 = 10.0;
/// Split panes keep at least this much room when a divider is dragged.
const MIN_COL_PX: f32 = 180.0;
const MIN_ROW_PX: f32 = 110.0;
/// A pane drag starts only after the pointer moved this far (dnd-kit's distance).
const PANE_DRAG_PX: f32 = 6.0;
/// Six or more panes on screen switch to the compact density (`DENSE_PANE_COUNT`), where
/// terminals draw at most this size (`DENSE_FONT_SIZE`).
const DENSE_PANE_COUNT: usize = 6;
const DENSE_FONT_SIZE: f32 = 11.0;
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
    /// Shown while the window is closed but the app keeps running (`keepInTray`).
    tray: Option<crate::tray::Tray>,
    _tray_tasks: Vec<Task<()>>,
    _second_launch: Option<Task<()>>,
    /// The window's own focus: holds the keys when no pane or screen has them, so shortcuts
    /// and Esc still reach the shell.
    focus: Option<FocusHandle>,
    /// A window was attached once: later ones are reopens from the tray.
    opened: bool,
    /// Each machine's host (H5), by machine id, with the ssh host it was made for.
    remotes: HashMap<String, (String, Arc<wraithgrid_remote::RemoteHost>)>,
    /// Scheduled runs started by this launch (`live_key`); others left running are stale.
    live_runs: std::collections::HashSet<String>,
    /// Chat panes opened as terminals this session (until the chat view is ported).
    as_terminal: Vec<String>,
    /// Panes whose sign-in banner was closed, and those where /login was run.
    banner_hidden: Vec<String>,
    login_started: Vec<String>,
    /// Account-switch questions waiting, oldest first (#161).
    switch_asks: Vec<auto_switch::SwitchAsk>,
    /// Panes answered Stay, by the account they stayed on.
    switch_declined: HashMap<String, String>,
    told_none_left: HashSet<String>,
    switching: HashSet<String>,
    /// The Changes panel's width while its edge is dragged.
    diff_drag: Option<f32>,
    /// The top row's ⋯ menu, open at a point.
    utility_menu: Option<Point<Pixels>>,
    /// The empty layout slot a new pane goes into ("New pane here").
    pending_slot: Option<String>,
    /// This version, once after an update restarted the app ("Updated to X").
    pub updated_to: Option<String>,
    /// A pane's menu, open at a point (`Pane.tsx`'s ContextMenu).
    pane_menu: Option<(String, Point<Pixels>)>,
    /// The pane whose close asks first: its process still works.
    closing: Option<String>,
    /// The confirm's Cancel button, which takes the keyboard when it shows.
    close_focus: Option<FocusHandle>,
    close_focus_pending: bool,
    /// Web preview panes' views (#74), made when first shown.
    previews: HashMap<String, Entity<crate::preview_view::PreviewView>>,
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
    /// The top bar's Git menu, while open (#162).
    git_menu: Option<git_button::GitMenu>,
    sidebar: sidebar::SidebarState,
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
    /// The open menu's keyboard and focus (`menu.rs`).
    menu_nav: menu::MenuNav,
    /// Each account's plan once a status check said it ("Pro", "Max").
    plans: HashMap<String, String>,
    _plan_task: Option<Task<()>>,
    /// Each pane's process id in the host, for the close confirm.
    pids: HashMap<String, i64>,
    /// When each terminal's process was first seen ended (ms), for its exit bar.
    ended_at: HashMap<String, i64>,
    /// "110%" for a moment after the zoom changed, like a browser.
    zoom_badge: Option<f64>,
    _zoom_badge_task: Option<Task<()>>,
    /// The window title last set.
    window_title: String,
    /// A pane header pressed and maybe dragged onto another pane to swap them.
    pane_drag: Option<PaneDrag>,
    /// Where the ⋯ and Git buttons were drawn, for their menus.
    more_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    git_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// The grid was dense last frame (the sidebar folds once when it gets dense).
    was_dense: bool,
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
            tray: None,
            _tray_tasks: vec![],
            _second_launch: None,
            focus: None,
            opened: false,
            remotes: HashMap::new(),
            live_runs: Default::default(),
            as_terminal: Vec::new(),
            banner_hidden: vec![],
            login_started: vec![],
            switch_asks: vec![],
            switch_declined: HashMap::new(),
            told_none_left: HashSet::new(),
            switching: HashSet::new(),
            diff_drag: None,
            utility_menu: None,
            pending_slot: None,
            updated_to: None,
            pane_menu: None,
            closing: None,
            close_focus: None,
            close_focus_pending: false,
            previews: HashMap::new(),
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
            git_menu: None,
            sidebar: Default::default(),
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
            menu_nav: Default::default(),
            plans: HashMap::new(),
            _plan_task: None,
            pids: HashMap::new(),
            ended_at: HashMap::new(),
            zoom_badge: None,
            _zoom_badge_task: None,
            window_title: String::new(),
            pane_drag: None,
            more_bounds: Rc::default(),
            git_bounds: Rc::default(),
            was_dense: false,
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
            let pids: HashMap<String, i64> =
                panes.iter().map(|p| (p.pane_id.clone(), p.pid)).collect();
            let states: HashMap<String, String> =
                panes.into_iter().map(|p| (p.pane_id, p.state)).collect();
            if this
                .update(cx, |this, cx| {
                    this.pids = pids;
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
        // Settings and Mods draw from a copy: a change made elsewhere (zoom, pins) shows there.
        if let Some(view) = self.settings_view.clone() {
            let config = self.store.get().clone();
            view.update(cx, |v, cx| v.sync(&config, cx));
        }
        if let Some(view) = self.mods_view.clone() {
            let config = self.store.get().clone();
            view.update(cx, |v, cx| v.set_config(config, cx));
        }
        self.refresh_project_view(cx);
        self.refresh_chat_later(cx);
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
                    .or_else(|| {
                        wraithgrid_core::platform::find_on_path(
                            cli.spec().bin,
                            |k| env.get(k).cloned(),
                            Os::current(),
                        )
                    })
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
        self.subscribe_chat(&view, pane_id, window, cx);
        self.chats.insert(pane.id.clone(), view.clone());
        view
    }

    /// The chat view's events, handled in this window (again after the window reopens).
    fn subscribe_chat(
        &mut self,
        view: &Entity<ChatView>,
        pane_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(
            view,
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
                            if !p.has_conversation && p.title == auto_title(p.shell, &p.cwd) {
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
                    this.refresh_project_view(cx);
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
                ChatViewEvent::Queue { text, when } => {
                    this.queue_message(&pane_id, text, *when, cx)
                }
                ChatViewEvent::CancelQueued(id) => {
                    let id = id.clone();
                    this.update(cx, |c| c.schedules.retain(|j| j.id != id));
                }
                ChatViewEvent::SendQueuedNow(id) => this.run_job_now(&id.clone(), window, cx),
                ChatViewEvent::ContinueAs(to) => {
                    let to = to.clone();
                    this.continue_as(&pane_id, &to, None, window, cx)
                }
                ChatViewEvent::SignIn(id) => this.login(id.clone(), window, cx),
            },
        )
        .detach();
    }

    /// `switchWorkspace`: the project's grid, its first pane focused, nothing zoomed or asked;
    /// screens and dialogs close.
    fn select_workspace(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(ws) = self.config().workspaces.iter().find(|w| w.id == id) else {
            return;
        };
        let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
        self.focused = pane_ids(shown_layout(ws.layout.as_ref(), &ids).as_ref())
            .into_iter()
            .next();
        self.side_show_grid();
        self.zoomed = None;
        self.closing = None;
        self.modal = None;
        self.pending_slot = None;
        self.close_menus(cx);
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
                if let NewPaneEvent::PinFolders(pins) = event {
                    let pins = pins.clone();
                    this.update(cx, |c| c.settings.pinned_folders = pins);
                    return;
                }
                if let NewPaneEvent::Start {
                    account,
                    cwd,
                    args,
                    worktree_panes,
                } = event
                {
                    if let Some(on) = worktree_panes {
                        this.remember_worktree_panes(*on, cx);
                    }
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
        cx.subscribe_in(&dialog, window, |this, _, e: &OpenAccounts, window, cx| {
            this.open_accounts_for(e.import, window, cx)
        })
        .detach();
        self.modal = Some(Modal::NewPane(dialog));
        cx.notify();
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.config().clone();
        let palette = cx.new(|cx| Palette::new(&config, window, cx));
        self.add_git_commands(&palette, cx);
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
                        PaletteAction::Preview(id) => this.open_preview(&id, window, cx),
                        PaletteAction::NewPane => return this.open_new_pane(window, cx),
                        PaletteAction::NewTerminal => this.new_pane(None, cx),
                        PaletteAction::ToggleZoom => this.toggle_zoom(None, cx),
                        PaletteAction::Settings => return this.open_settings(window, cx),
                        PaletteAction::Accounts => return this.open_accounts(window, cx),
                        PaletteAction::Changes => this.toggle_changes(window, cx),
                        PaletteAction::Usage => return this.open_usage(window, cx),
                        PaletteAction::Mods => return this.open_mods(window, cx),
                        PaletteAction::Scheduled => return this.open_scheduled(window, cx),
                        a @ (PaletteAction::SwitchBranch
                        | PaletteAction::BranchPane
                        | PaletteAction::OpenBranch) => return this.palette_git(a, window, cx),
                        PaletteAction::Shortcuts => return this.toggle_shortcuts(window, cx),
                        PaletteAction::ClosePane => {
                            if let Some(id) = this.focused.clone() {
                                this.request_close_pane(id, cx);
                            }
                        }
                    }
                }
                this.close_modal(window, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &palette,
            window,
            |this, _, command: &PaletteCommand, window, cx| {
                this.modal = None;
                match command.clone() {
                    PaletteCommand::NewProject => return this.open_new_project(window, cx),
                    PaletteCommand::Team { workspace, start } => {
                        if start {
                            this.start_team(workspace, None, false, window, cx)
                        } else {
                            this.stop_team(workspace, false, window, cx)
                        }
                        // It may ask first.
                        if this.modal.is_some() {
                            return;
                        }
                    }
                    PaletteCommand::NewScheduledJob => {
                        this.open_scheduled(window, cx);
                        if let Some(view) = this.scheduled_view.clone() {
                            view.update(cx, |v, cx| v.open_job(None, window, cx));
                        }
                        return cx.notify();
                    }
                    PaletteCommand::ToggleSidebar => this.update(cx, |c| {
                        c.settings.sidebar_collapsed = !c.settings.sidebar_collapsed
                    }),
                    PaletteCommand::SetTheme { dark } => {
                        this.update(cx, |c| {
                            c.settings.theme = if dark {
                                wraithgrid_core::types::ThemePreference::Dark
                            } else {
                                wraithgrid_core::types::ThemePreference::Light
                            }
                        });
                        let resolved =
                            crate::theme::resolve(this.config(), crate::theme::system_dark(window));
                        crate::theme::apply(&resolved, Some(window), cx);
                    }
                    PaletteCommand::NewPaneAs { account, cwd } => {
                        let id = this.add_pane(None, Some(account), cwd, vec![], false, true, cx);
                        this.focused = Some(id);
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
        // `WRAITHGRID_ATTACH=<path>[:<path>…]` (debug builds) attaches files to the first chat's
        // box as a drop does: images as images, the rest as paths.
        if let Some(paths) = std::env::var("WRAITHGRID_ATTACH")
            .ok()
            .filter(|_| cfg!(debug_assertions))
        {
            let paths: Vec<std::path::PathBuf> = std::env::split_paths(&paths).collect();
            cx.on_next_frame(window, move |this, _, cx| {
                if let Some(chat) = this.chats.values().next().cloned() {
                    chat.update(cx, |c, cx| c.attach_dropped(&paths, cx));
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
            Ok(what @ ("theme-editor" | "themes")) => {
                self.open_settings(window, cx);
                if let Some(view) = self.settings_view.clone() {
                    let editor = what == "theme-editor";
                    view.update(cx, |v, cx| v.open_theme_tool(editor, window, cx));
                }
            }
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
            Ok("shortcuts") => self.toggle_shortcuts(window, cx),
            Ok(tab @ ("about" | "keyboard")) => {
                self.open_settings(window, cx);
                let label = if tab == "about" { "About" } else { "Keyboard" };
                if let Some(view) = self.settings_view.clone() {
                    view.update(cx, |s, cx| s.show_tab(label, cx));
                }
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
            Ok(hook) if hook.starts_with("git-") => self.open_git_from_env(hook, window, cx),
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
                SettingsEvent::Changed(patch) => {
                    // Field-level: only what Settings changed, on the live config.
                    let s0 = this.config().settings.clone();
                    this.update(cx, |c| patch(c));
                    let config = this.config().clone();
                    let s = &config.settings;
                    this.host.set_cli_access(s.cli_access);
                    let look = |s: &wraithgrid_core::config::Settings| {
                        (
                            s.theme,
                            s.theme_dark.clone(),
                            s.theme_light.clone(),
                            s.accent,
                            s.terminal_palette,
                        )
                    };
                    if look(&s0) != look(s) {
                        let resolved =
                            crate::theme::resolve(&config, crate::theme::system_dark(window));
                        crate::theme::apply(&resolved, Some(window), cx);
                    }
                    if let Some(view) = this.settings_view.clone() {
                        view.update(cx, |v, cx| v.sync(&config, cx));
                    }
                }
                SettingsEvent::RestartClaudePanes => {
                    let n = this.restart_agent_panes(cx);
                    if let Some(view) = this.settings_view.clone() {
                        view.update(cx, |v, cx| v.set_restarted(n, cx));
                    }
                }
                SettingsEvent::Machines(e) => {
                    match e {
                        crate::machines_row::MachinesEvent::Add(m) => {
                            let m = m.clone();
                            this.update(cx, |c| c.machines.push(m));
                        }
                        // Its projects run here again; its panes there keep running.
                        crate::machines_row::MachinesEvent::Remove(id) => {
                            let id = id.clone();
                            this.remotes.remove(&id);
                            this.update(cx, |c| {
                                c.machines.retain(|m| m.id != id);
                                for w in c.workspaces.iter_mut() {
                                    if w.machine_id.as_deref() == Some(id.as_str()) {
                                        w.machine_id = None;
                                    }
                                }
                            });
                        }
                    }
                    let machines = this.config().machines.clone();
                    if let Some(view) = this.settings_view.clone() {
                        view.update(cx, |v, cx| v.set_machines(machines, cx));
                    }
                }
                SettingsEvent::Close => {
                    this.settings_view = None;
                    this.close_modal(window, cx);
                }
            },
        )
        .detach();
        let on_launch = config.settings.check_updates_on_launch;
        crate::about_view::attach(&view, cx.entity().downgrade(), on_launch, cx);
        window.focus(&view.focus_handle(cx), cx);
        self.settings_view = Some(view);
        cx.notify();
    }

    /// The shortcuts sheet; its own shortcut closes it again.
    fn toggle_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Some(Modal::Shortcuts(_))) {
            return self.close_modal(window, cx);
        }
        let sheet = cx.new(|cx| ShortcutsSheet::new(&self.config().settings.shortcuts, cx));
        cx.subscribe_in(
            &sheet,
            window,
            |this, _, _: &ShortcutsSheetEvent, window, cx| this.close_modal(window, cx),
        )
        .detach();
        window.focus(&sheet.focus_handle(cx), cx);
        self.modal = Some(Modal::Shortcuts(sheet));
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
                NewProjectEvent::PinFolders(pins) => {
                    let pins = pins.clone();
                    this.update(cx, |c| c.settings.pinned_folders = pins);
                }
                NewProjectEvent::Cancel => this.close_modal(window, cx),
            },
        )
        .detach();
        cx.subscribe_in(
            &dialog,
            window,
            |this, dialog, e: &CreateRemoteProject, window, cx| {
                // Made here, its machine set, then its first chat opens there.
                match this.create_project(&e.name, &e.folder, e.account.clone(), false, cx) {
                    Ok(()) => {
                        let (id, machine) =
                            (this.config().active_workspace.clone(), e.machine.clone());
                        this.update(cx, |c| {
                            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == id) {
                                w.machine_id = Some(machine);
                            }
                        });
                        if let (true, Some(account)) = (e.start_chat, e.account.clone()) {
                            let cwd = e.folder.trim().to_string();
                            this.add_pane(None, Some(account), cwd, vec![], false, true, cx);
                        }
                        this.close_modal(window, cx);
                    }
                    Err(err) => dialog.update(cx, |d, cx| {
                        d.error = Some(err);
                        cx.notify();
                    }),
                }
            },
        )
        .detach();
        cx.subscribe_in(&dialog, window, |this, _, e: &OpenAccounts, window, cx| {
            this.open_accounts_for(e.import, window, cx)
        })
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
        self.drop_settings(cx);
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
                    AccountsEvent::Remove { id, delete_dir } => {
                        // "Also delete its config dir": only an account's dir inside home.
                        let doomed = match next.account(id).filter(|_| *delete_dir) {
                            None => None,
                            Some(a) => {
                                let cli = a.cli.as_str().to_string();
                                match wraithgrid_core::accounts::deletable_config_dir(
                                    &next,
                                    &a.config_dir,
                                    &home,
                                ) {
                                    Ok(real) => real.map(|r| (r, cli)),
                                    Err(e) => {
                                        return view.update(cx, |v, cx| {
                                            v.error = Some(e);
                                            cx.notify();
                                        })
                                    }
                                }
                            }
                        };
                        // Panes first: a running agent must not keep writing into its account.
                        for pane in wraithgrid_core::accounts::remove_account(&mut next, id) {
                            this.close_pane(pane.clone(), cx);
                            for w in &mut next.workspaces {
                                w.panes.retain(|p| p.id != pane);
                                w.layout = remove_pane(w.layout.as_ref(), &pane);
                            }
                        }
                        let Some((real, cli)) = doomed else {
                            this.update(cx, |c| *c = next);
                            let config = this.config().clone();
                            return view.update(cx, |v, cx| v.set_config(&config, window, cx));
                        };
                        // The shared links out first, so the delete can never reach through
                        // them; then the folder, off the main thread; the account goes once
                        // it is gone.
                        wraithgrid_mods::shared_config::SharedBrains::new(home.clone())
                            .unshare(&cli, &real.to_string_lossy());
                        let task = cx.background_spawn(async move {
                            std::fs::remove_dir_all(&real).map_err(|e| e.to_string())
                        });
                        let view = view.clone();
                        cx.spawn_in(window, async move |this, cx| {
                            let res = task.await;
                            let _ = this.update_in(cx, |this, window, cx| match res {
                                Ok(()) => {
                                    this.update(cx, |c| *c = next);
                                    let config = this.config().clone();
                                    view.update(cx, |v, cx| v.set_config(&config, window, cx));
                                }
                                Err(e) => view.update(cx, |v, cx| {
                                    v.error = Some(format!("Could not delete the config dir: {e}"));
                                    cx.notify();
                                }),
                            });
                        })
                        .detach();
                        return;
                    }
                    AccountsEvent::Import { dir } => {
                        wraithgrid_core::accounts::import_account(&mut next, dir).map(|_| ())
                    }
                    AccountsEvent::MarkSignedIn { id } => {
                        next.accounts
                            .iter_mut()
                            .filter(|a| &a.id == id)
                            .for_each(|a| a.signed_in = true);
                        Ok(())
                    }
                    AccountsEvent::Folder(op) => {
                        use crate::accounts_view::FolderOp;
                        use wraithgrid_core::accounts as acc;
                        match op {
                            FolderOp::Create(name) => {
                                acc::create_folder(&mut next, name);
                            }
                            FolderOp::Rename(id, name) => acc::rename_folder(&mut next, id, name),
                            FolderOp::Toggle(id) => acc::toggle_folder(&mut next, id),
                            FolderOp::Delete(id) => acc::delete_folder(&mut next, id),
                            FolderOp::Move(id, to) => {
                                acc::move_account_to_folder(&mut next, id, to.as_deref())
                            }
                        }
                        this.update(cx, |c| *c = next.clone());
                        // In place: a folder toggle must not reset the page.
                        return view.update(cx, |v, cx| v.set_folders(&next, cx));
                    }
                    AccountsEvent::Login { id } => {
                        this.accounts_view = None;
                        return this.login(id.clone(), window, cx);
                    }
                    AccountsEvent::Close => {
                        this.accounts_view = None;
                        return this.close_modal(window, cx);
                    }
                };
                match result {
                    Ok(()) => {
                        this.update(cx, |c| *c = next);
                        // Shown in place: a rename keeps the page, its fields and focus.
                        let config = this.config().clone();
                        view.update(cx, |v, cx| v.set_config(&config, window, cx));
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
        cx.subscribe_in(&dialog, window, |this, _, e: &PinFolders, _, cx| {
            let pins = e.0.clone();
            this.update(cx, |c| c.settings.pinned_folders = pins);
        })
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
        self.probe_plans(cx);
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
                    this.refresh_chat_later(cx);
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
                    this.refresh_chat_later(cx);
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

    /// Each signed-in account's plan ("pro", "max", "ChatGPT"), from its CLI's own status
    /// command (the one sign-in ends with), for the account switcher (`accountInfo`'s plan).
    fn probe_plans(&mut self, cx: &mut Context<Self>) {
        let c = self.config().clone();
        if !c.accounts.iter().any(|a| a.signed_in) {
            return;
        }
        self._plan_task = Some(cx.spawn(async move |this, cx| {
            let plans = cx
                .background_spawn(async move {
                    // The login shell's PATH, so the CLIs are found (waits for it once).
                    let env = crate::runtime::spawn_env();
                    let home = home_dir();
                    c.accounts
                        .iter()
                        .filter(|a| a.signed_in)
                        .filter_map(|a| {
                            let flow = wraithgrid_core::login::login_flow(a.cli)?;
                            let program = if a.cli == AgentCli::Claude {
                                crate::mods_glue::claude_binary(&c, &env)
                            } else {
                                c.settings
                                    .cli_paths
                                    .iter()
                                    .find(|(cli, p)| *cli == a.cli && !p.trim().is_empty())
                                    .map(|(_, p)| {
                                        wraithgrid_core::paths::resolve_user_path(p.trim(), &home)
                                    })
                                    .or_else(|| {
                                        wraithgrid_core::platform::find_on_path(
                                            a.cli.spec().bin,
                                            |k| env.get(k).cloned(),
                                            Os::current(),
                                        )
                                    })
                            }?;
                            let pane_env = wraithgrid_core::pane_env::build_pane_env(
                                wraithgrid_core::pane_env::PaneEnvInput {
                                    base: &env,
                                    shell: false,
                                    cli: a.cli,
                                    config_dir: Some(a.config_dir.as_str()),
                                    home: &home,
                                    mod_dirs: &c.settings.mod_dirs,
                                    os: Os::current(),
                                },
                            );
                            let o = std::process::Command::new(program)
                                .args(flow.status)
                                .current_dir(&home)
                                .env_clear()
                                .envs(&pane_env)
                                .stdin(std::process::Stdio::null())
                                .output()
                                .ok()?;
                            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
                            if flow.status_stderr {
                                text.push_str(&String::from_utf8_lossy(&o.stderr));
                            }
                            let status = flow.parse_status(&text);
                            status
                                .plan
                                .filter(|_| status.logged_in)
                                .map(|p| (a.id.clone(), p))
                        })
                        .collect::<HashMap<_, _>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.plans.extend(plans);
                cx.notify();
            });
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
        self.drop_settings(cx);
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
        self.drop_settings(cx);
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
        cx.subscribe_in(
            &view,
            window,
            |this, view, e: &ModsRequest, window, cx| match e {
                ModsRequest::Config(patch) => this.update(cx, |c| patch(c)),
                ModsRequest::RestartClaudePanes => {
                    let n = this.restart_agent_panes(cx);
                    view.update(cx, |v, cx| v.set_restarted(n, cx));
                }
                ModsRequest::AddAccount => this.open_accounts_for(false, window, cx),
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
        self.drop_settings(cx);
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
                        this.reveal_chat(ws, pane, window, cx);
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

    /// A pane's status as Electron's `PaneStatus` names it (`deriveStatus`): a terminal shown
    /// but not in the host yet is starting; one whose account has not signed in needs login.
    fn pane_status(&self, pane_id: &str) -> Option<&'static str> {
        use wraithgrid_chat::ChatStatus;
        if let Some(s) = self.chat_states.get(pane_id) {
            return Some(match s {
                ChatStatus::Running => "running",
                ChatStatus::Background => "background",
                ChatStatus::Approval => "approval",
                ChatStatus::Idle => "idle",
            });
        }
        let ended = self
            .terminals
            .get(pane_id)
            .is_some_and(|_| self.ended_at.contains_key(pane_id));
        let host = self.states.get(pane_id).map(String::as_str);
        if ended || host == Some("exited") {
            return Some("exited");
        }
        if self.login_needed(pane_id) {
            return Some("login");
        }
        match host {
            Some("running") => Some("running"),
            Some("blocked") => Some("approval"),
            Some(_) => Some("idle"),
            None if self.terminals.contains_key(pane_id) => Some("starting"),
            None => None,
        }
    }

    /// An agent's terminal whose account has not signed in yet (`loginNeeded`).
    fn login_needed(&self, pane_id: &str) -> bool {
        let c = self.config();
        c.workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .find(|p| p.id == pane_id)
            .filter(|p| {
                !p.shell
                    && p.view != PaneView::Preview
                    && (p.view != PaneView::Chat || self.as_terminal.contains(&p.id))
            })
            .and_then(|p| p.account_id.as_deref())
            .and_then(|id| c.account(id))
            .is_some_and(|a| !a.signed_in)
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
        self.drop_settings(cx);
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
                    this.reveal_chat(&ws, pane, window, cx);
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
        self.wire_team(&view, window, cx);
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
                    this.refresh_chat_later(cx);
                    cx.notify();
                }
            });
            cx.background_executor().timer(Duration::from_secs(5)).await;
        })
    }

    /// Asks GitHub for a newer release once at startup (Settings → check on launch).
    pub fn check_updates_on_launch(&mut self, cx: &mut Context<Self>) {
        if self.blocked.is_none() && self.config().settings.check_updates_on_launch {
            self.check_updates(true, cx);
        }
    }

    /// `launch`: a newer release is also announced through the OS, once per version.
    pub(crate) fn check_updates(&mut self, launch: bool, cx: &mut Context<Self>) {
        self.update = UpdateState::Checking;
        self._update_task = Some(cx.spawn(async move |this, cx| {
            let (result, notice) = cx
                .background_spawn(async move {
                    let ua = format!("Wraithgrid/{}", crate::runtime::APP_VERSION);
                    let result =
                        update::check(crate::runtime::APP_VERSION, &|url| update::fetch(url, &ua));
                    let notice = crate::about_view::after_check(&result, launch);
                    (result, notice)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.update = match result {
                    update::CheckResult::Available { latest, .. } => UpdateState::Available(latest),
                    update::CheckResult::Current { .. } => {
                        UpdateState::Current(wraithgrid_usage::now_ms())
                    }
                    update::CheckResult::Error { error, .. } => UpdateState::Error(error),
                };
                if let Some((title, body)) = notice {
                    cx.show_system_notification(SystemNotification {
                        tag: "update".into(),
                        title: title.into(),
                        body: body.into(),
                        actions: vec![],
                    });
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The title bar's update button: download and verify, then restart into it. Builds that
    /// can't update themselves (macOS, from source) open the release page instead.
    pub(crate) fn run_update(&mut self, cx: &mut Context<Self>) {
        let exe = std::env::current_exe().unwrap_or_default();
        let kind = update::package_kind(&exe, |k| std::env::var(k).ok());
        match std::mem::replace(&mut self.update, UpdateState::Idle) {
            UpdateState::Available(release) | UpdateState::Failed { release, .. }
                if kind == update::PackageKind::Manual =>
            {
                cx.open_url(&release.url);
                self.update = UpdateState::Available(release);
            }
            UpdateState::Available(release) | UpdateState::Failed { release, .. } => {
                let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.update = UpdateState::Downloading {
                    version: release.version.clone(),
                    percent: 0,
                    cancel: cancel.clone(),
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
                            &|| cancel.load(std::sync::atomic::Ordering::Relaxed),
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
                            Err(e) if e == "cancelled" => UpdateState::Available(release),
                            Err(error) => UpdateState::Failed { release, error },
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
                        Err(error) => {
                            this.update = UpdateState::Failed { release, error };
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
            UpdateState::Downloading {
                version, percent, ..
            } => Some((format!("Downloading v{version}… {percent}%"), false)),
            UpdateState::Ready { release, .. } => {
                Some((format!("Restart to update to v{}", release.version), true))
            }
            UpdateState::Installing(v) => Some((format!("Installing v{v}…"), false)),
            UpdateState::Error(e) | UpdateState::Failed { error: e, .. } => {
                Some((format!("Update failed: {e}"), false))
            }
            UpdateState::Idle | UpdateState::Checking | UpdateState::Current(_) => None,
        }
    }

    /// Where the updater is, for About.
    pub(crate) fn update_state(&self) -> &UpdateState {
        &self.update
    }

    /// Stops a download; the release stays available.
    pub(crate) fn cancel_update(&self) {
        if let UpdateState::Downloading { cancel, .. } = &self.update {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
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
        self.refresh_chat_later(cx);
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
        // This CLI's accounts first (the whole conversation moves), then the others (a new
        // chat there starts from the visible text, #78; a CLI without chat gets a terminal).
        let mut others: Vec<&wraithgrid_core::config::Account> =
            c.accounts.iter().filter(|a| a.id != from.id).collect();
        others.sort_by_key(|a| a.cli != from.cli);
        let choices: Vec<(String, String, String)> = others
            .into_iter()
            .map(|a| {
                let mut detail = match (a.signed_in, self.limits_label(&a.id)) {
                    (false, _) => "not signed in".to_string(),
                    (true, Some((l, _))) => l,
                    (true, None) => String::new(),
                };
                if a.cli != from.cli {
                    let how = if a.cli.spec().chat {
                        format!("new {} chat", a.cli.spec().provider_label)
                    } else {
                        "terminal".to_string()
                    };
                    detail = if detail.is_empty() {
                        how
                    } else {
                        format!("{how} · {detail}")
                    };
                }
                (a.id.clone(), a.name.clone(), detail)
            })
            .collect();
        let title = "Continue as another account".to_string();
        let message = if choices.is_empty() {
            "No other account yet.".to_string()
        } else {
            format!(
                "A {} account takes the whole conversation; another CLI starts a new chat from what it shows.",
                from.cli.spec().provider_label
            )
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
        // Another CLI can't take the conversation itself: a handoff (#78).
        let other_cli = {
            let c = self.config();
            let from = c
                .workspaces
                .iter()
                .flat_map(|w| &w.panes)
                .find(|p| p.id == pane_id)
                .and_then(|p| p.account_id.as_deref())
                .and_then(|id| c.account(id))
                .map(|a| a.cli);
            let to = c.account(to_id).map(|a| a.cli);
            matches!((from, to), (Some(f), Some(t)) if f != t)
        };
        if other_cli {
            return self.continue_in_cli(pane_id, to_id, window, cx);
        }
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
        self.host_for(&pane_id).kill(&pane_id);
        self.router.unregister(&pane_id);
        self.terminals.remove(&pane_id);
        self.ended_at.remove(&pane_id);
        self.login_started.retain(|p| p != &pane_id);
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
                    CloseMode::Keep => this.keep_in_tray(window, cx),
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
                                    if keep {
                                        this.keep_in_tray(window, cx);
                                    } else {
                                        this.quit(false, cx);
                                    }
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
        self.tray = None;
        self._tray_tasks.clear();
        // Dev servers belong to their preview panes: they stop with the app (`devServers`).
        for v in self.previews.drain().map(|(_, v)| v) {
            v.update(cx, |v, _| v.stop_server());
        }
        self.flush();
        self.flush_chats(cx);
        let host = self.host.clone();
        // A machine's agents keep running there either way.
        let remotes: Vec<_> = self.remotes.drain().map(|(_, (_, r))| r).collect();
        cx.spawn(async move |_, cx| {
            cx.background_spawn(async move {
                if keep {
                    host.detach();
                } else {
                    host.stop_all();
                }
                for r in remotes {
                    r.detach();
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
        self.pending_slot = None;
        self.show_switch_ask(window, cx);
        if self.modal.is_some() {
            return;
        }
        if let Some(pane_id) = self.focused.clone() {
            // A new pane has no view until it is drawn: focus it on the next frame.
            cx.on_next_frame(window, move |this, window, cx| {
                this.focus_pane_view(&pane_id, window, cx)
            });
        }
        cx.notify();
    }

    /// Gives the keyboard to a pane: its terminal, or its chat's message box.
    fn focus_pane_view(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.terminals.get(pane_id) {
            window.focus(&view.focus_handle(cx), cx);
        } else if let Some(chat) = self.chats.get(pane_id) {
            let handle = chat.read(cx).composer_focus(cx);
            window.focus(&handle, cx);
        }
    }

    /// `focusPane`: the pane is focused, and its view takes the keyboard.
    fn focus_pane(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.focused.as_deref() != Some(pane_id) {
            self.focused = Some(pane_id.to_string());
            cx.notify();
        }
        self.focus_pane_view(pane_id, window, cx);
    }

    /// Settings goes away: a field still waiting for its save is saved first (an event from
    /// a page being dropped may never arrive).
    fn drop_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.settings_view.take() {
            for patch in view.update(cx, |v, _| v.take_pending()) {
                self.update(cx, |c| patch(c));
            }
        }
    }

    /// "Restart claude panes" (Settings, Mods): agent panes that started; terminals restart,
    /// idle chats start their CLI again with the next message (`restartClaudePanes`).
    fn restart_agent_panes(&mut self, cx: &mut Context<Self>) -> usize {
        let agents: Vec<String> = self
            .config()
            .workspaces
            .iter()
            .flat_map(|w| w.panes.iter())
            .filter(|p| !p.shell)
            .map(|p| p.id.clone())
            .collect();
        let mut n = 0;
        for id in agents {
            if self.terminals.contains_key(&id) {
                self.restart_pane(id, cx);
                n += 1;
            } else if let Some(chat) = self.chats.get(&id).cloned() {
                if !chat.read(cx).is_busy() {
                    chat.update(cx, |c, _| c.stop_session());
                    n += 1;
                }
            }
        }
        n
    }

    /// Opened from "No accounts yet" or Mods: Accounts with its import or add form ready.
    fn open_accounts_for(&mut self, import: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        self.open_accounts(window, cx);
        if let Some(view) = self.accounts_view.clone() {
            view.update(cx, |v, cx| v.start(import, window, cx));
        }
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
        // Chats run on this computer only: a machine's project gets terminals (H5).
        let remote = self
            .config()
            .workspaces
            .iter()
            .find(|w| Some(&w.id) == ws.as_ref().or(Some(&self.config().active_workspace)))
            .is_some_and(|w| w.machine_id.is_some());
        let view = if !shell
            && chat
            && !remote
            && acct.is_some_and(|a| a.cli.spec().chat && a.signed_in)
        {
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
        // From an empty slot's "New pane here": that slot.
        let slot = self.pending_slot.take();
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
                    slot.as_deref(),
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

    /// Signs an account in: claude and codex in a dialog over pipes (`LoginDialog`), the rest
    /// in their own screen, a terminal running the CLI.
    fn login(&mut self, account_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let c = self.config().clone();
        let Some(account) = c.account(&account_id).cloned() else {
            return self.close_modal(window, cx);
        };
        let Some(flow) = wraithgrid_core::login::login_flow(account.cli) else {
            return self.login_in_terminal(account_id, window, cx);
        };
        let env = crate::runtime::spawn_env();
        let home = home_dir();
        let program = if account.cli == AgentCli::Claude {
            crate::mods_glue::claude_binary(&c, &env)
        } else {
            c.settings
                .cli_paths
                .iter()
                .find(|(cli, p)| *cli == account.cli && !p.trim().is_empty())
                .map(|(_, p)| wraithgrid_core::paths::resolve_user_path(p.trim(), &home))
                .or_else(|| {
                    wraithgrid_core::platform::find_on_path(
                        account.cli.spec().bin,
                        |k| env.get(k).cloned(),
                        Os::current(),
                    )
                })
        };
        let Some(program) = program else {
            return self.login_in_terminal(account_id, window, cx);
        };
        let pane_env =
            wraithgrid_core::pane_env::build_pane_env(wraithgrid_core::pane_env::PaneEnvInput {
                base: &env,
                shell: false,
                cli: account.cli,
                config_dir: Some(account.config_dir.as_str()),
                home: &home,
                mod_dirs: &c.settings.mod_dirs,
                os: Os::current(),
            });
        let codex = account.cli == AgentCli::Codex;
        let name = account.name.clone();
        let dialog = cx.new(|cx| {
            crate::login_dialog::LoginDialog::new(
                flow, program, pane_env, home, name, codex, window, cx,
            )
        });
        cx.subscribe_in(
            &dialog,
            window,
            move |this, _, e: &crate::login_dialog::LoginEvent, window, cx| {
                use crate::login_dialog::LoginEvent;
                match e {
                    LoginEvent::SignedIn => {
                        let id = account_id.clone();
                        this.update(cx, |c| {
                            if let Some(a) = c.accounts.iter_mut().find(|a| a.id == id) {
                                a.signed_in = true;
                            }
                        });
                        this.probe_plans(cx);
                    }
                    LoginEvent::UseTerminal => {
                        this.login_in_terminal(account_id.clone(), window, cx)
                    }
                    LoginEvent::Close => this.close_modal(window, cx),
                }
            },
        )
        .detach();
        self.modal = Some(Modal::Login(dialog));
        cx.notify();
    }

    /// Signing in in the CLI's own screen: a terminal running it.
    fn login_in_terminal(
        &mut self,
        account_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_pane(None, Some(account_id), "~".into(), vec![], false, false, cx);
        self.close_modal(window, cx);
    }

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
        let closed = self.closing_pane(&pane_id);
        self.drop_pane(&pane_id, archive, cx);
        // The last pane on a Wraithgrid worktree: offer to remove its folder.
        self.offer_remove_worktree(closed, cx);
    }

    /// `dropPane`: ends the pane's process (terminal or chat CLI) and removes it; `archive`
    /// keeps it, with its saved chat view, at the top of the project's History.
    fn drop_pane(&mut self, pane_id: &str, archive: bool, cx: &mut Context<Self>) {
        self.host_for(pane_id).kill(pane_id);
        self.router.unregister(pane_id);
        self.terminals.remove(pane_id);
        // Dropping the view stops its dev server.
        self.previews.remove(pane_id);
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

    /// `revealPane`: switches to the chat's project, puts it back in the layout if it was left
    /// out, and focuses it in the grid; a closed one comes back from History.
    fn reveal_chat(
        &mut self,
        ws_id: &str,
        pane_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self
            .config()
            .workspaces
            .iter()
            .find(|w| w.panes.iter().any(|p| p.id == pane_id))
            .map(|w| w.id.clone());
        match open {
            Some(ws) => {
                if self.config().active_workspace != ws {
                    self.select_workspace(ws, cx);
                }
                let id = pane_id.to_string();
                self.update(cx, |c| {
                    let active = c.active_workspace.clone();
                    if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == active) {
                        let ids: Vec<String> = w.panes.iter().map(|p| p.id.clone()).collect();
                        let shown = shown_layout(w.layout.as_ref(), &ids);
                        if !pane_ids(shown.as_ref()).contains(&id) {
                            w.layout = Some(place_new_pane(
                                shown.as_ref(),
                                &id,
                                None,
                                None,
                                SplitDir::Row,
                            ));
                        }
                    }
                });
                self.focused = Some(pane_id.to_string());
                self.zoomed = None;
            }
            None => {
                self.reopen_chat(ws_id, pane_id, true, cx);
            }
        }
        self.side_show_grid();
        self.close_modal(window, cx);
    }

    /// The pane's terminal view, made the first time it is shown: it reattaches to the
    /// pane's process in the host, or starts it there.
    fn terminal(&mut self, pane: &Pane, cx: &mut Context<Self>) -> Entity<TerminalView> {
        if let Some(view) = self.terminals.get(&pane.id) {
            return view.clone();
        }
        // A machine's pane: its host, and the machine resolves what to run (H5).
        let remote = self.machine_of_pane(&pane.id).map(|m| self.remote_for(&m));
        let host = remote.as_ref().map_or(self.host.clone(), |r| r.pane_host());
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
                    if let Some(remote) = remote {
                        if let Err(error) = remote.spawn(&spec) {
                            core.end(error);
                        }
                        return;
                    }
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

    /// Where the window's frame starts under its top strip, in window px.
    pub(super) fn frame_top(&self, _window: &Window) -> f32 {
        let zoom = self.config().settings.ui_zoom as f32;
        if self.tall_strip() {
            TITLE_BAR * zoom
        } else {
            PANEL_GAP * zoom
        }
    }

    /// macOS and Windows draw their window buttons in a strip of their own (`TitleBar`);
    /// elsewhere the strip is a thin gap and our buttons end the top row (#89).
    fn tall_strip(&self) -> bool {
        matches!(self.chrome, Chrome::Mac | Chrome::Overlay)
    }

    /// Which window buttons to draw: close, and minimize/maximize.
    fn controls_shown(&self, window: &Window) -> (bool, bool) {
        let tiled_edge = match window.window_decorations() {
            Decorations::Client { tiling } => {
                tiling.top || tiling.left || tiling.right || tiling.bottom
            }
            Decorations::Server => false,
        };
        let native_frame = matches!(window.window_decorations(), Decorations::Server)
            && self.chrome == Chrome::Custom;
        match self.chrome {
            Chrome::Mac => (false, false),
            Chrome::Tiling => (true, false),
            Chrome::Overlay => (true, true),
            Chrome::Custom => (!native_frame, !native_frame && !tiled_edge),
        }
    }

    /// Empty space that moves the window; a double-click maximizes, as a title bar does. What
    /// sits on it occludes it, so its buttons never move the window (and on Windows the hit
    /// test sees them, not the drag area).
    fn drag_area(&self, el: Stateful<Div>) -> Stateful<Div> {
        let zoom = self.chrome != Chrome::Tiling;
        el.window_control_area(WindowControlArea::Drag)
            .on_mouse_down(MouseButton::Left, move |event, window, _| {
                if event.click_count == 2 {
                    if zoom {
                        window.zoom_window();
                    }
                } else {
                    window.start_window_move();
                }
            })
    }

    /// Our minimize / maximize / close at the end of the top row (`WindowControls`); none
    /// where the OS draws its own.
    pub(super) fn window_controls(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.tall_strip() {
            return None;
        }
        self.control_group(window, cx)
    }

    fn control_group(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (show_close, show_min_max) = self.controls_shown(window);
        if !show_close && !show_min_max {
            return None;
        }
        let p = sidebar::pal(cx);
        let control =
            |id: &'static str, g: Glyph, area: WindowControlArea, cx: &mut Context<Self>| {
                let close = area == WindowControlArea::Close;
                let group = SharedString::from(id);
                div()
                    .id(id)
                    .group(group.clone())
                    .a11y_button(match area {
                        WindowControlArea::Close => "Close window",
                        WindowControlArea::Max => "Maximize",
                        _ => "Minimize",
                    })
                    .occlude()
                    .relative()
                    .w(z(34.))
                    .h(z(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(z(6.))
                    .cursor_pointer()
                    .hover(move |s| {
                        if close {
                            s.bg(rgb(0xc0263e))
                        } else {
                            s.bg(p.raise)
                        }
                    })
                    .window_control_area(area)
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(match area {
                            WindowControlArea::Close => "Close window",
                            WindowControlArea::Max => "Maximize",
                            _ => "Minimize",
                        })
                        .build(window, cx)
                    })
                    .child(glyph(g, 16., p.mu, p.bg).group_hover(group.clone(), |s| {
                        if close {
                            s.opacity(0.)
                        } else {
                            s
                        }
                    }))
                    .when(close, |d| {
                        d.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(0.)
                                .group_hover(group, |s| s.opacity(1.))
                                .child(glyph(g, 16., gpui_kit::white(), p.bg)),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| match area {
                        WindowControlArea::Close => this.request_close(window, cx),
                        WindowControlArea::Max => window.zoom_window(),
                        _ => window.minimize_window(),
                    }))
            };
        Some(
            div()
                .flex_none()
                .ml(z(4.))
                .flex()
                .items_center()
                .gap(px(2.))
                .when(show_min_max, |d| {
                    d.child(control("min", Glyph::Minimize, WindowControlArea::Min, cx))
                        .child(control("max", Glyph::Maximize, WindowControlArea::Max, cx))
                })
                .when(show_close, |d| {
                    d.child(control("close", Glyph::Close, WindowControlArea::Close, cx))
                })
                .into_any_element(),
        )
    }

    /// The in-app update's button (download, then restart into it), while there is one.
    pub(super) fn update_chip(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (label, enabled) = self.update_button()?;
        let p = sidebar::pal(cx);
        Some(
            ui_btn(
                "update",
                if enabled {
                    BtnKind::Primary
                } else {
                    BtnKind::Plain
                },
                &p,
            )
            .a11y_button(label.clone())
            .occlude()
            .h(z(28.))
            .child(label)
            .on_click(cx.listener(|this, _, _, cx| this.run_update(cx)))
            .into_any_element(),
        )
    }

    /// What the main area shows, for the title: a screen's name, else the project's.
    fn place_name(&self) -> String {
        match self.sidebar_screen_title() {
            Some(t) => t.to_string(),
            None => self.config().active().name.clone(),
        }
    }

    fn sidebar_screen_title(&self) -> Option<&'static str> {
        [
            (self.settings_view.is_some(), "Settings"),
            (self.accounts_view.is_some(), "Accounts"),
            (self.mods_view.is_some(), "Mods"),
            (self.usage_view.is_some(), "Usage"),
            (self.scheduled_view.is_some(), "Scheduled"),
        ]
        .into_iter()
        .find_map(|(on, t)| on.then_some(t))
    }

    /// The window's top strip: on macOS and Windows the name with the OS's buttons
    /// (`TitleBar`), elsewhere a thin gap above the cards that moves the window.
    fn frame_strip(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.tall_strip() {
            return self
                .drag_area(div().id("title-strip"))
                .h(z(PANEL_GAP))
                .flex_none()
                .into_any_element();
        }
        let p = sidebar::pal(cx);
        let mac = self.chrome == Chrome::Mac;
        let controls = if mac {
            None
        } else {
            self.control_group(window, cx)
        };
        let update = if mac { None } else { self.update_chip(cx) };
        self.drag_area(div().id("title-bar"))
            .h(z(TITLE_BAR))
            .flex_none()
            .flex()
            .items_center()
            .gap(z(8.))
            .pl(if mac {
                DefiniteLength::from(px(80.))
            } else {
                DefiniteLength::from(z(12.))
            })
            .pr(z(6.))
            .child(logo(18.))
            .child(
                div()
                    .text_size(z(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Wraithgrid"),
            )
            .child(
                div()
                    .text_size(z(12.))
                    .text_color(p.mu)
                    .truncate()
                    .child(format!("— {}", self.place_name())),
            )
            .child(div().flex_1().h_full())
            .children(update)
            .children(controls)
            .into_any_element()
    }

    fn sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_sidebar(window, cx)
    }

    /// The pane the grid shows alone (one pane, not zoomed): its header is the top row (#89).
    pub(super) fn solo_pane(&self) -> Option<String> {
        if self.blocked.is_some() || self.side_view() != "grid" {
            return None;
        }
        let ws = self.config().active();
        if self
            .zoomed
            .as_ref()
            .is_some_and(|z| ws.panes.iter().any(|p| &p.id == z))
        {
            return None;
        }
        let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
        match shown_layout(ws.layout.as_ref(), &ids) {
            Some(LayoutNode::Pane { pane_id }) => Some(pane_id),
            _ => None,
        }
    }

    /// Six or more panes on screen: the compact density (smaller terminal text, sidebar folds).
    fn dense(&self) -> bool {
        if self.zoomed.is_some() || self.side_view() != "grid" {
            return false;
        }
        let ws = self.config().active();
        let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
        pane_ids(shown_layout(ws.layout.as_ref(), &ids).as_ref()).len() >= DENSE_PANE_COUNT
    }

    /// A rebindable shortcut as its keys ("Ctrl+Shift+Z").
    pub(super) fn hint(&self, id: ShortcutId) -> String {
        bindings(&self.config().settings.shortcuts)
            .into_iter()
            .find(|(i, _)| *i == id)
            .map(|(_, b)| sidebar::binding_hint(&b))
            .unwrap_or_default()
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
            return div()
                .size_full()
                .p(z(6.))
                .child(self.pane(&p.clone(), window, cx))
                .into_any_element();
        }
        // While a divider is dragged, its sizes live here and are saved on release.
        let layout = self
            .drag
            .as_ref()
            .and_then(|d| d.current.clone())
            .or_else(|| shown_layout(ws.layout.as_ref(), &ids));
        match layout {
            None => self.empty_project(cx),
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
            LayoutNode::Empty { slot_id } => {
                // `.slot`: a dashed card inset in its cell.
                let slot = slot_id.clone();
                let p = sidebar::pal(cx);
                let group = SharedString::from(format!("slot-{slot_id}"));
                div()
                    .size_full()
                    .p(z(8.))
                    .child(
                        div()
                            .id(SharedString::from(format!("slot-{slot_id}")))
                            .group(group.clone())
                            .a11y_button("New pane here")
                            .size_full()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(z(8.))
                            .rounded(z(6.))
                            .border_1()
                            .border_dashed()
                            .border_color(p.line2)
                            .text_size(z(13.))
                            .text_color(p.mu)
                            .cursor_pointer()
                            .hover(|s| s.border_color(p.acc).text_color(p.tx))
                            .child(glyph(Glyph::Plus, 16., p.mu, p.bg))
                            .child("New pane here")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pending_slot = Some(slot.clone());
                                this.open_new_pane(window, cx);
                            })),
                    )
                    .into_any_element()
            }
            LayoutNode::Split {
                dir,
                sizes,
                children,
            } => {
                let p = sidebar::pal(cx);
                let row = *dir == SplitDir::Row;
                let mut container = div().size_full().flex().relative();
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
                        // A 1px line; drag its wider edge to share the space between its
                        // neighbours (`.sep`).
                        let (drag_path, dir) = (path.clone(), *dir);
                        let active = self
                            .drag
                            .as_ref()
                            .is_some_and(|d| d.path == path && d.index == i - 1);
                        let handle = div()
                            .relative()
                            .flex_shrink_0()
                            .bg(if active { p.acc } else { p.line })
                            .when(row, |d| d.w(px(1.)).h_full())
                            .when(!row, |d| d.h(px(1.)).w_full())
                            .child(
                                div()
                                    .id(SharedString::from(format!("divider-{path:?}-{i}")))
                                    .role(Role::Splitter)
                                    .aria_label("Resize panes")
                                    .absolute()
                                    .when(row, |d| {
                                        d.top_0()
                                            .bottom_0()
                                            .left(px(-1.))
                                            .w(px(3.))
                                            .cursor_col_resize()
                                    })
                                    .when(!row, |d| {
                                        d.left_0()
                                            .right_0()
                                            .top(px(-1.))
                                            .h(px(3.))
                                            .cursor_row_resize()
                                    })
                                    .when(active, |d| d.bg(p.acc))
                                    .hover(|s| s.bg(p.acc))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                            this.start_drag(
                                                drag_path.clone(),
                                                i - 1,
                                                dir,
                                                e.position,
                                            );
                                            cx.stop_propagation();
                                            cx.notify();
                                        }),
                                    ),
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
            current: None,
        });
    }

    /// Sizes follow the pointer in memory; nothing is saved until the release.
    fn drag_to(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(d) = &mut self.drag else { return };
        let moved = if d.dir == SplitDir::Row {
            at.x - d.start.x
        } else {
            at.y - d.start.y
        };
        let extent = d.extent.max(1.0);
        let delta = f64::from(f32::from(moved) / extent * 100.0);
        let (a, b) = (d.start_sizes[d.index], d.start_sizes[d.index + 1]);
        // Neither neighbour gets narrower than a column (or lower than a row) can be.
        let min_px = if d.dir == SplitDir::Row {
            MIN_COL_PX
        } else {
            MIN_ROW_PX
        };
        let min = f64::from(min_px / extent * 100.0);
        let (lo, hi) = (min - a, b - min);
        let delta = if lo <= hi { delta.clamp(lo, hi) } else { 0.0 };
        let mut sizes = d.start_sizes.clone();
        sizes[d.index] = a + delta;
        sizes[d.index + 1] = b - delta;
        d.current = Some(set_sizes_at(&d.layout, &d.path, &sizes));
        cx.notify();
    }

    /// The divider was let go: its sizes are saved.
    fn finish_drag(&mut self, cx: &mut Context<Self>) {
        let Some(layout) = self.drag.take().and_then(|d| d.current) else {
            return cx.notify();
        };
        self.update(cx, |c| {
            let active = c.active_workspace.clone();
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == active) {
                w.layout = Some(layout);
            }
        });
    }

    /// A pane header pressed: a drag onto another pane starts after a few pixels.
    fn pane_drag_move(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(d) = &mut self.pane_drag else { return };
        d.at = at;
        if !d.active {
            let moved = at - d.start;
            let dist = (f32::from(moved.x).powi(2) + f32::from(moved.y).powi(2)).sqrt();
            d.active = dist >= PANE_DRAG_PX;
        }
        if d.active {
            cx.notify();
        }
    }

    /// The pane under the pointer while dragging one, other than the dragged one.
    fn pane_drop_target(&self) -> Option<String> {
        let d = self.pane_drag.as_ref().filter(|d| d.active)?;
        self.pane_bounds
            .borrow()
            .iter()
            .find(|(id, b)| **id != d.pane_id && b.contains(&d.at))
            .map(|(id, _)| id.clone())
    }

    fn pane_drag_end(&mut self, cx: &mut Context<Self>) {
        let target = self.pane_drop_target();
        if let Some(d) = self.pane_drag.take() {
            if let Some(t) = target {
                self.swap_panes(&d.pane_id, &t, cx);
            }
            cx.notify();
        }
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

    /// Esc: closes the nearest thing that is open (a menu, the drawer, the close confirm).
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.close_side_popups(cx) {
            return true;
        }
        if let Some(id) = self.closing.take() {
            self.focus_pane_view(&id, window, cx);
            cx.notify();
            return true;
        }
        false
    }

    /// App shortcuts, before the focused terminal sees the key.
    fn shortcut(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Settings → Keyboard is recording a new chord: the key is the binding.
        let recording = (self.settings_view.as_ref()).is_some_and(|v| v.read(cx).recording());
        if self.blocked.is_some() || recording {
            return;
        }
        let k = &event.keystroke;
        if k.key == "escape" && self.modal.is_none() && self.escape(window, cx) {
            window.refresh();
            return cx.stop_propagation();
        }
        // Shift+F10 or the Menu key: the focused pane's menu, under its header.
        let menu_key = (k.key == "f10" && k.modifiers.shift && !k.modifiers.control)
            || k.key == "menu"
            || k.key == "contextmenu";
        if menu_key && self.modal.is_none() && self.side_view() == "grid" {
            let in_sidebar = self.sidebar_has_focus(window, cx);
            if let (false, Some(id)) = (in_sidebar, self.focused.clone()) {
                let at = self
                    .pane_bounds
                    .borrow()
                    .get(&id)
                    .map_or(point(px(200.), px(120.)), |b| {
                        b.origin + point(px(12.), z(34.).to_pixels(window.rem_size()))
                    });
                self.open_pane_menu(id, at, true, cx);
                return cx.stop_propagation();
            }
        }
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
        // Dialogs own the keyboard, except that a sheet's own shortcut toggles it.
        let toggles = matches!(
            (&action, &self.modal),
            (
                Action::Shortcut(ShortcutId::Shortcuts),
                Some(Modal::Shortcuts(_))
            ) | (
                Action::Shortcut(ShortcutId::Palette),
                Some(Modal::Palette(_))
            )
        );
        if self.modal.is_some() && !toggles {
            return;
        }
        cx.stop_propagation();
        match action {
            Action::Shortcut(ShortcutId::NewPane) => self.open_new_pane(window, cx),
            Action::Shortcut(ShortcutId::Palette) => {
                if self.modal.is_some() {
                    self.close_modal(window, cx)
                } else {
                    self.open_palette(window, cx)
                }
            }
            Action::Shortcut(ShortcutId::ClosePane) => {
                if self.side_view() == "grid" {
                    if let Some(id) = self.focused.clone() {
                        self.request_close_pane(id, cx);
                    }
                }
            }
            Action::Shortcut(ShortcutId::ToggleZoom) => self.toggle_zoom(None, cx),
            Action::Shortcut(ShortcutId::Diff) => self.toggle_changes(window, cx),
            Action::Shortcut(ShortcutId::Shortcuts) => self.toggle_shortcuts(window, cx),
            Action::Focus(dir) => {
                if self.side_view() == "grid" {
                    self.move_focus(dir, window, cx)
                }
            }
            Action::Workspace(i) => {
                if let Some(id) = self.config().workspaces.get(i).map(|w| w.id.clone()) {
                    self.select_workspace(id, cx);
                    self.close_modal(window, cx);
                }
            }
            Action::UiZoom(delta) => {
                let next = step_zoom(self.config().settings.ui_zoom, delta);
                self.update(cx, |c| c.settings.ui_zoom = next);
                self.show_zoom_badge(next, cx);
            }
        }
    }

    /// The keyboard is in the sidebar (a project row): its own Shift+F10 opens its menu.
    fn sidebar_has_focus(&self, window: &Window, cx: &App) -> bool {
        let Some(focused) = window.focused(cx) else {
            return false;
        };
        let in_pane = self
            .terminals
            .values()
            .any(|v| v.focus_handle(cx).contains_focused(window, cx))
            || self.chats.values().any(|v| {
                v.focus_handle(cx).contains_focused(window, cx)
                    || v.read(cx).composer_focus(cx).contains_focused(window, cx)
            });
        let shell = self.focus.as_ref().is_some_and(|f| *f == focused);
        !in_pane && !shell
    }

    /// "110%" for a moment after the zoom changes (`ZoomBadge`).
    fn show_zoom_badge(&mut self, zoom: f64, cx: &mut Context<Self>) {
        self.zoom_badge = Some(zoom);
        self._zoom_badge_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.zoom_badge = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Focuses the nearest pane in a direction, by where panes were drawn.
    fn move_focus(&mut self, dir: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.pane_bounds.borrow().clone();
        let Some(from) = self.focused.as_ref().and_then(|f| bounds.get(f)).copied() else {
            // Nothing focused yet: the first pane on screen.
            let first = {
                let ws = self.config().active();
                let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
                pane_ids(shown_layout(ws.layout.as_ref(), &ids).as_ref())
                    .into_iter()
                    .next()
            };
            if let Some(id) = first {
                self.focus_pane(&id, window, cx);
            }
            return;
        };
        if self.zoomed.is_some() {
            return;
        }
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
            self.focus_pane(&id, window, cx);
        }
    }

    fn pane(&mut self, pane: &Pane, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = sidebar::pal(cx);
        let config = self.config().clone();
        let chat = pane.view == PaneView::Chat && !self.as_terminal.contains(&pane.id);
        let preview = pane.view == PaneView::Preview;
        let label = pane_label(&config, pane);
        let account = pane
            .account_id
            .as_deref()
            .and_then(|a| config.account(a))
            .cloned();
        let cli = account.as_ref().map_or(AgentCli::Claude, |a| a.cli);
        let command = if pane.shell {
            "shell".to_string()
        } else {
            cli.spec().bin.to_string()
        };
        let mut ended_text: Option<String> = None;
        // The terminal's own title (what the CLI set), for the state word's tooltip.
        let mut detail: Option<String> = None;
        let body: AnyElement = if preview {
            let view = self.preview(pane, window, cx);
            div().size_full().child(view).into_any_element()
        } else if chat {
            let view = self.chat(pane, window, cx);
            self.chat_states
                .insert(pane.id.clone(), view.read(cx).status());
            if view.focus_handle(cx).contains_focused(window, cx)
                || view
                    .read(cx)
                    .composer_focus(cx)
                    .contains_focused(window, cx)
            {
                self.focused = Some(pane.id.clone());
            }
            div().size_full().child(view).into_any_element()
        } else {
            let view = self.terminal(pane, cx);
            let ended = view.read(cx).ended();
            detail = Some(view.read(cx).title());
            if view.focus_handle(cx).contains_focused(window, cx) {
                self.focused = Some(pane.id.clone());
            }
            if ended.is_none() {
                self.ended_at.remove(&pane.id);
            }
            ended_text = ended;
            div().size_full().child(view).into_any_element()
        };
        let exited = ended_text.is_some();
        let confirm = self.close_confirm(&pane.id, &command, &pane.cwd, window, cx);
        let banner = self.login_banner(pane, chat, exited, cx);
        let exited_bar = self.exited_bar(pane, &command, ended_text.as_deref(), cx);
        let header = self.pane_header(pane, &label, detail, window, cx);
        let (bounds, key) = (self.pane_bounds.clone(), pane.id.clone());
        let drop_here = self.pane_drop_target().as_deref() == Some(pane.id.as_str());
        let dragging = self
            .pane_drag
            .as_ref()
            .is_some_and(|d| d.active && d.pane_id == pane.id);
        let focus_id = pane.id.clone();
        div()
            .id(SharedString::from(format!("pane-{}", pane.id)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(p.bg)
            .when(dragging, |d| d.opacity(0.6))
            // A click anywhere in the pane focuses it (`onMouseDownCapture`).
            .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                if this.focused.as_deref() != Some(focus_id.as_str()) {
                    this.focused = Some(focus_id.clone());
                    cx.notify();
                }
            }))
            .child(
                canvas(
                    move |b, _, _| bounds.borrow_mut().insert(key, b),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(body)
                    .children(banner)
                    .children(exited_bar)
                    .children(confirm),
            )
            // The drop target while another pane is dragged over it: an inset accent line.
            .when(drop_here, |d| {
                d.child(div().absolute().inset_0().border_2().border_color(p.acc))
            })
            .into_any_element()
    }

    /// A pane's title row (`PaneHeader`): logo, title, state word, a terminal's branch; its
    /// buttons show on hover. Alone on screen it is the app's top row too (#89).
    fn pane_header(
        &mut self,
        pane: &Pane,
        label: &str,
        detail: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = sidebar::pal(cx);
        let id = pane.id.clone();
        let solo = self.solo_pane().as_deref() == Some(id.as_str());
        let focused = self.focused.as_deref() == Some(id.as_str());
        let config = self.config().clone();
        let chat = pane.view == PaneView::Chat && !self.as_terminal.contains(&pane.id);
        let account = pane.account_id.as_deref().and_then(|a| config.account(a));
        let status = self.pane_status(&pane.id);
        let (word, tone) = if pane.view == PaneView::Preview {
            ("preview".to_string(), Tone::Quiet)
        } else {
            state_word(status)
        };
        let zoomed = self.zoomed.as_deref() == Some(id.as_str());
        let zoom_hint = self.hint(ShortcutId::ToggleZoom);
        let zoom_tip = if zoomed {
            format!("Back to grid ({zoom_hint})")
        } else {
            format!("Zoom pane ({zoom_hint})")
        };
        let compact = self
            .pane_bounds
            .borrow()
            .get(&id)
            .is_some_and(|b| f32::from(b.size.width) < 460.0);
        let group = SharedString::from(format!("ph-{id}"));
        let menu_open = self.pane_menu.as_ref().is_some_and(|(m, _)| *m == id);
        let mono = crate::terminal::mono(cx);
        let focus_hint = if cfg!(target_os = "macos") {
            "⌘⌥Arrow"
        } else {
            "Ctrl+Alt+Arrow"
        };
        let icon_btn = |bid: String, g: Glyph, tip: String| {
            div()
                .id(SharedString::from(bid))
                .a11y_button(tip.clone())
                .when(solo, |d| d.occlude())
                .flex_none()
                .size(z(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(z(6.))
                .cursor_pointer()
                .hover(|s| s.bg(p.raise))
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(small(g, p.mu, p.bg))
        };
        let word_tip = [Some(status_label(status).to_string()), detail]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let word_el = {
            let el = div()
                .id(SharedString::from(format!("ph-st-tip-{id}")))
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(word_tip.clone()).build(window, cx)
                })
                .flex_none()
                .font_family(mono)
                .text_size(z(11.))
                .text_color(match tone {
                    Tone::Working => p.ok,
                    Tone::Warn => p.warn,
                    Tone::Quiet => p.fa,
                })
                .child(word);
            if tone == Tone::Working {
                // `working` sweeps (#75): the word breathes rather than carrying a dot.
                el.with_animation(
                    SharedString::from(format!("ph-st-{id}")),
                    Animation::new(Duration::from_millis(1800))
                        .repeat()
                        .with_easing(bounce(ease_in_out)),
                    |d, t| d.opacity(1.0 - 0.45 * t),
                )
                .into_any_element()
            } else {
                el.into_any_element()
            }
        };
        let tools = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(z(if solo { 6. } else { 4. }))
            .when(!solo && !menu_open, |d| {
                d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
            })
            .when(solo, |d| {
                d.child(self.layout_presets(false, cx))
                    .child(self.usage_pill(cx))
            })
            .child(
                icon_btn(
                    format!("zoom-{id}"),
                    if zoomed { Glyph::Unzoom } else { Glyph::Zoom },
                    zoom_tip,
                )
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| this.toggle_zoom(Some(id.clone()), cx)
                })),
            )
            .child(
                icon_btn(
                    format!("more-{id}"),
                    Glyph::More,
                    if solo {
                        "Chat and app actions".into()
                    } else {
                        format!("Pane actions: {label}")
                    },
                )
                .when(menu_open, |d| d.bg(p.raise))
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, e: &ClickEvent, _, cx| {
                        if menu_open {
                            this.close_menus(cx);
                        } else {
                            this.open_pane_menu(id.clone(), e.position(), e.is_keyboard(), cx)
                        }
                    }
                })),
            );
        let ws = self.config().active().clone();
        let hidden = if solo {
            ws.panes.len().saturating_sub(1)
        } else {
            0
        };
        let lead: Vec<AnyElement> = if solo {
            vec![
                self.project_link(&ws.id, &ws.name, cx),
                div()
                    .flex_none()
                    .mx(z(-2.))
                    .text_color(p.fa)
                    .child("/")
                    .into_any_element(),
            ]
        } else {
            vec![]
        };
        let after: Vec<AnyElement> = if solo {
            let mut v: Vec<AnyElement> = self.hidden_chip(hidden, cx).into_iter().collect();
            v.extend(self.git_button(window, cx));
            v
        } else if !chat {
            self.branch_chip(pane, compact, cx).into_iter().collect()
        } else {
            vec![]
        };
        let end: Vec<AnyElement> = if solo {
            let mut v: Vec<AnyElement> = self.update_chip(cx).into_iter().collect();
            v.extend(self.window_controls(window, cx));
            v
        } else {
            vec![]
        };
        let title_tip = format!("{label} — focus pane ({focus_hint})");
        let base = div().id(SharedString::from(format!("pane-head-{id}")));
        let base = if solo { self.drag_area(base) } else { base };
        base.group(group)
            .h(z(if solo { SOLO_ROW } else { 34. }))
            .flex_none()
            .flex()
            .items_center()
            .gap(z(if solo { 10. } else { 8. }))
            .pl(z(if solo { 14. } else { 10. }))
            .pr(z(if solo { 8. } else { 4. }))
            .border_b_1()
            .border_color(p.line)
            .bg(if focused && !solo { p.side } else { p.bg })
            .text_size(z(12.))
            .text_color(p.tx)
            .on_mouse_down(MouseButton::Right, {
                let id = id.clone();
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    this.open_pane_menu(id.clone(), e.position, false, cx)
                })
            })
            // Pressed on its empty space: the pane takes the keyboard, and a drag past a few
            // pixels carries it onto another pane to swap them (not alone, not zoomed).
            .when(!solo, |d| {
                let id = id.clone();
                let title = label.to_string();
                let can_drag = self.zoomed.is_none();
                d.cursor_grab().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                        this.focus_pane(&id.clone(), window, cx);
                        if can_drag {
                            this.pane_drag = Some(PaneDrag {
                                pane_id: id.clone(),
                                title: title.clone(),
                                start: e.position,
                                at: e.position,
                                active: false,
                            });
                        }
                    }),
                )
            })
            .children(lead)
            .child(match account.filter(|_| !pane.shell) {
                Some(a) => sidebar::provider_logo(a.cli.as_str(), &a.name, 13., &p),
                None if !pane.shell => sidebar::provider_logo("claude", label, 13., &p),
                None => small(Glyph::Monitor, p.mu, p.bg).into_any_element(),
            })
            .child(
                div()
                    .id(SharedString::from(format!("ph-title-{id}")))
                    .a11y_button(title_tip.clone())
                    .when(solo, |d| d.occlude())
                    .min_w(px(0.))
                    .flex_shrink(1.)
                    .when(!solo, |d| d.max_w(relative(0.4)))
                    .truncate()
                    .text_size(z(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if focused || solo { p.tx } else { p.mu })
                    .cursor_pointer()
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(title_tip.clone())
                            .build(window, cx)
                    })
                    .child(label.to_string())
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| this.focus_pane(&id.clone(), window, cx)
                    })),
            )
            .child(word_el)
            .children(after)
            .child(div().flex_1().min_w(px(0.)).h_full())
            .child(tools)
            .children(end)
            .into_any_element()
    }
}

/// How a state word is drawn.
#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Working,
    Warn,
    Quiet,
}

/// The state as one quiet word (#75): `working` breathes, `approve` is amber; no dots.
fn state_word(status: Option<&str>) -> (String, Tone) {
    let (w, t) = match status {
        Some("running") => ("working", Tone::Working),
        Some("background") => ("background", Tone::Working),
        Some("approval") => ("approve", Tone::Warn),
        Some("login") => ("sign in", Tone::Warn),
        Some("exited") => ("exited", Tone::Warn),
        Some("starting") | None => ("starting", Tone::Quiet),
        Some("done") => ("done", Tone::Quiet),
        Some(_) => ("idle", Tone::Quiet),
    };
    (w.to_string(), t)
}

/// `PANE_STATUS_LABEL`: the state in words, for tooltips.
fn status_label(status: Option<&str>) -> &'static str {
    match status {
        Some("running") => "running",
        Some("background") => "in background",
        Some("approval") => "needs approval",
        Some("login") => "login needed",
        Some("exited") => "exited",
        Some("done") => "done",
        Some("idle") => "idle",
        _ => "starting",
    }
}

/// The app's mark (`Logo`): one filled cell and three outlined.
fn logo(size: f32) -> AnyElement {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72" viewBox="0 0 18 18"><rect x="1" y="1" width="7" height="7" rx="2" fill="#7c5cff"/><g fill="none" stroke="#8a8f98" stroke-width="1.5"><rect x="10" y="1" width="7" height="7" rx="2"/><rect x="1" y="10" width="7" height="7" rx="2"/><rect x="10" y="10" width="7" height="7" rx="2"/></g></svg>"##;
    img(icons::svg_image(svg.to_string()))
        .size(z(size))
        .flex_none()
        .into_any_element()
}

/// Records where an element was drawn (for a menu that opens under it).
fn bounds_probe(slot: Rc<RefCell<Option<Bounds<Pixels>>>>) -> impl IntoElement {
    canvas(
        move |b, _, _| {
            *slot.borrow_mut() = Some(b);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

/// `.btn` and its kinds: plain, `pri` (accent), `gh` (ghost), `dng` (danger).
#[derive(Clone, Copy, PartialEq)]
enum BtnKind {
    Plain,
    Primary,
    Ghost,
    Danger,
}

fn ui_btn(id: impl Into<ElementId>, kind: BtnKind, p: &sidebar::Pal) -> Stateful<Div> {
    let p = *p;
    let (bg, fg, line) = match kind {
        BtnKind::Plain => (p.raise, p.tx, p.line2),
        BtnKind::Primary => (p.acc_fill, p.on_acc, p.acc_fill),
        BtnKind::Ghost => (
            gpui_kit::transparent_black(),
            p.tx,
            gpui_kit::transparent_black(),
        ),
        BtnKind::Danger => (p.err, gpui_kit::white(), p.err),
    };
    div()
        .id(id)
        .occlude()
        .flex_none()
        .h(z(32.))
        .px(z(12.))
        .flex()
        .items_center()
        .justify_center()
        .gap(z(8.))
        .rounded(z(8.))
        .border_1()
        .border_color(line)
        .bg(bg)
        .text_size(z(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(fg)
        .cursor_pointer()
        .hover(move |s| match kind {
            BtnKind::Plain | BtnKind::Ghost => s.bg(p.hover),
            BtnKind::Primary => s.bg(p.acc_fill_hover),
            BtnKind::Danger => s.opacity(0.9),
        })
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = sidebar::pal(cx);
        let settings = &self.config().settings;
        let zoom = settings.ui_zoom as f32;
        window.set_rem_size(px(16.0 * zoom));
        // Six or more panes on screen: smaller terminal text, and the sidebar folds once.
        let dense = self.dense();
        let font = settings.font_size as f32;
        let style = TerminalStyle {
            family: settings.font_family.as_str().into(),
            size: if dense {
                font.min(DENSE_FONT_SIZE)
            } else {
                font
            } * zoom,
        };
        if cx.try_global::<TerminalStyle>() != Some(&style) {
            cx.set_global(style);
        }
        if dense && !self.was_dense && !self.config().settings.sidebar_collapsed {
            self.update(cx, |c| c.settings.sidebar_collapsed = true);
        }
        self.was_dense = dense;
        // `Wraithgrid — <project or screen>`.
        let title = format!("Wraithgrid — {}", self.place_name());
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
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
        let on_screen = match &self.zoomed {
            Some(z) => vec![z.clone()],
            None => shown.clone(),
        };
        self.pane_bounds
            .borrow_mut()
            .retain(|id, _| on_screen.contains(id));
        self.sync_preview_visibility(&on_screen, cx);
        let strip = self.frame_strip(window, cx);
        let top_row = self.top_row(window, cx);
        let grid = self.grid(window, cx);
        let pane_menu = self.pane_menu_layer(window, cx);
        let utility_menu = self.utility_menu_layer(window, cx);
        let status_bar = self.status_bar(cx).into_any_element();
        let sidebar = self.sidebar(window, cx).into_any_element();
        let changes = self
            .changes
            .clone()
            .filter(|_| self.side_view() == "grid")
            .map(|panel| self.changes_panel(panel, cx));
        let modal: Option<AnyElement> = self.modal.as_ref().map(|m| match m {
            Modal::NewPane(d) => d.clone().into_any_element(),
            Modal::Palette(p) => p.clone().into_any_element(),
            Modal::Quit(q) => q.clone().into_any_element(),
            Modal::NewProject(d) => d.clone().into_any_element(),
            Modal::Project(d) => d.clone().into_any_element(),
            Modal::Choice(d) => d.clone().into_any_element(),
            Modal::Branches(d) => d.clone().into_any_element(),
            Modal::RemoveWorktree(d) => d.clone().into_any_element(),
            Modal::Shortcuts(s) => s.clone().into_any_element(),
            Modal::Login(d) => d.clone().into_any_element(),
            Modal::Switch(d) => d.clone().into_any_element(),
        });
        let drag_ghost = self.pane_drag.as_ref().filter(|d| d.active).map(|d| {
            deferred(
                anchored().position(d.at + point(px(12.), px(8.))).child(
                    div()
                        .px(z(12.))
                        .py(z(4.))
                        .rounded(z(8.))
                        .border_1()
                        .border_color(p.acc)
                        .bg(p.panel)
                        .shadow_lg()
                        .text_size(z(12.))
                        .text_color(p.tx)
                        .child(d.title.clone()),
                ),
            )
            .with_priority(5)
        });
        let badge = self.zoom_badge.map(|z| {
            let size = window.viewport_size();
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .w(size.width)
                        .h(size.height)
                        .flex()
                        .flex_col()
                        .justify_end()
                        .items_center()
                        .pb(px(44.))
                        .child(
                            div()
                                .px(px(14.))
                                .py(px(6.))
                                .rounded_full()
                                .bg(p.panel)
                                .border_1()
                                .border_color(p.line2)
                                .shadow_lg()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.tx)
                                .child(format!("{}%", (z * 100.0).round() as i64)),
                        ),
                ),
            )
            .with_priority(6)
        });
        let focus = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        self.menu_focus_back(window, cx);
        if window.focused(cx).is_none() {
            window.focus(&focus, cx);
        }
        div()
            .id("shell")
            .track_focus(&focus)
            .relative()
            .capture_key_down(cx.listener(Self::shortcut))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                // Let go outside the window: the drag ends where it is.
                let held = e.pressed_button == Some(MouseButton::Left);
                if this.drag.is_some() {
                    if held {
                        this.drag_to(e.position, cx);
                    } else {
                        this.finish_drag(cx);
                    }
                }
                if this.pane_drag.is_some() {
                    if held {
                        this.pane_drag_move(e.position, cx);
                    } else {
                        this.pane_drag = None;
                        cx.notify();
                    }
                }
            }))
            .capture_any_mouse_up(cx.listener(|this, e: &MouseUpEvent, _, cx| {
                if e.button != MouseButton::Left {
                    return;
                }
                if this.drag.is_some() {
                    this.finish_drag(cx);
                }
                if this.pane_drag.is_some() {
                    this.pane_drag_end(cx);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().window_border)
            .text_color(p.tx)
            .text_sm()
            .child(strip)
            .child(
                // The sidebar, main and Changes cards on the frame, split by a gap (#88).
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .gap(z(PANEL_GAP))
                    .px(z(PANEL_GAP))
                    .pb(z(PANEL_GAP))
                    .child(sidebar)
                    .child(
                        div()
                            .id("main")
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .flex()
                            .flex_col()
                            .rounded(z(PANEL_R))
                            .overflow_hidden()
                            .bg(p.bg)
                            .children(top_row)
                            .child(div().flex_1().min_h(px(0.)).child(grid))
                            .child(status_bar),
                    )
                    .children(changes),
            )
            .children(pane_menu)
            .children(utility_menu)
            .children(drag_ghost)
            .children(badge)
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

/// The Changes panel's left edge being dragged.
#[derive(Clone)]
struct DiffResize;

impl Render for DiffResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Shell {
    /// The Changes panel at its width, with an edge to drag (`EdgeResizer`): live while
    /// dragging, saved on release, double-click resets.
    fn changes_panel(&mut self, panel: Entity<ChangesPanel>, cx: &mut Context<Self>) -> AnyElement {
        use wraithgrid_core::types::{DIFF_WIDTH_DEFAULT, DIFF_WIDTH_MAX, DIFF_WIDTH_MIN};
        let theme = cx.theme().clone();
        let width = self
            .diff_drag
            .unwrap_or(self.config().settings.diff_width as f32);
        let dragging = self.diff_drag.is_some();
        let commit = |this: &mut Shell, cx: &mut Context<Shell>| {
            if let Some(w) = this.diff_drag.take() {
                this.update(cx, |c| c.settings.diff_width = w as i64);
            }
        };
        // A card of its own on the frame (#88); its edge is the gap before it.
        div()
            .id("changes-panel")
            .relative()
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<DiffResize>, _, cx| {
                let w = f32::from(e.bounds.right() - e.event.position.x);
                this.diff_drag = Some(
                    w.clamp(DIFF_WIDTH_MIN as f32, DIFF_WIDTH_MAX as f32)
                        .round(),
                );
                cx.notify();
            }))
            .child(
                div()
                    .size_full()
                    .rounded(z(PANEL_R))
                    .overflow_hidden()
                    .bg(theme.background)
                    .child(panel),
            )
            .child(
                div()
                    .id("changes-resize")
                    .group("changes-resize")
                    .role(Role::Splitter)
                    .aria_label("Resize changes panel")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(z(-PANEL_GAP))
                    .w(z(PANEL_GAP))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .child(
                        div()
                            .absolute()
                            .top(px(18.))
                            .bottom(px(18.))
                            .left(z(PANEL_GAP / 2. - 1.))
                            .w(px(2.))
                            .rounded(px(1.))
                            .bg(theme.ring)
                            .opacity(if dragging { 0.6 } else { 0. })
                            .group_hover("changes-resize", |s| s.opacity(0.6)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &MouseDownEvent, _, cx| {
                            if e.click_count == 2 {
                                this.diff_drag = None;
                                this.update(cx, |c| c.settings.diff_width = DIFF_WIDTH_DEFAULT);
                            }
                        }),
                    )
                    .on_drag(DiffResize, |r, _, _, cx| cx.new(|_| r.clone()))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| commit(this, cx)),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| commit(this, cx)),
                    ),
            )
            .into_any_element()
    }
}

/// A CLI's mark (`ProviderLogo`), else the name's letter; for views outside the shell.
pub(crate) fn provider_mark(cli: &str, name: &str, size: f32, cx: &App) -> AnyElement {
    sidebar::provider_logo(cli, name, size, &sidebar::pal(cx))
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
    /// The layout with the sizes so far, drawn while dragging and saved on release.
    current: Option<LayoutNode>,
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

/// A pane header pressed, and once past a few pixels dragged onto another pane to swap them
/// (dnd-kit's drag in PaneGrid.tsx).
struct PaneDrag {
    pane_id: String,
    title: String,
    start: Point<Pixels>,
    at: Point<Pixels>,
    active: bool,
}

enum Modal {
    NewPane(Entity<NewPaneDialog>),
    Palette(Entity<Palette>),
    Quit(Entity<QuitDialog>),
    NewProject(Entity<NewProjectDialog>),
    Project(Entity<ProjectDialog>),
    Choice(Entity<ChoiceDialog>),
    Branches(Entity<crate::branch_picker::BranchPicker>),
    RemoveWorktree(Entity<git_button::RemoveWorktreeDialog>),
    Shortcuts(Entity<ShortcutsSheet>),
    Login(Entity<crate::login_dialog::LoginDialog>),
    Switch(Entity<auto_switch::AutoSwitchDialog>),
}

/// Where the in-app update is (`update-install.ts` phases); About shows it.
#[derive(Clone)]
pub(crate) enum UpdateState {
    Idle,
    Checking,
    /// Up to date, checked at this time (ms).
    Current(i64),
    Available(update::ReleaseInfo),
    Downloading {
        version: String,
        percent: u8,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    },
    Ready {
        release: update::ReleaseInfo,
        file: std::path::PathBuf,
        kind: update::PackageKind,
    },
    Installing(String),
    /// The check failed.
    Error(String),
    /// Downloading or installing failed; Retry starts over.
    Failed {
        release: update::ReleaseInfo,
        error: String,
    },
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
