//! The window's overlays: New pane (`NewPaneDialog.tsx`: Account → Folder → Start, launch args
//! and a branch of its own (a git worktree, #162) under More options), New project, a project's
//! settings, the command palette (`CommandPalette.tsx`: search panes, projects, accounts and
//! commands), quitting with agents running, and short questions. Each emits what was chosen;
//! the window carries it out.
//!
//! Every dialog keeps keyboard focus inside itself (`ModalFocus`), closes on Esc, fits the
//! window (scrolling when taller), and one that a stray click must not lose holds the backdrop
//! itself (`hold_backdrop`). Requests the window's own event enums can't carry yet go out as
//! separate events (`OpenAccounts`, `PinFolders`, `PaletteCommand`, `CreateRemoteProject`).

use std::time::Duration;

use gpui_kit::component::input::{self, Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::args::{format_args, parse_args};
use wraithgrid_core::config::Config;
use wraithgrid_core::config::Pane;
use wraithgrid_core::paths::{contract_home, home_dir, resolve_user_path, worktrees_root};
use wraithgrid_git::{add_worktree, GitCli, WorktreeFrom};

use crate::a11y::A11y as _;
use crate::folder_picker::Picking;

// ---- Shared pieces -------------------------------------------------------------------------

/// Keeps keyboard focus inside a modal (`Dialog.tsx`): its own handle on the first frame
/// (unless a field in it was focused when it was made), and back inside whenever focus has
/// left, so keys never reach the pane behind and Esc always closes it.
pub(crate) struct ModalFocus {
    handle: Option<FocusHandle>,
    frames: u8,
    child_first: bool,
}

impl ModalFocus {
    /// The dialog itself takes focus when it opens.
    pub fn new() -> Self {
        ModalFocus {
            handle: None,
            frames: 0,
            child_first: false,
        }
    }

    /// A field inside was focused when the dialog was made: leave it there.
    pub fn child_first() -> Self {
        ModalFocus {
            child_first: true,
            ..Self::new()
        }
    }

    /// After something drawn in the dialog's place (the folder picker) closes and focuses a
    /// field of the dialog again.
    pub fn refocus_child(&mut self) {
        self.frames = 0;
        self.child_first = true;
    }

    /// The handle to `track_focus` with; call once per render.
    pub fn keep(&mut self, window: &mut Window, cx: &mut App) -> FocusHandle {
        let handle = self.handle.get_or_insert_with(|| cx.focus_handle()).clone();
        if self.frames == 0 {
            self.frames = 1;
            if !self.child_first {
                window.focus(&handle, cx);
            }
        } else if !handle.contains_focused(window, cx) {
            window.focus(&handle, cx);
        }
        handle
    }
}

/// Draws a dialog that a click beside it must not close (signing in, a worktree being made)
/// on a clear layer of its own over the whole window, which takes those clicks before the
/// window's backdrop sees them. Placed like the window's overlay places dialogs.
pub(crate) fn hold_backdrop(content: AnyElement, window: &Window) -> AnyElement {
    let size = window.viewport_size();
    deferred(
        anchored().position(point(px(0.), px(0.))).child(
            div()
                .id("held-backdrop")
                .occlude()
                .w(size.width)
                .h(size.height)
                .flex()
                .flex_col()
                .items_center()
                .pt(px(90.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(content),
        ),
    )
    .with_priority(1)
    .into_any_element()
}

/// Opened from "No accounts yet" (New pane, New project): Accounts, with its import form
/// (`import`) or its add form ready (`setAccountsMode`).
// The window reads it once it subscribes to this event (shell.rs follow-up).
#[allow(dead_code)]
pub struct OpenAccounts {
    pub import: bool,
}

/// The folder picker's pins changed: save them (`settings.pinnedFolders`).
// The window reads it once it subscribes to this event (shell.rs follow-up).
#[allow(dead_code)]
pub struct PinFolders(pub Vec<String>);

/// A dialog's title row with its ✕ (`dlg-hd`).
fn header(
    title: impl Into<SharedString>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().clone();
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .child(
            div()
                .id("dialog-close")
                .a11y_button("Close dialog (Esc)")
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .text_color(theme.muted_foreground)
                .hover(|d| d.bg(theme.secondary))
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new("Close (Esc)").build(window, cx)
                })
                .child("✕")
                .on_click(on_close),
        )
}

/// The frame every dialog is drawn in.
/// As tall as the window allows below the overlay's top gap; taller bodies scroll.
fn frame(
    id: &'static str,
    title: impl Into<SharedString>,
    width: f32,
    window: &Window,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let room = (window.viewport_size().height - px(114.)).max(px(160.));
    div()
        .id(id)
        .a11y_dialog(title)
        .w(px(width))
        .max_h(room)
        .overflow_y_scroll()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .shadow_lg()
}

/// A footer button. A disabled one is dimmed and takes no clicks.
fn button(
    id: &'static str,
    label: impl Into<SharedString>,
    primary: bool,
    enabled: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().clone();
    let label = label.into();
    div()
        .id(id)
        .a11y_button(label.clone())
        .px_3()
        .py_1()
        .rounded_md()
        .flex_shrink_0()
        .when(primary, |d| {
            d.bg(theme.primary).text_color(theme.primary_foreground)
        })
        .when(!primary, |d| d.border_1().border_color(theme.border))
        .when(enabled, |d| {
            d.cursor_pointer().hover(move |d| {
                if primary {
                    d.bg(theme.primary_hover)
                } else {
                    d.bg(theme.secondary)
                }
            })
        })
        .when(!enabled, |d| d.opacity(0.45).cursor_default())
        .child(label)
}

/// A hint line (`.hint`).
fn hint(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// An error line (`.hint.err`, `role="alert"`).
fn error_line(id: &'static str, text: String, cx: &App) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Alert)
        .aria_label(text.clone())
        .text_sm()
        .text_color(cx.theme().danger)
        .child(text)
}

/// A field's label (`.lbl`).
fn label(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// An account to pick (`np-acct`): its initial, name, CLI and whether it is signed in.
#[derive(Clone)]
struct AccountChoice {
    id: String,
    name: String,
    cli: AgentCli,
    config_dir: String,
    color: Hsla,
    signed_in: bool,
}

fn account_choices(config: &Config) -> Vec<AccountChoice> {
    config
        .accounts
        .iter()
        .map(|a| AccountChoice {
            id: a.id.clone(),
            name: a.name.clone(),
            cli: a.cli,
            config_dir: a.config_dir.clone(),
            color: crate::shell::parse_hex(&a.color).unwrap_or(rgb(0x7c5cff).into()),
            signed_in: a.signed_in,
        })
        .collect()
}

fn account_chip(a: &AccountChoice, on: bool, enabled: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme().clone();
    div()
        .id(SharedString::from(format!("acct-{}", a.id)))
        .role(Role::RadioButton)
        .aria_label(format!(
            "{} {}{}",
            a.name,
            a.cli.spec().provider_label,
            if a.signed_in { "" } else { ", not signed in" }
        ))
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .px_2()
        .py_1()
        .rounded_md()
        .flex()
        .items_center()
        .gap_2()
        .border_1()
        .border_color(if on { theme.ring } else { theme.border })
        .when(on, |d| d.bg(theme.secondary))
        .when(enabled, |d| {
            d.cursor_pointer().hover(|d| d.bg(theme.secondary))
        })
        .when(!enabled, |d| d.opacity(0.45))
        .child(crate::settings::initial(&a.name, a.color, 20.))
        .child(a.name.clone())
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(a.cli.as_str()),
        )
        .when(!a.signed_in, |d| {
            d.child(
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child("not signed in"),
            )
        })
        .when(on, |d| d.child(div().text_color(theme.ring).child("✓")))
}

/// The chosen folder, checked off the UI thread: it exists, and its git branch.
#[derive(Clone)]
struct FolderCheck {
    folder: String,
    branch: Option<String>,
    error: Option<String>,
}

/// Checks `folder` a moment after typing stops; `done` gets the result if it is still the
/// folder in the field.
fn check_folder<T: 'static>(
    folder: String,
    cx: &mut Context<T>,
    done: fn(&mut T, FolderCheck, &mut Context<T>),
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        cx.background_executor()
            .timer(Duration::from_millis(150))
            .await;
        let f = folder.clone();
        let listed = cx
            .background_spawn(async move { wraithgrid_core::folders::list_folder(&f, &home_dir()) })
            .await;
        let check = match listed.result {
            Ok(l) => FolderCheck {
                folder,
                branch: l.branch,
                error: None,
            },
            Err(e) => FolderCheck {
                folder,
                branch: None,
                error: Some(e),
            },
        };
        let _ = this.update(cx, |this, cx| done(this, check, cx));
    })
}

