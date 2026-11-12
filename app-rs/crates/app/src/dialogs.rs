//! The window's overlays: New pane (`NewPaneDialog.tsx`: Account → Folder → Start, launch args
//! under More options) and the command palette (`CommandPalette.tsx`: search panes, projects and
//! commands). Each emits what was chosen; the window carries it out.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::args::{format_args, parse_args};
use wraithgrid_core::config::Config;
use wraithgrid_core::paths::{contract_home, home_dir, resolve_user_path};

// ---- New pane ------------------------------------------------------------------------------

pub enum NewPaneEvent {
    Start {
        account: Option<String>,
        cwd: String,
        args: Vec<String>,
    },
    Cancel,
}

struct AccountChoice {
    id: String,
    name: String,
    cli: AgentCli,
    config_dir: String,
    color: Hsla,
}

pub struct NewPaneDialog {
    focus: FocusHandle,
    accounts: Vec<AccountChoice>,
    /// None: a plain shell.
    account: Option<String>,
    folder: Entity<InputState>,
    args: Entity<InputState>,
    recent: Vec<String>,
    more: bool,
    error: Option<String>,
    shell_name: String,
    _subscriptions: Vec<Subscription>,
}

impl NewPaneDialog {
    pub fn new(config: &Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = config.active();
        let s = &config.settings;
        // The project's own defaults come first, then the app-wide ones.
        let account = [
            project.account_id.as_ref(),
            s.default_account_id.as_ref(),
            config.accounts.first().map(|a| &a.id),
        ]
        .into_iter()
        .flatten()
        .find(|id| config.account(id).is_some())
        .cloned();
        let folder = if !project.folder.is_empty() {
            project.folder.clone()
        } else {
            config
                .recent_folders
                .first()
                .cloned()
                .unwrap_or_else(|| s.default_cwd.clone())
        };
        let home = home_dir().to_string_lossy().into_owned();
        let folder_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Folder")
                .default_value(contract_home(&folder, &home))
        });
        let args_state =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. --model sonnet"));
        let subs = [&folder_state, &args_state]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(
                    input,
                    window,
                    |this, _, event: &InputEvent, _, cx| match event {
                        InputEvent::PressEnter { .. } => this.start(cx),
                        InputEvent::Change => {
                            this.error = None;
                            cx.notify();
                        }
                        _ => {}
                    },
                )
            })
            .collect();
        folder_state.update(cx, |_, cx| cx.notify());
        window.focus(&folder_state.focus_handle(cx), cx);
        let shell_name = if s.shell_path.is_empty() {
            if cfg!(windows) {
                "PowerShell".into()
            } else {
                "$SHELL".into()
            }
        } else {
            s.shell_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&s.shell_path)
                .to_string()
        };
        NewPaneDialog {
            focus: cx.focus_handle(),
            accounts: config
                .accounts
                .iter()
                .map(|a| AccountChoice {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    cli: a.cli,
                    config_dir: a.config_dir.clone(),
                    color: crate::shell::parse_hex(&a.color).unwrap_or(rgb(0x7c5cff).into()),
                })
                .collect(),
            account: if config.accounts.is_empty() {
                None
            } else {
                account
            },
            folder: folder_state,
            args: args_state,
            recent: config
                .recent_folders
                .iter()
                .take(6)
                .map(|f| contract_home(f, &home))
                .collect(),
            more: config.accounts.is_empty(),
            error: None,
            shell_name,
            _subscriptions: subs,
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let folder = self.folder.read(cx).value().trim().to_string();
        if folder.is_empty() {
            self.error = Some("Pick a folder.".into());
            cx.notify();
            return;
        }
        match std::fs::metadata(resolve_user_path(&folder, &home_dir())) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return self.fail(format!("Not a folder: {folder}"), cx),
            Err(_) => return self.fail(format!("Folder not found: {folder}"), cx),
        }
        let args = parse_args(&self.args.read(cx).value());
        cx.emit(NewPaneEvent::Start {
            account: self.account.clone(),
            cwd: folder,
            args,
        });
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    /// What Start will run, as the Electron dialog shows it.
    fn will_run(&self, cx: &App) -> String {
        let folder = self.folder.read(cx).value().to_string();
        let folder = if folder.trim().is_empty() {
            "<folder>".to_string()
        } else {
            folder
        };
        match self
            .account
            .as_ref()
            .and_then(|id| self.accounts.iter().find(|a| &a.id == id))
        {
            None => format!("cd {folder} && {}", self.shell_name),
            Some(a) => {
                let spec = a.cli.spec();
                let env = spec
                    .config_dir_var
                    .map(|v| format!("{v}={} ", a.config_dir))
                    .unwrap_or_default();
                let args = parse_args(&self.args.read(cx).value());
                let args = if args.is_empty() {
                    String::new()
                } else {
                    format!(" {}", format_args(&args))
                };
                format!("{env}{}{args}   (in {folder})", spec.bin)
            }
        }
    }
}

