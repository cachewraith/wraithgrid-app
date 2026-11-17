//! The top bar's Git button (`GitButton.tsx`, #162) and the window's side of the git flows
//! (`store.ts`): the focused pane's branch and changed count, and its menu (Show changes,
//! Commit… / Pull / Push asked in the Changes panel, Switch branch…, a new pane on a new or an
//! existing branch); the pane header's branch chip; the palette's git commands; and, when the
//! last pane on a Wraithgrid worktree closes, whether to remove its folder.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::config::{Pane, Workspace};
use wraithgrid_core::paths::{home_dir, is_strictly_inside, resolve_user_path, worktrees_root};
use wraithgrid_core::types::{PaneView, ShortcutId};
use wraithgrid_git::{
    check_worktree, git_status, remove_worktree, GitCli, GitStatus, WorktreeCheck,
};

use super::icons::{small, z, Glyph};
use super::menu::MenuItem;
use super::sidebar::pal;
use super::{bounds_probe, Modal, Shell, TITLE_BAR};
use crate::a11y::A11y as _;
use crate::branch_picker::{BranchEvent, BranchPicker, PickerMode};
use crate::changes::{ChangesPanel, GitAsk};
use crate::dialogs::{Palette, PaletteAction};

const MENU_W: f32 = 270.0;

/// The Git menu, open for a pane at a point of the window.
pub(super) struct GitMenu {
    pane: String,
    at: Point<Pixels>,
}

#[derive(Clone, Copy)]
pub(super) enum MenuAction {
    Changes,
    Ask(GitAsk),
    Switch,
    NewBranchPane,
    OpenBranch,
}

/// A folder Wraithgrid made with "Work on a new branch" (saved home-relative).
fn is_worktree_cwd(cwd: &str) -> bool {
    cwd.replace('\\', "/")
        .starts_with("~/.wraithgrid/worktrees/")
}

/// Each folder's branch and changed count, one git call per folder.
fn git_heads(folders: Vec<String>) -> HashMap<String, GitStatus> {
    let git = GitCli::default();
    let home = home_dir();
    folders
        .into_iter()
        .map(|f| {
            let st = git_status(&git, &resolve_user_path(&f, &home));
            (f, st)
        })
        .collect()
}

impl Shell {
    fn pane_by_id(&self, id: &str) -> Option<(&Workspace, &Pane)> {
        self.config()
            .workspaces
            .iter()
            .find_map(|w| w.panes.iter().find(|p| p.id == id).map(|p| (w, p)))
    }

    /// The focused pane with its git status, when it is in a repo; `local`: worktrees are
    /// made on this computer only, not for a pane on an ssh machine.
    fn focused_git(&self) -> Option<(String, GitStatus, bool)> {
        let ws = self.config().active();
        let pane = self
            .focused
            .as_ref()
            .and_then(|f| ws.panes.iter().find(|p| &p.id == f))?;
        let git = self.git_heads.get(&pane.cwd).filter(|g| g.repo)?;
        Some((pane.id.clone(), git.clone(), ws.machine_id.is_none()))
    }