/// The git branch badge after a folder field (`np-br`).
fn branch_badge(branch: &str, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex_shrink_0()
        .px_2()
        .py(px(1.))
        .rounded_md()
        .bg(theme.secondary)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(format!("⑂ {branch}"))
}

/// The folder field and its "Choose…" button (the in-app picker), with the folder's branch.
fn folder_row(
    folder: &Entity<InputState>,
    branch: Option<String>,
    on_choose: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().flex_1().child(Input::new(folder)))
        .children(branch.map(|b| branch_badge(&b, cx)))
        .child(
            div()
                .id("choose-folder")
                .a11y_button("Choose a folder")
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .text_sm()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child("Choose…")
                .on_click(on_choose),
        )
}

// ---- New pane ------------------------------------------------------------------------------

pub enum NewPaneEvent {
    Start {
        account: Option<String>,
        cwd: String,
        args: Vec<String>,
        /// The project's "Work on a new branch" to remember (#162); None from the Git menu.
        worktree_panes: Option<bool>,
    },
    /// The folder picker's pins changed: save them (`settings.pinnedFolders`).
    PinFolders(Vec<String>),
    Cancel,
}

pub struct NewPaneDialog {
    focus: ModalFocus,
    accounts: Vec<AccountChoice>,
    account: Option<String>,
    /// "Open a plain shell instead".
    shell: bool,
    folder: Entity<InputState>,
    args: Entity<InputState>,
    /// Recent folders, home as `~`.
    recent: Vec<String>,
    more: bool,
    error: Option<String>,
    shell_name: String,
    /// "Work on a new branch (git worktree)", and that branch's name.
    worktree: bool,
    branch: Entity<InputState>,
    /// Opened from the Git menu for a pane (`branchFrom`): not remembered for the project.
    branch_from: bool,
    /// The project runs on another machine: its folder is there, so nothing is checked here
    /// and no worktree is made.
    remote: bool,
    busy: bool,
    check: Option<FolderCheck>,
    _check: Option<Task<()>>,
    picking: Picking,
    _subscriptions: Vec<Subscription>,
}

/// A branch name to start from, free in practice: `task-1105-0942` (month, day, time).
pub fn suggest_branch<Tz: chrono::TimeZone>(now: chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    now.format("task-%m%d-%H%M").to_string()
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
        let args_state = cx.new(|cx| InputState::new(window, cx).placeholder("--resume"));
        let remote = project.machine_id.is_some();
        let worktree = project.worktree_panes && !remote;
        let branch_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("feat/rate-limit")
                .default_value(if worktree {
                    suggest_branch(chrono::Local::now())
                } else {
                    String::new()
                })
        });
        let mut subs: Vec<Subscription> = [&args_state, &branch_state]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| {
                    if let InputEvent::Change = event {
                        this.error = None;
                        cx.notify();
                    }
                })
            })
            .collect();
        subs.push(cx.subscribe_in(
            &folder_state,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    this.error = None;
                    this.recheck(cx);
                }
            },
        ));
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
        let mut dialog = NewPaneDialog {
            focus: ModalFocus::child_first(),
            accounts: account_choices(config),
            shell: config.accounts.is_empty(),
            account,
            folder: folder_state,
            args: args_state,
            recent: config
                .recent_folders
                .iter()
                .map(|f| contract_home(f, &home))
                .collect(),
            more: config.accounts.is_empty(),
            error: None,
            shell_name,
            worktree,
            branch: branch_state,
            branch_from: false,
            remote,
            busy: false,
            check: None,
            _check: None,
            picking: Picking::new(config),
            _subscriptions: subs,
        };
        dialog.recheck(cx);
        dialog
    }

    /// Set up from the Git menu (#162): that pane's folder and account, a new branch suggested,
    /// so Enter starts it.
    pub fn branch_from(
        &mut self,
        pane: &Pane,
        config: &Config,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if pane.shell {
            self.shell = true;
        } else if let Some(id) = pane
            .account_id
            .as_ref()
            .filter(|id| config.account(id).is_some())
        {
            self.shell = false;
            self.account = Some(id.clone());
        }
        let home = home_dir().to_string_lossy().into_owned();
        let folder = contract_home(&pane.cwd, &home);
        self.folder
            .update(cx, |s, cx| s.set_value(folder, window, cx));
        self.branch.update(cx, |s, cx| {
            s.set_value(suggest_branch(chrono::Local::now()), window, cx)
        });
        self.worktree = true;
        self.branch_from = true;
        self.more = true;
        self.recheck(cx);
        window.focus(&self.branch.focus_handle(cx), cx);
        self.focus.refocus_child();
        cx.notify();
    }

    fn folder_text(&self, cx: &App) -> String {
        self.folder.read(cx).value().trim().to_string()
    }

    fn recheck(&mut self, cx: &mut Context<Self>) {
        let folder = self.folder_text(cx);
        if self.remote || folder.is_empty() {
            self._check = None;
            self.check = None;
            return cx.notify();
        }
        self._check = Some(check_folder(folder, cx, |this: &mut Self, c, cx| {
            this.check = Some(c);
            cx.notify();
        }));
        cx.notify();
    }

    /// The check of the folder now in the field, once it has come back.
    fn folder_check(&self, cx: &App) -> Option<&FolderCheck> {
        let folder = self.folder_text(cx);
        self.check.as_ref().filter(|c| c.folder == folder)
    }

    fn can_start(&self, cx: &App) -> bool {
        !self.busy
            && !self.folder_text(cx).is_empty()
            && self.folder_check(cx).is_none_or(|c| c.error.is_none())
            && (self.shell || self.account.is_some())
            && (!self.worktree || !self.branch.read(cx).value().trim().is_empty())
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if !self.can_start(cx) {
            return;
        }
        let folder = self.folder_text(cx);
        let args = if self.shell {
            vec![]
        } else {
            parse_args(&self.args.read(cx).value())
        };
        let account = if self.shell {
            None
        } else {
            self.account.clone()
        };
        let worktree_panes = (!self.branch_from && !self.remote).then_some(self.worktree);
        if self.remote {
            // The folder is on the other machine: its host says if it is missing.
            return cx.emit(NewPaneEvent::Start {
                account,
                cwd: folder,
                args,
                worktree_panes,
            });
        }
        let branch = self
            .worktree
            .then(|| self.branch.read(cx).value().trim().to_string());
        self.busy = branch.is_some();
        self.error = None;
        cx.notify();
        // Off the UI thread: the folder is checked again, then the worktree made, if asked.
        cx.spawn(async move |this, cx| {
            let made = cx
                .background_spawn(async move {
                    let home = home_dir();
                    let dir = resolve_user_path(&folder, &home);
                    match std::fs::metadata(&dir) {
                        Ok(m) if m.is_dir() => {}
                        Ok(_) => return Err(format!("Not a folder: {folder}")),
                        Err(_) => return Err(format!("Folder not found: {folder}")),
                    }
                    let Some(branch) = branch else {
                        return Ok(folder);
                    };
                    add_worktree(
                        &GitCli::default(),
                        &dir,
                        &branch,
                        &worktrees_root(&home),
                        false,
                        WorktreeFrom::New,
                    )
                    .map(|dir| contract_home(&dir.to_string_lossy(), &home.to_string_lossy()))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match made {
                    Ok(cwd) => cx.emit(NewPaneEvent::Start {
                        account,
                        cwd,
                        args,
                        worktree_panes,
                    }),
                    Err(e) => this.fail(e, cx),
                }
            });
        })
        .detach();
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    fn set_folder(&mut self, folder: String, window: &mut Window, cx: &mut Context<Self>) {
        self.folder
            .update(cx, |s, cx| s.set_value(folder, window, cx));
        self.recheck(cx);
    }

    /// What Start will run, as the Electron dialog shows it.
    fn will_run(&self, cx: &App) -> String {
        let folder = self.folder.read(cx).value().to_string();
        let folder = if folder.trim().is_empty() {
            "<folder>".to_string()
        } else {
            folder
        };
        let branch = self.branch.read(cx).value().trim().to_string();
        let run_in = if self.worktree && !branch.is_empty() {
            format!("~/.wraithgrid/worktrees/…/{branch}")
        } else {
            folder.clone()
        };
        let account = self
            .account
            .as_ref()
            .and_then(|id| self.accounts.iter().find(|a| &a.id == id));
        match account.filter(|_| !self.shell) {
            None if self.shell => format!("cd {folder} && {}", self.shell_name),
            None => "<account>".into(),
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
                format!("{env}{}{args}   (in {run_in})", spec.bin)
            }
        }
    }
}