impl EventEmitter<NewPaneEvent> for NewPaneDialog {}

impl Focusable for NewPaneDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for NewPaneDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let option = |id: SharedString, selected: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .px_2()
                .py_1()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .border_1()
                .border_color(if selected { theme.ring } else { theme.border })
                .when(selected, |d| d.bg(cx.theme().secondary))
                .hover(|d| d.bg(cx.theme().secondary))
        };
        let mut accounts = div().flex().flex_wrap().gap_2();
        for a in &self.accounts {
            let id = a.id.clone();
            accounts = accounts.child(
                option(
                    SharedString::from(format!("acct-{}", a.id)),
                    self.account.as_ref() == Some(&a.id),
                    cx,
                )
                .child(div().size(px(8.)).rounded_full().bg(a.color))
                .child(a.name.clone())
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(a.cli.spec().provider_label),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.account = Some(id.clone());
                    cx.notify();
                })),
            );
        }
        accounts = accounts.child(
            option("acct-shell".into(), self.account.is_none(), cx)
                .child("Shell")
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(self.shell_name.clone()),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.account = None;
                    cx.notify();
                })),
        );
        let recent = self.recent.iter().enumerate().map(|(i, f)| {
            let folder = f.clone();
            div()
                .id(SharedString::from(format!("recent-{i}")))
                .px_2()
                .py(px(2.))
                .rounded_md()
                .text_xs()
                .bg(theme.secondary)
                .cursor_pointer()
                .child(f.clone())
                .on_click(cx.listener(move |this, _, window, cx| {
                    let folder = folder.clone();
                    this.folder
                        .update(cx, |s, cx| s.set_value(folder, window, cx));
                    cx.notify();
                }))
        });
        let label = |text: &'static str| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let title = if self.account.is_some() {
            "New chat"
        } else {
            "New pane"
        };
        div()
            .track_focus(&self.focus)
            .w(px(560.))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(NewPaneEvent::Cancel);
                }
            }))
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(label("Account"))
            .child(accounts)
            .child(label("Folder"))
            .child(Input::new(&self.folder))
            .when(!self.recent.is_empty(), |d| {
                d.child(div().flex().flex_wrap().gap_1().children(recent))
            })
            .child(
                div()
                    .id("more")
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .child(if self.more {
                        "▾ More options"
                    } else {
                        "▸ More options"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.more = !this.more;
                        cx.notify();
                    })),
            )
            .when(self.more && self.account.is_some(), |d| {
                d.child(label("Launch args")).child(Input::new(&self.args))
            })
            .child(
                div()
                    .text_xs()
                    .font_family(crate::terminal::mono(cx))
                    .text_color(theme.muted_foreground)
                    .child(self.will_run(cx)),
            )
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_sm().text_color(theme.danger).child(e))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .id("cancel")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .cursor_pointer()
                            .child("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NewPaneEvent::Cancel))),
                    )
                    .child(
                        div()
                            .id("start")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .cursor_pointer()
                            .child("Start")
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    ),
            )
    }
}

// ---- Command palette -----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAction {
    FocusPane { workspace: String, pane: String },
    Project(String),
    NewPane,
    NewTerminal,
    ToggleZoom,
    ClosePane,
    Settings,
    Accounts,
    Changes,
    Usage,
    Mods,
    Scheduled,
}

pub enum PaletteEvent {
    Run(PaletteAction),
    Cancel,
}

struct Item {
    label: String,
    detail: String,
    action: PaletteAction,
}