    /// The branch and changed count in the top row (`main · 3`), hidden outside a repo or with
    /// no pane focused; a click opens the Git menu under it.
    pub(super) fn git_button(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (pane, git, _) = self.focused_git()?;
        let p = pal(cx);
        let branch = git.branch.clone().unwrap_or_else(|| "detached".into());
        let viewport = window.viewport_size();
        let open = self.git_menu.as_ref().is_some_and(|m| m.pane == pane);
        let menu = self
            .git_menu
            .as_ref()
            .filter(|m| m.pane == pane)
            .map(|m| (m.pane.clone(), m.at));
        let menu = menu.map(|(pane, at)| {
            let items = self.git_menu_items(&pane, &git, cx);
            let card = self.menu_card(
                "git-menu",
                "Git",
                items,
                Some(MENU_W),
                |this, (pane, action), window, cx| {
                    this.close_menus(cx);
                    this.run_git_menu(pane, action, window, cx);
                },
                window,
                cx,
            );
            anchored()
                .position(at)
                .snap_to_window_with_margin(px(4.))
                .child(card)
        });
        let tip = [
            Some(branch.clone()),
            (git.changed > 0).then(|| format!("{} changed", git.changed)),
            (git.ahead > 0).then(|| format!("{} ahead", git.ahead)),
            (git.behind > 0).then(|| format!("{} behind", git.behind)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let label = if git.changed > 0 {
            format!("Git: {branch}, {} changed", git.changed)
        } else {
            format!("Git: {branch}")
        };
        let bounds = self.git_bounds.clone();
        Some(
            div()
                .id("git-button")
                .a11y_toggle(label, open)
                .occlude()
                .flex_shrink(1.)
                .min_w(px(0.))
                .max_w(z(260.))
                .h(z(26.))
                .px(z(9.))
                .flex()
                .items_center()
                .gap(z(6.))
                .rounded(z(7.))
                .border_1()
                .border_color(p.line)
                .text_size(z(12.))
                .text_color(if open { p.tx } else { p.mu })
                .cursor_pointer()
                .when(open, |d| d.bg(p.raise))
                .hover(|s| s.bg(p.raise).text_color(p.tx))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(bounds_probe(self.git_bounds.clone()))
                .child(small(Glyph::Branch, p.mu, p.bg))
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .font_family(crate::terminal::mono(cx))
                        .child(branch),
                )
                .when(git.changed > 0, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .text_color(p.warn)
                            .child(format!("· {}", git.changed)),
                    )
                })
                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                    if open {
                        this.close_menus(cx);
                        return;
                    }
                    this.close_menus(cx);
                    let b = *bounds.borrow();
                    let (left, bottom) = b
                        .map_or((e.position().x - px(12.), e.position().y + px(14.)), |b| {
                            (b.left(), b.bottom())
                        });
                    let x = left
                        .min(
                            window.viewport_size().width
                                - z(MENU_W + 8.).to_pixels(window.rem_size()),
                        )
                        .max(px(8.));
                    this.git_menu = Some(GitMenu {
                        pane: pane.clone(),
                        at: point(x, bottom + px(6.)),
                    });
                    this.menu_opened(e.is_keyboard());
                    cx.notify();
                }))
                .when_some(menu, |d, menu| {
                    // A scrim over the window closes the menu on any click outside it.
                    d.child(
                        deferred(
                            anchored().position(point(px(0.), px(0.))).child(
                                div()
                                    .id("git-scrim")
                                    .occlude()
                                    .w(viewport.width)
                                    .h(viewport.height)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.close_menus(cx);
                                            cx.stop_propagation();
                                        }),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(|this, _, _, cx| {
                                            this.close_menus(cx);
                                        }),
                                    )
                                    .child(menu),
                            ),
                        )
                        .with_priority(3),
                    )
                })
                .into_any_element(),
        )
    }

    fn git_menu_items(
        &self,
        pane: &str,
        git: &GitStatus,
        cx: &App,
    ) -> Vec<MenuItem<(String, MenuAction)>> {
        let p = pal(cx);
        let local = self
            .pane_by_id(pane)
            .is_some_and(|(w, _)| w.machine_id.is_none());
        let n = |k: usize, what: &str| {
            if k > 0 {
                format!(" ({k} {what})")
            } else {
                String::new()
            }
        };
        let item =
            |label: String, action: MenuAction| MenuItem::new(label, (pane.to_string(), action));
        let icon = |g: Glyph| small(g, p.mu, p.panel);
        let has_branch = git.branch.is_some();
        let mut items = vec![
            item("Show changes".into(), MenuAction::Changes)
                .icon(icon(Glyph::Diff))
                .kbd(self.hint(ShortcutId::Diff))
                .sub(if git.changed > 0 {
                    format!("{} changed", git.changed)
                } else {
                    "nothing changed".into()
                }),
            MenuItem {
                disabled: git.changed == 0,
                ..item("Commit…".into(), MenuAction::Ask(GitAsk::Commit))
            },
            MenuItem {
                disabled: !has_branch,
                ..item(
                    format!("Pull{}", n(git.behind, "behind")),
                    MenuAction::Ask(GitAsk::Pull),
                )
            },
            MenuItem {
                disabled: !has_branch,
                ..item(
                    format!("Push{}", n(git.ahead, "ahead")),
                    MenuAction::Ask(GitAsk::Push),
                )
            },
            MenuItem {
                divider: true,
                ..item("Switch branch…".into(), MenuAction::Switch).icon(icon(Glyph::Branch))
            },
        ];
        if local {
            items.push(
                item(
                    "New pane on a new branch…".into(),
                    MenuAction::NewBranchPane,
                )
                .icon(icon(Glyph::Plus))
                .sub("its own folder (git worktree)"),
            );
            items.push(
                item(
                    "Open a branch in a new pane…".into(),
                    MenuAction::OpenBranch,
                )
                .icon(icon(Glyph::Plus)),
            );
        }
        items
    }

    fn run_git_menu(
        &mut self,
        pane: String,
        action: MenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            MenuAction::Changes => {
                self.show_changes(&pane, window, cx);
            }
            // Commit / pull / push only open the panel, with its own confirm (`askGit`).
            MenuAction::Ask(what) => {
                if let Some(panel) = self.show_changes(&pane, window, cx) {
                    panel.update(cx, |p, cx| p.ask(what, window, cx));
                }
            }
            MenuAction::Switch => self.open_branches(pane, PickerMode::Switch, window, cx),
            MenuAction::OpenBranch => self.open_branches(pane, PickerMode::Open, window, cx),
            MenuAction::NewBranchPane => self.open_new_pane_on_branch(pane, window, cx),
        }
        cx.notify();
    }

    /// Opens the Changes panel on the pane's folder (`toggleDiff(true)`), focusing the pane.
    pub(super) fn show_changes(
        &mut self,
        pane_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ChangesPanel>> {
        if self.blocked.is_some() {
            return None;
        }
        let cwd = resolve_user_path(&self.pane_by_id(pane_id)?.1.cwd, &home_dir());
        self.focused = Some(pane_id.to_string());
        if let Some(panel) = self.changes.clone().filter(|p| p.read(cx).cwd() == cwd) {
            return Some(panel);
        }
        let panel = cx.new(|cx| ChangesPanel::new(cwd, window, cx));
        self.changes = Some(panel.clone());
        cx.notify();
        Some(panel)
    }

    /// The branch picker for a pane's folder: switch it, or open a branch in a new pane.
    fn open_branches(
        &mut self,
        pane_id: String,
        mode: PickerMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, pane)) = self.pane_by_id(&pane_id) else {
            return;
        };
        let cwd = pane.cwd.clone();
        // Panes elsewhere on the same folder switch with it; the picker says so first.
        let sharing = self
            .config()
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .filter(|p| p.id != pane_id && p.cwd == cwd)
            .count();
        let running = self.states.get(&pane_id).is_some_and(|s| s == "running")
            || matches!(
                self.chat_states.get(&pane_id),
                Some(wraithgrid_chat::ChatStatus::Running)
            );
        let dir = resolve_user_path(&cwd, &home_dir());
        let picker = cx.new(|cx| BranchPicker::new(dir, mode, sharing, running, window, cx));
        cx.subscribe_in(
            &picker,
            window,
            move |this, _, event: &BranchEvent, window, cx| {
                if let BranchEvent::Opened(dir) = event {
                    this.open_branch_pane(&pane_id, dir.clone(), cx);
                }
                if !matches!(event, BranchEvent::Cancel) {
                    this.refresh_git(cx);
                }
                this.close_modal(window, cx);
            },
        )
        .detach();
        self.modal = Some(Modal::Branches(picker));
        cx.notify();
    }

    /// New pane on a new branch…: the New pane dialog set up for that pane (`branchFrom`).
    fn open_new_pane_on_branch(
        &mut self,
        pane_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.pane_by_id(&pane_id).map(|(_, p)| p.clone()) else {
            return;
        };
        // The new pane goes next to it.
        self.focused = Some(pane_id);
        self.open_new_pane(window, cx);
        if let Some(Modal::NewPane(dialog)) = &self.modal {
            let config = self.config().clone();
            dialog.update(cx, |d, cx| d.branch_from(&pane, &config, window, cx));
        }
    }

    /// A pane like `base` (account, view) on the worktree at `cwd` (`openBranchPane`).
    fn open_branch_pane(&mut self, base_id: &str, cwd: String, cx: &mut Context<Self>) {
        let Some((ws, base)) = self
            .pane_by_id(base_id)
            .map(|(w, p)| (w.clone(), p.clone()))
        else {
            return;
        };
        let c = self.config();
        let account = if base.shell {
            None
        } else {
            [
                base.account_id.as_ref(),
                ws.account_id.as_ref(),
                c.settings.default_account_id.as_ref(),
            ]
            .into_iter()
            .flatten()
            .find(|id| c.account(id).is_some())
            .or(c.accounts.first().map(|a| &a.id))
            .cloned()
        };
        let shell = base.shell || account.is_none();
        self.focused = Some(base.id.clone());
        let chat = base.view == PaneView::Chat;
        let id = self.add_pane(Some(ws.id), account, cwd, vec![], shell, chat, cx);
        self.focused = Some(id);
    }

    /// Every pane on a switched folder shows its new branch now, not at the next poll.
    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        let mut folders: Vec<String> = self
            .config()
            .active()
            .panes
            .iter()
            .map(|p| p.cwd.clone())
            .collect();
        folders.sort();
        folders.dedup();
        cx.spawn(async move |this, cx| {
            let heads = cx.background_spawn(async move { git_heads(folders) }).await;
            let _ = this.update(cx, |this, cx| {
                this.git_heads.extend(heads);
                cx.notify();
            });
        })
        .detach();
    }

    /// The New pane dialog's worktree switch, remembered per project (`worktreePanes`).
    pub(super) fn remember_worktree_panes(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.config().active().worktree_panes == on {
            return;
        }
        let id = self.config().active_workspace.clone();
        self.update(cx, |c| {
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == id) {
                w.worktree_panes = on;
            }
        });
    }

    /// Switch branch… and the worktree commands, when the focused pane is in a repo.
    pub(super) fn add_git_commands(&self, palette: &Entity<Palette>, cx: &mut Context<Self>) {
        if let Some((_, git, local)) = self.focused_git() {
            palette.update(cx, |p, _| p.add_git_commands(git.branch, local));
        }
    }

    /// The palette's git commands, on the focused pane.
    pub(super) fn palette_git(
        &mut self,
        action: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane, _, _)) = self.focused_git() else {
            return self.close_modal(window, cx);
        };
        match action {
            PaletteAction::BranchPane => self.open_new_pane_on_branch(pane, window, cx),
            PaletteAction::OpenBranch => self.open_branches(pane, PickerMode::Open, window, cx),
            _ => self.open_branches(pane, PickerMode::Switch, window, cx),
        }
    }

    /// A terminal pane header's branch and changed count (`gitc`), hidden outside a repo:
    /// click shows its changes, right-click switches its branch; `compact` drops the name.
    pub(super) fn branch_chip(
        &self,
        pane: &Pane,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let g = self.git_heads.get(&pane.cwd).filter(|g| g.repo)?;
        let p = pal(cx);
        let branch = g.branch.clone().unwrap_or_else(|| "detached".into());
        let tip = format!(
            "{}{}{} — show changes ({}); right-click to switch branch",
            g.branch.clone().unwrap_or_else(|| "detached HEAD".into()),
            if g.changed > 0 {
                format!(" · {} changed", g.changed)
            } else {
                String::new()
            },
            if g.ahead > 0 {
                format!(" · {} ahead", g.ahead)
            } else {
                String::new()
            },
            self.hint(ShortcutId::Diff)
        );
        let (show, switch) = (pane.id.clone(), pane.id.clone());
        Some(
            div()
                .id(SharedString::from(format!("branch-chip-{}", pane.id)))
                .a11y_button(format!(
                    "Branch {branch}, {} changed files. Show changes",
                    g.changed
                ))
                .flex_shrink(1.)
                .min_w(px(0.))
                .max_w(relative(0.34))
                .h(z(22.))
                .px(z(7.))
                .flex()
                .items_center()
                .gap(z(5.))
                .rounded(z(6.))
                .cursor_pointer()
                .text_size(z(11.5))
                .text_color(p.mu)
                .hover(|s| s.bg(p.raise).text_color(p.tx))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(small(Glyph::Branch, p.mu, p.bg))
                .when(!compact, |d| {
                    d.child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .font_family(crate::terminal::mono(cx))
                            .child(branch),
                    )
                })
                .when(g.changed > 0, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .text_color(p.warn)
                            .child(g.changed.to_string()),
                    )
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.show_changes(&show, window, cx);
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, window, cx| {
                        // Not also the pane's menu, which the header opens on right-click.
                        cx.stop_propagation();
                        this.open_branches(switch.clone(), PickerMode::Switch, window, cx)
                    }),
                )
                .into_any_element(),
        )
    }

    /// The folder and title of a pane about to close, for `offer_remove_worktree`.
    pub(super) fn closing_pane(&self, pane_id: &str) -> Option<(String, String)> {
        self.pane_by_id(pane_id)
            .map(|(_, p)| (p.cwd.clone(), p.title.clone()))
    }

    /// The last pane on a Wraithgrid worktree closed: offer to remove its folder.
    pub(super) fn offer_remove_worktree(
        &mut self,
        closed: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let Some((cwd, title)) = closed else { return };
        let used = self
            .config()
            .workspaces
            .iter()
            .any(|w| w.panes.iter().any(|p| p.cwd == cwd));
        if !is_worktree_cwd(&cwd) || used || self.modal.is_some() {
            return;
        }
        let dialog = cx.new(|cx| RemoveWorktreeDialog::new(cwd, title, cx));
        cx.subscribe(&dialog, |this, _, _: &RemoveWorktreeEvent, cx| {
            this.modal = None;
            cx.notify();
        })
        .detach();
        self.modal = Some(Modal::RemoveWorktree(dialog));
        cx.notify();
    }

    /// Debug builds: `WRAITHGRID_OPEN=git-menu|git-branches|git-open|git-new-pane|git-commit|
    /// git-push` opens that part of the Git flow once the first git poll is in.
    pub(super) fn open_git_from_env(
        &mut self,
        hook: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hook = hook.to_string();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some((pane, _, _)) = this.focused_git() else {
                    return;
                };
                let action = match hook.as_str() {
                    "git-menu" => {
                        this.git_menu = Some(GitMenu {
                            pane,
                            at: point(px(260.), px(TITLE_BAR + 4.)),
                        });
                        return cx.notify();
                    }
                    "git-branches" => MenuAction::Switch,
                    "git-open" => MenuAction::OpenBranch,
                    "git-new-pane" => MenuAction::NewBranchPane,
                    "git-commit" => MenuAction::Ask(GitAsk::Commit),
                    "git-push" => MenuAction::Ask(GitAsk::Push),
                    _ => return,
                };
                this.run_git_menu(pane, action, window, cx);
            });
        })
        .detach();
    }
}