impl EventEmitter<NewPaneEvent> for NewPaneDialog {}
impl EventEmitter<OpenAccounts> for NewPaneDialog {}

impl Focusable for NewPaneDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.folder.focus_handle(cx)
    }
}

impl Render for NewPaneDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.picking.picker.clone() {
            self.focus.refocus_child();
            return picker.into_any_element();
        }
        let focus = self.focus.keep(window, cx);
        let theme = cx.theme().clone();
        let accounts: AnyElement = if self.accounts.is_empty() {
            div()
                .p_2()
                .rounded_md()
                .bg(theme.secondary)
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .text_sm()
                .child("No accounts yet. An agent pane needs one.")
                .child(
                    button("np-import", "Import ~/.claude", false, true, cx).on_click(
                        cx.listener(|_, _, _, cx| cx.emit(OpenAccounts { import: true })),
                    ),
                )
                .child(
                    button("np-add-account", "Add account", false, true, cx).on_click(
                        cx.listener(|_, _, _, cx| cx.emit(OpenAccounts { import: false })),
                    ),
                )
                .into_any_element()
        } else {
            let shell = self.shell;
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(self.accounts.iter().map(|a| {
                    let id = a.id.clone();
                    let on = !shell && self.account.as_ref() == Some(&a.id);
                    account_chip(a, on, !shell, cx).when(!shell, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            this.account = Some(id.clone());
                            cx.notify();
                        }))
                    })
                }))
                .into_any_element()
        };
        let current = self.folder_text(cx);
        let recent = self
            .recent
            .iter()
            .filter(|f| **f != current)
            .take(4)
            .enumerate()
            .map(|(i, f)| {
                let folder = f.clone();
                let name = wraithgrid_core::paths::base_name(f);
                div()
                    .id(SharedString::from(format!("recent-{i}")))
                    .a11y_button(f.clone())
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .flex()
                    .gap_1()
                    .text_xs()
                    .bg(theme.secondary)
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.secondary_active))
                    .child(div().font_weight(FontWeight::MEDIUM).child(name))
                    .child(div().text_color(theme.muted_foreground).child(f.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_folder(folder.clone(), window, cx);
                    }))
            })
            .collect::<Vec<_>>();
        let title = if self.branch_from {
            "New pane on a new branch"
        } else if !self.shell {
            "New chat"
        } else {
            "New pane"
        };
        let can_start = self.can_start(cx);
        let check = self.folder_check(cx).cloned();
        let more_on = [
            (!self.args.read(cx).value().trim().is_empty() && !self.shell).then_some("args"),
            self.worktree.then_some("worktree"),
            self.shell.then_some("shell"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let busy = self.busy;
        let dialog = frame("new-pane-dialog", title, 560., window, cx)
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => cx.emit(NewPaneEvent::Cancel),
                    // Enter anywhere in it starts (a one-line field passes Enter on).
                    "enter" => this.start(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(header(
                title,
                cx.listener(|_, _, _, cx| cx.emit(NewPaneEvent::Cancel)),
                cx,
            ))
            .child(label("Account", cx))
            .child(accounts)
            .child(label("Folder", cx))
            .child(folder_row(
                &self.folder,
                check.as_ref().and_then(|c| c.branch.clone()),
                cx.listener(|this, _, window, cx| {
                    let folder = this.folder.clone();
                    Picking::open(
                        &folder,
                        |d: &mut Self| &mut d.picking,
                        |pins, cx| cx.emit(NewPaneEvent::PinFolders(pins)),
                        this,
                        window,
                        cx,
                    )
                }),
                cx,
            ))
            .when_some(check.and_then(|c| c.error), |d, e| {
                d.child(error_line("np-folder-error", e, cx))
            })
            .when(self.remote, |d| {
                d.child(hint(
                    "This project runs on another machine: the folder is there.",
                    cx,
                ))
            })
            .when(!recent.is_empty(), |d| {
                d.child(div().flex().flex_wrap().gap_1().children(recent))
            })
            .child(
                div()
                    .id("more")
                    .role(Role::Button)
                    .aria_label("More options")
                    .aria_expanded(self.more)
                    .flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .child(if self.more {
                        "▾ More options"
                    } else {
                        "▸ More options"
                    })
                    .when(!self.more && !more_on.is_empty(), |d| {
                        d.child(div().text_color(theme.foreground).child(more_on))
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.more = !this.more;
                        cx.notify();
                    })),
            )
            .when(self.more, |d| self.more_options(d, cx))
            .when_some(self.error.clone(), |d, e| {
                d.child(error_line("np-error", e, cx))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(hint("Enter to start · Esc to cancel", cx)),
                    )
                    .child(
                        button("cancel", "Cancel", false, true, cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NewPaneEvent::Cancel))),
                    )
                    .child(
                        button(
                            "start",
                            if self.busy {
                                "Creating worktree…"
                            } else if !self.shell {
                                "Start chat"
                            } else {
                                "Create pane"
                            },
                            true,
                            can_start,
                            cx,
                        )
                        .when(can_start, |d| {
                            d.on_click(cx.listener(|this, _, _, cx| this.start(cx)))
                        }),
                    ),
            )
            .into_any_element();
        // A worktree being made must not be lost to a click beside the dialog.
        if busy {
            hold_backdrop(dialog, window)
        } else {
            dialog
        }
    }
}