pub struct Palette {
    query: Entity<InputState>,
    items: Vec<Item>,
    selected: usize,
    _subscription: Subscription,
}

/// True when every character of `query` appears in `text` in order (case-insensitive).
pub fn fuzzy_match(text: &str, query: &str) -> bool {
    let mut chars = text.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| !c.is_whitespace())
        .all(|q| chars.any(|c| c == q))
}

impl Palette {
    pub fn new(config: &Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut items = Vec::new();
        for w in &config.workspaces {
            for p in &w.panes {
                let account = p
                    .account_id
                    .as_deref()
                    .and_then(|id| config.account(id))
                    .map(|a| a.name.clone());
                items.push(Item {
                    label: if p.title.is_empty() {
                        "pane".into()
                    } else {
                        p.title.clone()
                    },
                    detail: format!(
                        "{}{}",
                        w.name,
                        account.map(|a| format!(" · {a}")).unwrap_or_default()
                    ),
                    action: PaletteAction::FocusPane {
                        workspace: w.id.clone(),
                        pane: p.id.clone(),
                    },
                });
            }
        }
        for w in &config.workspaces {
            items.push(Item {
                label: w.name.clone(),
                detail: "Project".into(),
                action: PaletteAction::Project(w.id.clone()),
            });
        }
        for (label, action) in [
            ("New chat…", PaletteAction::NewPane),
            ("New terminal", PaletteAction::NewTerminal),
            ("Zoom pane / back to grid", PaletteAction::ToggleZoom),
            ("Close pane", PaletteAction::ClosePane),
            ("Settings", PaletteAction::Settings),
            ("Accounts", PaletteAction::Accounts),
            ("Show / hide the git diff", PaletteAction::Changes),
            ("Usage", PaletteAction::Usage),
            ("Mods", PaletteAction::Mods),
            ("Scheduled chats", PaletteAction::Scheduled),
        ] {
            items.push(Item {
                label: label.into(),
                detail: "Command".into(),
                action,
            });
        }
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search panes, projects and commands")
        });
        let sub = cx.subscribe_in(
            &query,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { .. } => this.run(cx),
                InputEvent::Change => {
                    this.selected = 0;
                    cx.notify();
                }
                _ => {}
            },
        );
        window.focus(&query.focus_handle(cx), cx);
        Palette {
            query,
            items,
            selected: 0,
            _subscription: sub,
        }
    }

    fn matches(&self, cx: &App) -> Vec<usize> {
        let q = self.query.read(cx).value().to_string();
        (0..self.items.len())
            .filter(|&i| {
                fuzzy_match(
                    &format!("{} {}", self.items[i].label, self.items[i].detail),
                    &q,
                )
            })
            .take(50)
            .collect()
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if let Some(&i) = self.matches(cx).get(self.selected) {
            cx.emit(PaletteEvent::Run(self.items[i].action.clone()));
        }
    }
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let matches = self.matches(cx);
        let rows = matches.iter().enumerate().map(|(n, &i)| {
            let item = &self.items[i];
            let action = item.action.clone();
            div()
                .id(SharedString::from(format!("palette-{i}")))
                .px_3()
                .py_1()
                .rounded_md()
                .flex()
                .gap_2()
                .cursor_pointer()
                .when(n == self.selected, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
                .child(div().flex_1().truncate().child(item.label.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(item.detail.clone()),
                )
                .on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(PaletteEvent::Run(action.clone()))),
                )
        });
        let count = matches.len();
        div()
            .w(px(600.))
            .max_h(px(440.))
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .capture_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => cx.emit(PaletteEvent::Cancel),
                    "down" if count > 0 => {
                        this.selected = (this.selected + 1) % count;
                        cx.notify();
                        cx.stop_propagation();
                    }
                    "up" if count > 0 => {
                        this.selected = (this.selected + count - 1) % count;
                        cx.notify();
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .child(Input::new(&self.query))
            .child(
                div()
                    .id("palette-list")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .children(rows),
            )
    }
}

// ---- Quit with agents running --------------------------------------------------------------

pub enum QuitEvent {
    /// `keep`: leave the agents running in the background host; else stop them and quit.
    Choose {
        keep: bool,
        remember: bool,
    },
    Cancel,
}