// ---- Remove a worktree ---------------------------------------------------------------------

pub(super) enum RemoveWorktreeEvent {
    Close,
}

/// Asked after closing the last pane on a Wraithgrid worktree (`RemoveWorktreeDialog.tsx`):
/// keep its folder, or remove it. The branch and its commits always stay; uncommitted files are
/// named before they are lost.
pub(super) struct RemoveWorktreeDialog {
    cwd: String,
    title: String,
    /// The folder's real path and what is in it, once checked.
    check: Option<Result<(PathBuf, WorktreeCheck), String>>,
    error: Option<String>,
    busy: bool,
}

/// A Wraithgrid worktree folder: its real path under ~/.wraithgrid/worktrees, else None.
fn own_worktree(cwd: &str) -> Option<PathBuf> {
    let home = home_dir();
    let dir = std::fs::canonicalize(resolve_user_path(cwd, &home)).ok()?;
    let root = std::fs::canonicalize(worktrees_root(&home)).ok()?;
    is_strictly_inside(&dir, &root).then_some(dir)
}

impl RemoveWorktreeDialog {
    fn new(cwd: String, title: String, cx: &mut Context<Self>) -> Self {
        let folder = cwd.clone();
        cx.spawn(async move |this, cx| {
            let check = cx
                .background_spawn(async move {
                    let dir = own_worktree(&folder).ok_or("Not a Wraithgrid worktree.")?;
                    check_worktree(&GitCli::default(), &dir).map(|c| (dir, c))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.check = Some(check);
                cx.notify();
            });
        })
        .detach();
        RemoveWorktreeDialog {
            cwd,
            title,
            check: None,
            error: None,
            busy: false,
        }
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let Some(Ok((dir, check))) = &self.check else {
            return;
        };
        if self.busy {
            return;
        }
        let (dir, force) = (dir.clone(), check.changed > 0);
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let removed = cx
                .background_spawn(async move { remove_worktree(&GitCli::default(), &dir, force) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match removed {
                    Ok(_) => cx.emit(RemoveWorktreeEvent::Close),
                    Err(e) => {
                        this.error = Some(e);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

impl EventEmitter<RemoveWorktreeEvent> for RemoveWorktreeDialog {}

impl Render for RemoveWorktreeDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let dirty = match &self.check {
            Some(Ok((_, c))) => c.changed,
            _ => 0,
        };
        let files = |n: usize| if n == 1 { "file" } else { "files" };
        let text = match &self.check {
            None => "Checking it for uncommitted work…".to_string(),
            Some(Err(e)) => e.clone(),
            Some(Ok((_, c))) => format!(
                "{}{}",
                c.branch
                    .as_ref()
                    .map(|b| format!("Branch {b} and its commits stay in the repository. "))
                    .unwrap_or_default(),
                if dirty > 0 {
                    format!(
                        "{dirty} {} not committed and will be lost.",
                        if dirty == 1 { "file is" } else { "files are" }
                    )
                } else {
                    "Everything in it is committed.".into()
                }
            ),
        };
        let can_remove = matches!(self.check, Some(Ok(_))) && !self.busy;
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
                    cx.emit(RemoveWorktreeEvent::Close);
                }
            }))
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Remove the worktree of “{}”?", self.title)),
            )
            .child(
                div()
                    .text_xs()
                    .font_family(crate::terminal::mono(cx))
                    .text_color(theme.muted_foreground)
                    .child(self.cwd.clone()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(text),
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
                            .id("rmwt-keep")
                            .a11y_button("Keep it")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .cursor_pointer()
                            .child("Keep it")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(RemoveWorktreeEvent::Close)),
                            ),
                    )
                    .child(
                        div()
                            .id("rmwt-remove")
                            .a11y_button("Remove")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(theme.danger)
                            .text_color(white())
                            .cursor_pointer()
                            .when(!can_remove, |d| d.opacity(0.5))
                            .child(if dirty > 0 {
                                format!("Remove and lose {dirty} {}", files(dirty))
                            } else {
                                "Remove worktree".into()
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_folders_under_the_worktrees_root_count_as_wraithgrid_worktrees() {
        assert!(super::is_worktree_cwd("~/.wraithgrid/worktrees/api/feat-x"));
        assert!(super::is_worktree_cwd("~\\.wraithgrid\\worktrees\\api\\x"));
        assert!(!super::is_worktree_cwd("~/code/api"));
        assert!(!super::is_worktree_cwd(
            "/home/a/.wraithgrid/worktrees/api/x"
        ));
    }
}