impl NewPaneDialog {
    /// Launch args, a branch of its own (#162), a plain shell instead, and what will run.
    fn more_options(&self, d: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let shell = self.shell;
        let quick = |id: &'static str, text: &'static str, cx: &mut Context<Self>| {
            div()
                .id(id)
                .a11y_button(text)
                .px_2()
                .py(px(2.))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .text_xs()
                .font_family(crate::terminal::mono(cx))
                .when(shell, |d| d.opacity(0.45))
                .when(!shell, |d| {
                    d.cursor_pointer()
                        .hover(|d| d.bg(theme.secondary))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.args.update(cx, |s, cx| s.set_value(text, window, cx));
                            cx.notify();
                        }))
                })
                .child(text)
        };
        let d = d
            .child(label("Launch args (optional)", cx))
            .child(
                div()
                    .when(shell, |d| d.opacity(0.45))
                    .child(Input::new(&self.args)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(quick("np-resume", "--resume", cx))
                    .child(quick("np-continue", "-c", cx))
                    .child(hint("Continue the last conversation with -c.", cx)),
            );
        let d = if self.remote {
            d
        } else {
            self.worktree_option(d, cx)
        };
        d.child(
            toggle_row(
                "np-shell",
                "Open a plain shell instead",
                format!("Runs {} in the same folder.", self.shell_name),
                self.shell,
                cx,
            )
            .when(!self.accounts.is_empty(), |d| {
                d.on_click(cx.listener(|this, _, _, cx| {
                    this.shell = !this.shell;
                    cx.notify();
                }))
            }),
        )
        .child(label("Will run", cx))
        .child(
            div()
                .p_2()
                .rounded_md()
                .bg(theme.secondary)
                .text_xs()
                .font_family(crate::terminal::mono(cx))
                .child(self.will_run(cx)),
        )
    }

    /// "Work on a new branch (git worktree)" and the branch's name (#162).
    fn worktree_option(&self, d: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        let toggle = toggle_row(
            "np-worktree",
            "Work on a new branch (git worktree)",
            "A separate checkout, so edits don't collide with other panes in the repo.".into(),
            self.worktree,
            cx,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            if !this.worktree && this.branch.read(cx).value().trim().is_empty() {
                this.branch.update(cx, |s, cx| {
                    s.set_value(suggest_branch(chrono::Local::now()), window, cx)
                });
            }
            this.worktree = !this.worktree;
            this.error = None;
            // Shown now: its name is next, and Enter there starts.
            if this.worktree {
                window.focus(&this.branch.focus_handle(cx), cx);
            }
            cx.notify();
        }));
        d.child(toggle).when(self.worktree, |d| {
            d.child(label("New branch", cx))
                .child(Input::new(&self.branch))
                .child(hint(
                    format!(
                        "Branches off the folder's current commit, in ~/.wraithgrid/worktrees.{}",
                        if self.branch_from {
                            ""
                        } else {
                            " New panes in this project start this way until you turn it off."
                        }
                    ),
                    cx,
                ))
        })
    }
}

/// A row with a title, its explanation and a switch (`.trow`).
fn toggle_row(
    id: &'static str,
    title: &'static str,
    explain: String,
    on: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().clone();
    div()
        .id(id)
        .role(Role::Switch)
        .aria_label(title)
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .flex()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(explain),
                ),
        )
        .child(switch(on, &theme))
}

/// An on/off switch drawn like the Electron app's `.sw`.
fn switch(on: bool, theme: &gpui_kit::component::Theme) -> impl IntoElement {
    div()
        .w(px(30.))
        .h(px(18.))
        .flex_shrink_0()
        .p(px(2.))
        .rounded_full()
        .flex()
        .when(on, |d| d.justify_end())
        .bg(if on { theme.primary } else { theme.border })
        .child(div().size(px(14.)).rounded_full().bg(if on {
            theme.primary_foreground
        } else {
            theme.muted_foreground
        }))
}

// ---- Command palette -----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAction {
    FocusPane {
        workspace: String,
        pane: String,
    },
    Project(String),
    /// "Preview: open for <project>".
    Preview(String),
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
    /// The focused pane's branch (#162).
    SwitchBranch,
    BranchPane,
    OpenBranch,
    Shortcuts,
}

/// The palette's commands beyond `PaletteAction` (`CommandPalette.tsx`), sent as their own
/// event for the window to carry out.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteCommand {
    NewProject,
    /// Start a project's team, or stop it while its agents run (#79).
    Team {
        workspace: String,
        start: bool,
    },
    NewScheduledJob,
    ToggleSidebar,
    /// Switch light/dark theme (`dark`: to dark).
    SetTheme {
        dark: bool,
    },
    /// "New claude pane as <account>", in the most recent folder.
    NewPaneAs {
        account: String,
        cwd: String,
    },
}

/// What a palette row does.
#[derive(Clone, PartialEq)]
enum Does {
    Run(PaletteAction),
    Command(PaletteCommand),
    /// "Switch project…": the same list, projects only.
    SwitchProject,
}

pub enum PaletteEvent {
    Run(PaletteAction),
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Panes,
    Commands,
    Projects,
    Accounts,
}

impl Group {
    fn label(self) -> &'static str {
        match self {
            Group::Panes => "Panes",
            Group::Commands => "Commands",
            Group::Projects => "Projects",
            Group::Accounts => "Accounts",
        }
    }
}

struct Item {
    group: Group,
    label: String,
    detail: String,
    action: Does,
}

pub struct Palette {
    query: Entity<InputState>,
    items: Vec<Item>,
    selected: usize,
    /// "Switch project…": only projects are listed.
    projects_only: bool,
    scroll: ScrollHandle,
    focus: ModalFocus,
    _subscription: Subscription,
}

/// `matches` (lib/search.ts): every word of `query` appears in `text` (case-insensitive).
pub fn fuzzy_match(text: &str, query: &str) -> bool {
    let hay = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|w| hay.contains(w))
}