/// "N agents are running": keep them, stop them and quit, or cancel (`chooseOnClose`).
pub struct QuitDialog {
    live: usize,
    remember: bool,
}

impl QuitDialog {
    pub fn new(live: usize) -> Self {
        QuitDialog {
            live,
            remember: false,
        }
    }
}

impl EventEmitter<QuitEvent> for QuitDialog {}

impl Render for QuitDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(primary, |d| {
                    d.bg(theme.primary).text_color(theme.primary_foreground)
                })
                .when(!primary, |d| d.border_1().border_color(theme.border))
                .child(label)
        };
        let message = format!(
            "{} agent{} running",
            self.live,
            if self.live == 1 { " is" } else { "s are" }
        );
        div()
            .w(px(460.))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(message))
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Keep them running in the background, and Wraithgrid shows them again, screen and all, next time it opens. Or stop them now.",
            ))
            .child(
                div()
                    .id("remember")
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .cursor_pointer()
                    .child(if self.remember { "☑" } else { "☐" })
                    .child("Remember my choice (Settings → When the window closes)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.remember = !this.remember;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(button("quit-cancel", "Cancel", false).on_click(cx.listener(|_, _, _, cx| cx.emit(QuitEvent::Cancel))))
                    .child(
                        button("quit-stop", "Stop agents and quit", false)
                            .on_click(cx.listener(|this, _, _, cx| cx.emit(QuitEvent::Choose { keep: false, remember: this.remember }))),
                    )
                    .child(
                        button("quit-keep", "Keep running", true)
                            .on_click(cx.listener(|this, _, _, cx| cx.emit(QuitEvent::Choose { keep: true, remember: this.remember }))),
                    ),
            )
    }
}

// ---- New project ---------------------------------------------------------------------------

pub enum NewProjectEvent {
    Create {
        name: String,
        folder: String,
        account: Option<String>,
        start_chat: bool,
    },
    Cancel,
}

/// New project (`NewProjectDialog.tsx`): a folder, a name (the folder's by default), the
/// account its chats use, and whether to start a chat right away.
pub struct NewProjectDialog {
    folder: Entity<InputState>,
    name: Entity<InputState>,
    accounts: Vec<(String, String, Hsla)>,
    account: Option<String>,
    start_chat: bool,
    pub error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl NewProjectDialog {
    pub fn new(config: &Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let folder = cx.new(|cx| InputState::new(window, cx).placeholder("~/code/my-project"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. wraithgrid"));
        let subs = [&folder, &name]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(
                    input,
                    window,
                    |this, _, event: &InputEvent, _, cx| match event {
                        InputEvent::PressEnter { .. } => this.create(cx),
                        InputEvent::Change => {
                            this.error = None;
                            cx.notify();
                        }
                        _ => {}
                    },
                )
            })
            .collect();
        window.focus(&folder.focus_handle(cx), cx);
        let account = config
            .settings
            .default_account_id
            .clone()
            .filter(|id| config.account(id).is_some())
            .or_else(|| config.accounts.first().map(|a| a.id.clone()));
        NewProjectDialog {
            folder,
            name,
            accounts: config
                .accounts
                .iter()
                .map(|a| {
                    (
                        a.id.clone(),
                        a.name.clone(),
                        crate::shell::parse_hex(&a.color).unwrap_or(rgb(0x7c5cff).into()),
                    )
                })
                .collect(),
            account,
            start_chat: true,
            error: None,
            _subscriptions: subs,
        }
    }

    /// The name the project gets when none is typed: its folder's.
    fn suggested(&self, cx: &App) -> String {
        let folder = self.folder.read(cx).value().trim().to_string();
        if folder.is_empty() {
            String::new()
        } else {
            wraithgrid_core::paths::base_name(&folder)
        }
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let typed = self.name.read(cx).value().trim().to_string();
        let name = if typed.is_empty() {
            self.suggested(cx)
        } else {
            typed
        };
        let folder = self.folder.read(cx).value().trim().to_string();
        if !folder.is_empty() {
            match std::fs::metadata(resolve_user_path(&folder, &home_dir())) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return self.fail(format!("Not a folder: {folder}"), cx),
                Err(_) => return self.fail(format!("Folder not found: {folder}"), cx),
            }
        }
        cx.emit(NewProjectEvent::Create {
            name,
            folder,
            account: self.account.clone(),
            start_chat: self.start_chat,
        });
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }
}