impl Palette {
    pub fn new(config: &Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut items = Vec::new();
        let cmd = |label: &str, action: Does| Item {
            group: Group::Commands,
            label: label.into(),
            detail: String::new(),
            action,
        };
        for w in &config.workspaces {
            for p in &w.panes {
                let account = p
                    .account_id
                    .as_deref()
                    .and_then(|id| config.account(id))
                    .map_or("shell".to_string(), |a| a.name.clone());
                items.push(Item {
                    group: Group::Panes,
                    label: if p.title.is_empty() {
                        "pane".into()
                    } else {
                        p.title.clone()
                    },
                    detail: format!("{account} · {}", w.name),
                    action: Does::Run(PaletteAction::FocusPane {
                        workspace: w.id.clone(),
                        pane: p.id.clone(),
                    }),
                });
            }
        }
        let dark = crate::theme::resolve(config, crate::theme::system_dark(window)).mode
            == wraithgrid_themes::ThemeName::Dark;
        items.push(cmd("New chat…", Does::Run(PaletteAction::NewPane)));
        items.push(cmd("New terminal", Does::Run(PaletteAction::NewTerminal)));
        items.push(cmd(
            "Show / hide the git diff",
            Does::Run(PaletteAction::Changes),
        ));
        for w in config.workspaces.iter().filter(|w| !w.team.is_empty()) {
            let on = w.panes.iter().any(|p| {
                w.team
                    .iter()
                    .any(|a| Some(&a.id) == p.team_agent_id.as_ref())
            });
            items.push(Item {
                group: Group::Commands,
                label: format!("{} team: {}", if on { "Stop" } else { "Start" }, w.name),
                detail: w
                    .team
                    .iter()
                    .map(|a| a.role.clone())
                    .collect::<Vec<_>>()
                    .join(" · "),
                action: Does::Command(PaletteCommand::Team {
                    workspace: w.id.clone(),
                    start: !on,
                }),
            });
        }
        items.push(cmd(
            "New scheduled job…",
            Does::Command(PaletteCommand::NewScheduledJob),
        ));
        items.push(cmd("Scheduled", Does::Run(PaletteAction::Scheduled)));
        let active = config.active();
        if active.machine_id.is_none() {
            items.push(cmd(
                &format!("Preview: open for {}", active.name),
                Does::Run(PaletteAction::Preview(active.id.clone())),
            ));
        }
        items.push(cmd(
            "New project…",
            Does::Command(PaletteCommand::NewProject),
        ));
        items.push(Item {
            group: Group::Commands,
            label: "Switch project…".into(),
            detail: String::new(),
            action: Does::SwitchProject,
        });
        items.push(cmd(
            if config.settings.sidebar_collapsed {
                "Expand sidebar"
            } else {
                "Collapse sidebar"
            },
            Does::Command(PaletteCommand::ToggleSidebar),
        ));
        items.push(cmd(
            if dark {
                "Switch to light theme"
            } else {
                "Switch to dark theme"
            },
            Does::Command(PaletteCommand::SetTheme { dark: !dark }),
        ));
        for (label, action) in [
            ("Zoom pane / back to grid", PaletteAction::ToggleZoom),
            ("Close pane", PaletteAction::ClosePane),
            ("Accounts", PaletteAction::Accounts),
            ("Usage", PaletteAction::Usage),
            ("Mods", PaletteAction::Mods),
            ("Settings", PaletteAction::Settings),
            ("Keyboard shortcuts", PaletteAction::Shortcuts),
        ] {
            items.push(cmd(label, Does::Run(action)));
        }
        for w in &config.workspaces {
            items.push(Item {
                group: Group::Projects,
                label: w.name.clone(),
                detail: format!(
                    "{} {}",
                    w.panes.len(),
                    if w.panes.len() == 1 { "pane" } else { "panes" }
                ),
                action: Does::Run(PaletteAction::Project(w.id.clone())),
            });
        }
        let cwd = config
            .recent_folders
            .first()
            .cloned()
            .unwrap_or_else(|| config.settings.default_cwd.clone());
        let home = home_dir().to_string_lossy().into_owned();
        for a in &config.accounts {
            items.push(Item {
                group: Group::Accounts,
                label: format!("New {} pane as {}", a.cli.as_str(), a.name),
                detail: format!("in {}", contract_home(&cwd, &home)),
                action: Does::Command(PaletteCommand::NewPaneAs {
                    account: a.id.clone(),
                    cwd: cwd.clone(),
                }),
            });
        }
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search panes, projects, accounts and commands")
        });
        let sub = cx.subscribe_in(&query, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                this.select(0, cx);
            }
        });
        window.focus(&query.focus_handle(cx), cx);
        Palette {
            query,
            items,
            selected: 0,
            projects_only: false,
            scroll: ScrollHandle::new(),
            focus: ModalFocus::child_first(),
            _subscription: sub,
        }
    }

    /// The focused pane's git commands, after Show changes (#162). `local`: worktrees are made
    /// on this computer only.
    pub fn add_git_commands(&mut self, branch: Option<String>, local: bool) {
        let at = self
            .items
            .iter()
            .position(|i| i.action == Does::Run(PaletteAction::Changes))
            .map_or(self.items.len(), |i| i + 1);
        let mut add = vec![(
            "Switch branch…",
            branch.unwrap_or_default(),
            PaletteAction::SwitchBranch,
        )];
        if local {
            add.push((
                "New pane on a new branch…",
                "its own folder (git worktree)".into(),
                PaletteAction::BranchPane,
            ));
            add.push((
                "Open a branch in a new pane…",
                String::new(),
                PaletteAction::OpenBranch,
            ));
        }
        for (n, (label, detail, action)) in add.into_iter().enumerate() {
            self.items.insert(
                at + n,
                Item {
                    group: Group::Commands,
                    label: label.into(),
                    detail,
                    action: Does::Run(action),
                },
            );
        }
    }

    fn matches(&self, cx: &App) -> Vec<usize> {
        let q = self.query.read(cx).value().to_string();
        (0..self.items.len())
            .filter(|&i| !self.projects_only || self.items[i].group == Group::Projects)
            .filter(|&i| {
                fuzzy_match(
                    &format!("{} {}", self.items[i].label, self.items[i].detail),
                    &q,
                )
            })
            .collect()
    }

    fn select(&mut self, n: usize, cx: &mut Context<Self>) {
        self.selected = n;
        self.scroll.scroll_to_item(n);
        cx.notify();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if let Some(&i) = self.matches(cx).get(self.selected) {
            self.run_item(i, cx);
        }
    }

    fn run_item(&mut self, i: usize, cx: &mut Context<Self>) {
        match self.items[i].action.clone() {
            Does::Run(action) => cx.emit(PaletteEvent::Run(action)),
            Does::Command(command) => cx.emit(command),
            Does::SwitchProject => {
                self.projects_only = true;
                self.select(0, cx);
            }
        }
    }
}

impl EventEmitter<PaletteEvent> for Palette {}
impl EventEmitter<PaletteCommand> for Palette {}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus.keep(window, cx);
        let theme = cx.theme().clone();
        let matches = self.matches(cx);
        let selected = self.selected.min(matches.len().saturating_sub(1));
        let rows = matches.iter().enumerate().map(|(n, &i)| {
            let item = &self.items[i];
            let head = (n == 0 || self.items[matches[n - 1]].group != item.group)
                .then(|| item.group.label());
            // One child per item (header and row), so the scroll handle's index is the item's.
            div()
                .flex()
                .flex_col()
                .when_some(head, |d, h| {
                    d.child(
                        div()
                            .px_3()
                            .pt_2()
                            .pb_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(h),
                    )
                })
                .child(
                    div()
                        .id(SharedString::from(format!("palette-{i}")))
                        .role(Role::ListBoxOption)
                        .aria_label(format!("{} {}", item.label, item.detail))
                        .aria_selected(n == selected)
                        .when(n == selected, |d| d.aria_active_descendant())
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .flex()
                        .gap_2()
                        .cursor_pointer()
                        .when(n == selected, |d| d.bg(theme.secondary))
                        .hover(|d| d.bg(theme.secondary))
                        .child(div().flex_1().truncate().child(item.label.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(item.detail.clone()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.run_item(i, cx))),
                )
        });
        let count = matches.len();
        let query = self.query.read(cx).value().to_string();
        div()
            .id("palette")
            .a11y_dialog("Search panes and commands")
            .track_focus(&focus)
            .w(px(600.))
            .max_h(px(480.))
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
                    "escape" if this.projects_only => {
                        this.projects_only = false;
                        this.select(0, cx);
                    }
                    "escape" => cx.emit(PaletteEvent::Cancel),
                    "enter" => this.run(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            // The search field takes up and down as actions (they never arrive as keys).
            .capture_action(cx.listener(move |this, _: &input::MoveDown, _, cx| {
                if count > 0 {
                    this.select((selected + 1).min(count - 1), cx);
                }
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(move |this, _: &input::MoveUp, _, cx| {
                if count > 0 {
                    this.select(selected.saturating_sub(1), cx);
                }
                cx.stop_propagation();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(self.projects_only, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Projects"),
                        )
                    })
                    .child(div().flex_1().child(Input::new(&self.query)))
                    .child(
                        div()
                            .px_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(theme.border)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Esc"),
                    ),
            )
            .child(
                div()
                    .id("palette-list")
                    .role(Role::ListBox)
                    .flex()
                    .flex_col()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .when(count == 0, |d| {
                        d.child(
                            div()
                                .p_3()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!("Nothing matches “{query}”.")),
                        )
                    })
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
    focus: ModalFocus,
}

impl QuitDialog {
    pub fn new(live: usize) -> Self {
        QuitDialog {
            live,
            remember: false,
            focus: ModalFocus::new(),
        }
    }
}

impl EventEmitter<QuitEvent> for QuitDialog {}

impl Render for QuitDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus.keep(window, cx);
        let theme = cx.theme().clone();
        let message = format!(
            "{} agent{} running",
            self.live,
            if self.live == 1 { " is" } else { "s are" }
        );
        frame("quit-dialog", message.clone(), 460., window, cx)
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => cx.emit(QuitEvent::Cancel),
                    // Enter takes the highlighted choice: keep running.
                    "enter" => cx.emit(QuitEvent::Choose {
                        keep: true,
                        remember: this.remember,
                    }),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(header(message, cx.listener(|_, _, _, cx| cx.emit(QuitEvent::Cancel)), cx))
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Keep them running in the background, and Wraithgrid shows them again, screen and all, next time it opens. Or stop them now.",
            ))
            .child(
                div()
                    .id("remember")
                    .role(Role::CheckBox)
                    .aria_label("Remember my choice (Settings → When the window closes)")
                    .aria_toggled(if self.remember { Toggled::True } else { Toggled::False })
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
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(div().flex_1().child(hint("Esc to cancel", cx)))
                    .child(
                        button("quit-cancel", "Cancel", false, true, cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(QuitEvent::Cancel))),
                    )
                    .child(button("quit-stop", "Stop agents and quit", false, true, cx).on_click(
                        cx.listener(|this, _, _, cx| {
                            cx.emit(QuitEvent::Choose {
                                keep: false,
                                remember: this.remember,
                            })
                        }),
                    ))
                    .child(button("quit-keep", "Keep running", true, true, cx).on_click(
                        cx.listener(|this, _, _, cx| {
                            cx.emit(QuitEvent::Choose {
                                keep: true,
                                remember: this.remember,
                            })
                        }),
                    )),
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
    /// The folder picker's pins changed: save them (`settings.pinnedFolders`).
    PinFolders(Vec<String>),
    Cancel,
}

/// New project, set to run on another machine ("Runs on"): the window makes the project,
/// sets its machine, then opens the first chat there if asked.
// The window reads it once it subscribes to this event (shell.rs follow-up).
#[allow(dead_code)]
pub struct CreateRemoteProject {
    pub name: String,
    pub folder: String,
    pub account: Option<String>,
    pub start_chat: bool,
    pub machine: String,
}

/// New project (`NewProjectDialog.tsx`): a folder, a name (the folder's by default), the
/// account its chats use, the machine it runs on, and whether to start a chat right away.
pub struct NewProjectDialog {
    folder: Entity<InputState>,
    name: Entity<InputState>,
    /// The last name filled in from the folder: typing your own stops the suggesting.
    suggested: String,
    accounts: Vec<AccountChoice>,
    account: Option<String>,
    machines: Vec<(String, String)>,
    machine: Option<String>,
    start_chat: bool,
    check: Option<FolderCheck>,
    _check: Option<Task<()>>,
    pub error: Option<String>,
    picking: Picking,
    focus: ModalFocus,
    _subscriptions: Vec<Subscription>,
}

impl NewProjectDialog {
    pub fn new(config: &Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let folder = cx.new(|cx| InputState::new(window, cx).placeholder("~/code/my-project"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. wraithgrid"));
        let subs = vec![
            cx.subscribe_in(
                &folder,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::Change = event {
                        this.error = None;
                        this.folder_changed(window, cx);
                    }
                },
            ),
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    this.error = None;
                    cx.notify();
                }
            }),
        ];
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
            suggested: String::new(),
            accounts: account_choices(config),
            account,
            machines: config
                .machines
                .iter()
                .map(|m| (m.id.clone(), m.name.clone()))
                .collect(),
            machine: None,
            start_chat: !config.accounts.is_empty(),
            check: None,
            _check: None,
            picking: Picking::new(config),
            error: None,
            focus: ModalFocus::child_first(),
            _subscriptions: subs,
        }
    }

    fn folder_text(&self, cx: &App) -> String {
        self.folder.read(cx).value().trim().to_string()
    }

    /// The name follows the folder until one is typed; a local folder is checked.
    fn folder_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let folder = self.folder_text(cx);
        let base = if folder.is_empty() {
            String::new()
        } else {
            wraithgrid_core::paths::base_name(&folder)
        };
        let typed = self.name.read(cx).value().trim().to_string();
        if !base.is_empty() && (typed.is_empty() || typed == self.suggested) {
            self.suggested = base.clone();
            self.name.update(cx, |n, cx| n.set_value(base, window, cx));
        }
        self.recheck(cx);
    }

    fn recheck(&mut self, cx: &mut Context<Self>) {
        let folder = self.folder_text(cx);
        if self.machine.is_some() || folder.is_empty() {
            self._check = None;
            self.check = None;
            return cx.notify();
        }
        self._check = Some(check_folder(folder, cx, |this: &mut Self, c, cx| {
            this.check = Some(c);
            cx.notify();
        }));
        cx.notify();
    }

    fn folder_check(&self, cx: &App) -> Option<&FolderCheck> {
        let folder = self.folder_text(cx);
        self.check.as_ref().filter(|c| c.folder == folder)
    }

    fn can_create(&self, cx: &App) -> bool {
        !self.name.read(cx).value().trim().is_empty()
            && !self.folder_text(cx).is_empty()
            && self.folder_check(cx).is_none_or(|c| c.error.is_none())
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let folder = self.folder_text(cx);
        if name.is_empty() {
            return self.fail("Give the project a name.".into(), cx);
        }
        if folder.is_empty() {
            return self.fail("Pick the project’s folder.".into(), cx);
        }
        if let Some(e) = self.folder_check(cx).and_then(|c| c.error.clone()) {
            return self.fail(e, cx);
        }
        let (account, start_chat) = (
            self.account.clone(),
            self.start_chat && self.account.is_some(),
        );
        match self.machine.clone() {
            None => cx.emit(NewProjectEvent::Create {
                name,
                folder,
                account,
                start_chat,
            }),
            Some(machine) => cx.emit(CreateRemoteProject {
                name,
                folder,
                account,
                start_chat,
                machine,
            }),
        }
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }
}