impl EventEmitter<NewProjectEvent> for NewProjectDialog {}

impl Render for NewProjectDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let chip = |id: SharedString, on: bool| {
            div()
                .id(id)
                .px_2()
                .py_1()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .border_1()
                .border_color(if on { theme.ring } else { theme.border })
                .when(on, |d| d.bg(theme.secondary))
        };
        let accounts = self.accounts.iter().map(|(id, name, color)| {
            let pick = id.clone();
            chip(
                SharedString::from(format!("np-acct-{id}")),
                self.account.as_ref() == Some(id),
            )
            .child(div().size(px(8.)).rounded_full().bg(*color))
            .child(name.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.account = Some(pick.clone());
                cx.notify();
            }))
        });
        let label = |t: &'static str| div().text_xs().text_color(theme.muted_foreground).child(t);
        let suggested = self.suggested(cx);
        let can_start = self.account.is_some() && !self.folder.read(cx).value().trim().is_empty();
        div()
            .w(px(520.))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(NewProjectEvent::Cancel);
                }
            }))
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("New project"),
            )
            .child(label("Folder"))
            .child(Input::new(&self.folder))
            .child(label("Name"))
            .child(Input::new(&self.name))
            .when(!suggested.is_empty(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("Empty: {suggested}")),
                )
            })
            .when(!self.accounts.is_empty(), |d| {
                d.child(label("Account"))
                    .child(div().flex().flex_wrap().gap_2().children(accounts))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("The default account for the project’s chats."),
                    )
            })
            .child(label("Start"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        chip("np-start".into(), self.start_chat && can_start)
                            .child("Start a chat")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_chat = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        chip("np-empty".into(), !self.start_chat || !can_start)
                            .child("Empty project")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_chat = false;
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_sm().text_color(theme.danger).child(e))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .id("np-cancel")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .cursor_pointer()
                            .child("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NewProjectEvent::Cancel))),
                    )
                    .child(
                        div()
                            .id("np-create")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .cursor_pointer()
                            .child("Create")
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
                    ),
            )
    }
}

// ---- Edit project --------------------------------------------------------------------------

pub enum ProjectEvent {
    Save {
        id: String,
        name: String,
        folder: String,
        account: Option<String>,
    },
    Delete {
        id: String,
    },
    Cancel,
}

/// A project's name, folder and default account, and deleting it (the sidebar's project menu).
pub struct ProjectDialog {
    id: String,
    name: Entity<InputState>,
    folder: Entity<InputState>,
    accounts: Vec<(String, String)>,
    account: Option<String>,
    can_delete: bool,
    confirm_delete: bool,
    panes: usize,
    _subscriptions: Vec<Subscription>,
}

impl ProjectDialog {
    pub fn new(
        config: &Config,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Self> {
        let ws = config.workspaces.iter().find(|w| w.id == id)?;
        let name = cx.new(|cx| InputState::new(window, cx).default_value(ws.name.clone()));
        let folder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("No folder")
                .default_value(ws.folder.clone())
        });
        let subs = [&name, &folder]
            .into_iter()
            .map(|i| {
                cx.subscribe_in(i, window, |this, _, e: &InputEvent, _, cx| {
                    if let InputEvent::PressEnter { .. } = e {
                        this.save(cx);
                    }
                })
            })
            .collect();
        window.focus(&name.focus_handle(cx), cx);
        Some(ProjectDialog {
            id: id.into(),
            name,
            folder,
            accounts: config
                .accounts
                .iter()
                .map(|a| (a.id.clone(), a.name.clone()))
                .collect(),
            account: ws.account_id.clone(),
            can_delete: config.workspaces.len() > 1,
            confirm_delete: false,
            panes: ws.panes.len(),
            _subscriptions: subs,
        })
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        cx.emit(ProjectEvent::Save {
            id: self.id.clone(),
            name: self.name.read(cx).value().to_string(),
            folder: self.folder.read(cx).value().to_string(),
            account: self.account.clone(),
        });
    }
}