impl EventEmitter<NewProjectEvent> for NewProjectDialog {}
impl EventEmitter<CreateRemoteProject> for NewProjectDialog {}
impl EventEmitter<OpenAccounts> for NewProjectDialog {}

impl Render for NewProjectDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.picking.picker.clone() {
            self.focus.refocus_child();
            return picker.into_any_element();
        }
        let focus = self.focus.keep(window, cx);
        let theme = cx.theme().clone();
        let seg = |id: SharedString, text: String, on: bool, enabled: bool| {
            div()
                .id(id)
                .role(Role::RadioButton)
                .aria_label(text.clone())
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .border_1()
                .border_color(if on { theme.ring } else { theme.border })
                .when(on, |d| d.bg(theme.secondary))
                .when(enabled, |d| {
                    d.cursor_pointer().hover(|d| d.bg(theme.secondary))
                })
                .when(!enabled, |d| d.opacity(0.45))
                .child(text)
        };
        let accounts = self.accounts.iter().map(|a| {
            let pick = a.id.clone();
            account_chip(a, self.account.as_ref() == Some(&a.id), true, cx).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.account = Some(pick.clone());
                    cx.notify();
                },
            ))
        });
        let check = self.folder_check(cx).cloned();
        let can_create = self.can_create(cx);
        let has_account = self.account.is_some();
        let start_on = self.start_chat && has_account;
        let mut machines = vec![seg(
            "npj-local".into(),
            "This computer".into(),
            self.machine.is_none(),
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.machine = None;
            this.recheck(cx);
        }))];
        for (id, name) in &self.machines {
            let pick = id.clone();
            machines.push(
                seg(
                    SharedString::from(format!("npj-m-{id}")),
                    name.clone(),
                    self.machine.as_ref() == Some(id),
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.machine = Some(pick.clone());
                    this.recheck(cx);
                })),
            );
        }
        frame("new-project-dialog", "New project", 540., window, cx)
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => cx.emit(NewProjectEvent::Cancel),
                    "enter" => this.create(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(header(
                "New project",
                cx.listener(|_, _, _, cx| cx.emit(NewProjectEvent::Cancel)),
                cx,
            ))
            .child(label("Folder", cx))
            .child(folder_row(
                &self.folder,
                check.as_ref().and_then(|c| c.branch.clone()),
                cx.listener(|this, _, window, cx| {
                    let folder = this.folder.clone();
                    Picking::open(
                        &folder,
                        |d: &mut Self| &mut d.picking,
                        |pins, cx| cx.emit(NewProjectEvent::PinFolders(pins)),
                        this,
                        window,
                        cx,
                    )
                }),
                cx,
            ))
            .when_some(check.and_then(|c| c.error), |d, e| {
                d.child(error_line("npj-folder-error", e, cx))
            })
            .when(self.folder_text(cx).is_empty(), |d| {
                d.child(hint("New chats in the project start there.", cx))
            })
            .child(label("Name", cx))
            .child(Input::new(&self.name))
            .map(|d| {
                if self.accounts.is_empty() {
                    d.child(
                        div()
                            .p_2()
                            .rounded_md()
                            .bg(theme.secondary)
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child("No accounts yet; the project can still be made.")
                            .child(
                                button("npj-add-account", "Add account", false, true, cx).on_click(
                                    cx.listener(|_, _, _, cx| {
                                        cx.emit(OpenAccounts { import: false })
                                    }),
                                ),
                            ),
                    )
                } else {
                    d.child(label("Account", cx))
                        .child(div().flex().flex_wrap().gap_2().children(accounts))
                        .child(hint("The default account for the project’s chats.", cx))
                }
            })
            .when(!self.machines.is_empty(), |d| {
                d.child(label("Runs on", cx))
                    .child(div().flex().flex_wrap().gap_2().children(machines))
            })
            .child(label("Start with", cx))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        seg(
                            "npj-start".into(),
                            "The first chat open".into(),
                            start_on,
                            has_account,
                        )
                        .when(!has_account, |d| {
                            d.tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new("Needs an account")
                                    .build(window, cx)
                            })
                        })
                        .when(has_account, |d| {
                            d.on_click(cx.listener(|this, _, _, cx| {
                                this.start_chat = true;
                                cx.notify();
                            }))
                        }),
                    )
                    .child(
                        seg(
                            "npj-empty".into(),
                            "An empty project".into(),
                            !start_on,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.start_chat = false;
                            cx.notify();
                        })),
                    ),
            )
            .when_some(self.error.clone(), |d, e| {
                d.child(error_line("npj-error", e, cx))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(hint("Enter to create · Esc to cancel", cx)),
                    )
                    .child(
                        button("npj-cancel", "Cancel", false, true, cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NewProjectEvent::Cancel))),
                    )
                    .child(
                        button("npj-create", "Create project", true, can_create, cx)
                            .when(can_create, |d| {
                                d.on_click(cx.listener(|this, _, _, cx| this.create(cx)))
                            }),
                    ),
            )
            .into_any_element()
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
    /// The other projects' names, lowercased: a name must be its own.
    taken: Vec<String>,
    /// It runs on another machine: its folder is not checked here.
    remote: bool,
    can_delete: bool,
    confirm_delete: bool,
    panes: usize,
    error: Option<String>,
    check: Option<FolderCheck>,
    _check: Option<Task<()>>,
    picking: Picking,
    focus: ModalFocus,
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
        let subs = vec![
            cx.subscribe_in(&name, window, |this, _, e: &InputEvent, _, cx| {
                if let InputEvent::Change = e {
                    this.error = None;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&folder, window, |this, _, e: &InputEvent, _, cx| {
                if let InputEvent::Change = e {
                    this.error = None;
                    this.recheck(cx);
                }
            }),
        ];
        window.focus(&name.focus_handle(cx), cx);
        let mut dialog = ProjectDialog {
            id: id.into(),
            name,
            folder,
            accounts: config
                .accounts
                .iter()
                .map(|a| (a.id.clone(), a.name.clone()))
                .collect(),
            account: ws.account_id.clone(),
            taken: config
                .workspaces
                .iter()
                .filter(|w| w.id != id)
                .map(|w| w.name.to_lowercase())
                .collect(),
            remote: ws.machine_id.is_some(),
            can_delete: config.workspaces.len() > 1,
            confirm_delete: false,
            panes: ws.panes.len(),
            error: None,
            check: None,
            _check: None,
            picking: Picking::new(config),
            focus: ModalFocus::child_first(),
            _subscriptions: subs,
        };
        dialog.recheck(cx);
        Some(dialog)
    }

    fn folder_text(&self, cx: &App) -> String {
        self.folder.read(cx).value().trim().to_string()
    }

    fn recheck(&mut self, cx: &mut Context<Self>) {
        let folder = self.folder_text(cx);
        if self.remote || folder.is_empty() {
            self._check = None;
            self.check = None;
            return cx.notify();
        }
        self._check = Some(check_folder(folder, cx, |this: &mut Self, c, cx| {
            this.check = Some(c);
            cx.notify();
        }));
        cx.notify();
    }

    fn folder_check(&self, cx: &App) -> Option<&FolderCheck> {
        let folder = self.folder_text(cx);
        self.check.as_ref().filter(|c| c.folder == folder)
    }

    /// Why the typed values can't be saved, if they can't.
    fn problem(&self, cx: &App) -> Option<String> {
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return Some("Give the project a name.".into());
        }
        if self.taken.contains(&name.to_lowercase()) {
            return Some(format!("A project named {name} exists."));
        }
        self.folder_check(cx).and_then(|c| c.error.clone())
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self.problem(cx) {
            self.error = Some(e);
            return cx.notify();
        }
        cx.emit(ProjectEvent::Save {
            id: self.id.clone(),
            name: self.name.read(cx).value().to_string(),
            folder: self.folder.read(cx).value().to_string(),
            account: self.account.clone(),
        });
    }
}