impl EventEmitter<ProjectEvent> for ProjectDialog {}

impl Render for ProjectDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let chip = |id: SharedString, label: String, on: bool| {
            div()
                .id(id)
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .text_sm()
                .cursor_pointer()
                .border_color(if on { theme.ring } else { theme.border })
                .when(on, |d| d.bg(theme.secondary))
                .child(label)
        };
        let mut accounts = vec![chip(
            "pj-acct-none".into(),
            "App default".into(),
            self.account.is_none(),
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.account = None;
            cx.notify();
        }))
        .into_any_element()];
        for (id, name) in &self.accounts {
            let pick = id.clone();
            accounts.push(
                chip(
                    SharedString::from(format!("pj-acct-{id}")),
                    name.clone(),
                    self.account.as_ref() == Some(id),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.account = Some(pick.clone());
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        let label = |t: &'static str| div().text_xs().text_color(theme.muted_foreground).child(t);
        let button = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .child(text)
        };
        let delete_note = format!(
            "Delete this project? Its {} pane{} stop.",
            self.panes,
            if self.panes == 1 { "" } else { "s" }
        );
        div()
            .w(px(480.))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(ProjectEvent::Cancel);
                }
            }))
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Project"),
            )
            .child(label("Name"))
            .child(Input::new(&self.name))
            .child(label("Folder (where its new chats start)"))
            .child(Input::new(&self.folder))
            .child(label("Account for its chats"))
            .child(div().flex().flex_wrap().gap_2().children(accounts))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(self.can_delete && !self.confirm_delete, |d| {
                        d.child(
                            button("pj-delete", "Delete…")
                                .text_color(theme.danger)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_delete = true;
                                    cx.notify();
                                })),
                        )
                    })
                    .when(self.confirm_delete, |d| {
                        let id = self.id.clone();
                        d.child(div().text_sm().text_color(theme.danger).child(delete_note))
                            .child(
                                button("pj-really", "Delete")
                                    .text_color(theme.danger)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(ProjectEvent::Delete { id: id.clone() });
                                    })),
                            )
                    })
                    .child(div().flex_1())
                    .child(
                        button("pj-cancel", "Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ProjectEvent::Cancel))),
                    )
                    .child(
                        div()
                            .id("pj-save")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .cursor_pointer()
                            .child("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}

// ---- A question with a few answers -------------------------------------------------------

pub enum ChoiceEvent {
    Pick(String),
    Cancel,
}

/// A short question with its answers: "Continue as…", and switching at the limit.
pub struct ChoiceDialog {
    title: String,
    message: String,
    /// (id, label, detail)
    choices: Vec<(String, String, String)>,
    cancel: String,
}

impl ChoiceDialog {
    pub fn new(
        title: impl Into<String>,
        message: impl Into<String>,
        choices: Vec<(String, String, String)>,
        cancel: impl Into<String>,
    ) -> Self {
        ChoiceDialog {
            title: title.into(),
            message: message.into(),
            choices,
            cancel: cancel.into(),
        }
    }
}

impl EventEmitter<ChoiceEvent> for ChoiceDialog {}

impl Render for ChoiceDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let choices = self.choices.iter().map(|(id, label, detail)| {
            let pick = id.clone();
            div()
                .id(SharedString::from(format!("choice-{id}")))
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .flex()
                .gap_2()
                .child(div().font_weight(FontWeight::MEDIUM).child(label.clone()))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(detail.clone()),
                )
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChoiceEvent::Pick(pick.clone()))))
        });
        div()
            .w(px(460.))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(ChoiceEvent::Cancel);
                }
            }))
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title.clone()),
            )
            .when(!self.message.is_empty(), |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(self.message.clone()),
                )
            })
            .children(choices)
            .child(
                div().flex().justify_end().child(
                    div()
                        .id("choice-cancel")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .cursor_pointer()
                        .child(self.cancel.clone())
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ChoiceEvent::Cancel))),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fuzzy() {
        assert!(super::fuzzy_match("WraithGrid · org-info", "wgo"));
        assert!(super::fuzzy_match("Close pane", "close p"));
        assert!(!super::fuzzy_match("shell", "zsh"));
        assert!(super::fuzzy_match("anything", ""));
    }
}