impl EventEmitter<ProjectEvent> for ProjectDialog {}
impl EventEmitter<PinFolders> for ProjectDialog {}

impl Render for ProjectDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.picking.picker.clone() {
            self.focus.refocus_child();
            return picker.into_any_element();
        }
        let focus = self.focus.keep(window, cx);
        let theme = cx.theme().clone();
        let chip = |id: SharedString, label: String, on: bool| {
            div()
                .id(id)
                .a11y_toggle(label.clone(), on)
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .text_sm()
                .cursor_pointer()
                .border_color(if on { theme.ring } else { theme.border })
                .when(on, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
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
        let delete_note = format!(
            "Delete this project? Its {} pane{} stop.",
            self.panes,
            if self.panes == 1 { "" } else { "s" }
        );
        let check = self.folder_check(cx).cloned();
        let problem = self.problem(cx);
        let can_save = problem.is_none();
        // A typed name problem shows as you type; the folder's once checked.
        let shown = self.error.clone().or(problem);
        frame("project-dialog", "Project", 500., window, cx)
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => cx.emit(ProjectEvent::Cancel),
                    "enter" => this.save(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(header(
                "Project",
                cx.listener(|_, _, _, cx| cx.emit(ProjectEvent::Cancel)),
                cx,
            ))
            .child(label("Name", cx))
            .child(Input::new(&self.name))
            .child(label("Folder (where its new chats start)", cx))
            .child(folder_row(
                &self.folder,
                check.and_then(|c| c.branch),
                cx.listener(|this, _, window, cx| {
                    let folder = this.folder.clone();
                    Picking::open(
                        &folder,
                        |d: &mut Self| &mut d.picking,
                        |pins, cx| cx.emit(PinFolders(pins)),
                        this,
                        window,
                        cx,
                    )
                }),
                cx,
            ))
            .when(self.remote, |d| {
                d.child(hint(
                    "This project runs on another machine: the folder is there.",
                    cx,
                ))
            })
            .child(label("Account for its chats", cx))
            .child(div().flex().flex_wrap().gap_2().children(accounts))
            .when_some(shown, |d, e| d.child(error_line("pj-error", e, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(self.can_delete && !self.confirm_delete, |d| {
                        d.child(
                            button("pj-delete", "Delete…", false, true, cx)
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
                                button("pj-really", "Delete", false, true, cx)
                                    .text_color(theme.danger)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(ProjectEvent::Delete { id: id.clone() });
                                    })),
                            )
                    })
                    .child(div().flex_1())
                    .child(
                        button("pj-cancel", "Cancel", false, true, cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ProjectEvent::Cancel))),
                    )
                    .child(
                        button("pj-save", "Save", true, can_save, cx).when(can_save, |d| {
                            d.on_click(cx.listener(|this, _, _, cx| this.save(cx)))
                        }),
                    ),
            )
            .into_any_element()
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
    /// The answer arrows move to and Enter picks.
    selected: usize,
    focus: ModalFocus,
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
            selected: 0,
            focus: ModalFocus::new(),
        }
    }
}

impl EventEmitter<ChoiceEvent> for ChoiceDialog {}

impl Render for ChoiceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus.keep(window, cx);
        let theme = cx.theme().clone();
        let count = self.choices.len();
        let choices = self
            .choices
            .iter()
            .enumerate()
            .map(|(n, (id, label, detail))| {
                let pick = id.clone();
                let on = n == self.selected;
                div()
                    .id(SharedString::from(format!("choice-{id}")))
                    .a11y_button(format!("{label} {detail}"))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(if on { theme.ring } else { theme.border })
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
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(ChoiceEvent::Pick(pick.clone()))),
                    )
            });
        frame("choice-dialog", self.title.clone(), 460., window, cx)
            .track_focus(&focus)
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => cx.emit(ChoiceEvent::Cancel),
                    "down" | "tab" if count > 0 => {
                        this.selected = (this.selected + 1) % count;
                        cx.notify();
                    }
                    "up" if count > 0 => {
                        this.selected = (this.selected + count - 1) % count;
                        cx.notify();
                    }
                    "enter" => match this.choices.get(this.selected) {
                        Some((id, _, _)) => cx.emit(ChoiceEvent::Pick(id.clone())),
                        None => cx.emit(ChoiceEvent::Cancel),
                    },
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(header(
                self.title.clone(),
                cx.listener(|_, _, _, cx| cx.emit(ChoiceEvent::Cancel)),
                cx,
            ))
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
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(hint("Enter to choose · Esc to cancel", cx)),
                    )
                    .child(
                        button("choice-cancel", self.cancel.clone(), false, true, cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChoiceEvent::Cancel))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fuzzy() {
        assert!(super::fuzzy_match("WraithGrid · org-info", "grid org"));
        assert!(!super::fuzzy_match("WraithGrid · org-info", "wgo"));
        assert!(super::fuzzy_match("Close pane", "close p"));
        // Words, not scattered letters: "site" isn't in "Wraithgrid Website" as a word part.
        assert!(super::fuzzy_match(
            "Preview: open for Site test",
            "open for site"
        ));
        assert!(!super::fuzzy_match(
            "Preview: open for Wraithgrid Website",
            "for site test"
        ));
        assert!(!super::fuzzy_match("shell", "zsh"));
        assert!(super::fuzzy_match("anything", ""));
    }

    #[test]
    fn suggests_a_task_branch_from_the_date_and_time() {
        use chrono::TimeZone;
        let at = chrono::Utc.with_ymd_and_hms(2026, 11, 5, 9, 42, 0).unwrap();
        assert_eq!(super::suggest_branch(at), "task-1105-0942");
        assert!(wraithgrid_git::valid_branch_name(&super::suggest_branch(
            chrono::Local::now()
        )));
    }
}
