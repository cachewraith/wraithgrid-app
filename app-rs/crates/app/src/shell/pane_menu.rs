//! A pane's menu (`Pane.tsx`'s ContextMenu): from its ⋯ button, a right-click on its header or
//! Shift+F10; alone on screen it is the app's More menu too (#89). And the confirm before
//! closing a pane whose process still works (`CloseConfirm`).

use gpui_kit::*;
use wraithgrid_core::types::{PaneView, ShortcutId};

use super::icons::{glyph, small, z, Glyph};
use super::menu::MenuItem;
use super::sidebar::pal;
use super::top_bar::UtilAct;
use super::{BtnKind, Shell};
use crate::a11y::A11y as _;
use crate::dialogs::PaletteAction;

#[derive(Clone, Copy)]
enum Act {
    NewConversation,
    Resume(bool),
    ShowChanges,
    SwitchBranch,
    OpenBranch,
    Restart,
    Close,
    Util(UtilAct),
}

impl Shell {
    /// Opens a pane's menu at a point; `keys`: from the keyboard, its first row shows lit.
    pub(super) fn open_pane_menu(
        &mut self,
        pane_id: String,
        at: Point<Pixels>,
        keys: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_menus(cx);
        self.focused = Some(pane_id.clone());
        self.pane_menu = Some((pane_id, at));
        self.menu_opened(keys);
        cx.notify();
    }

    fn pane_menu_items(&self, pane_id: &str, cx: &App) -> Vec<MenuItem<(String, Act)>> {
        let p = pal(cx);
        let c = self.config();
        let Some(pane) = c
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .find(|p| p.id == pane_id)
        else {
            return vec![];
        };
        let chat = pane.view == PaneView::Chat && !self.as_terminal.contains(&pane.id);
        let cli = pane
            .account_id
            .as_deref()
            .and_then(|id| c.account(id))
            .map(|a| a.cli);
        // Only a CLI that can resume a conversation offers to (`canResume`).
        let resumable = !pane.shell
            && pane.view != PaneView::Preview
            && cli.is_none_or(|c| c.spec().resume_args.is_some());
        let repo = self.git_heads.get(&pane.cwd).is_some_and(|g| g.repo);
        let remote = self.machine_of_pane(pane_id).is_some();
        let item = |label: &str, act: Act| MenuItem::new(label, (pane_id.to_string(), act));
        let icon = |g: Glyph| small(g, p.mu, p.panel);
        let mut items = vec![];
        if chat {
            items.push(item("New conversation", Act::NewConversation).icon(icon(Glyph::Compose)));
        }
        if resumable && !chat {
            let label = format!(
                "Continue conversation on restore: {}",
                if pane.resume { "on" } else { "off" }
            );
            let mut it = MenuItem::new(label, (pane_id.to_string(), Act::Resume(!pane.resume)));
            if pane.resume {
                it = it.icon(small(Glyph::Check, p.tx, p.panel));
            }
            items.push(it);
        }
        if repo {
            items.push(item("Show changes", Act::ShowChanges).kbd(self.hint(ShortcutId::Diff)));
            items.push(item("Switch branch…", Act::SwitchBranch).icon(icon(Glyph::Branch)));
            if !remote {
                items.push(item("Open a branch in a new pane…", Act::OpenBranch));
            }
        }
        items.push(item("Restart", Act::Restart).icon(icon(Glyph::Restart)));
        items.push(MenuItem {
            danger: true,
            ..item("Close pane", Act::Close).icon(small(Glyph::Close, p.err, p.panel))
        });
        // Alone on screen, this ⋯ is the app's More menu too (#89).
        if self.solo_pane().as_deref() == Some(pane_id) {
            let id = pane_id.to_string();
            for (i, mut it) in self
                .utility_items(move |u| (id.clone(), Act::Util(u)), cx)
                .into_iter()
                .enumerate()
            {
                it.divider = i == 0;
                items.push(it);
            }
        }
        items
    }

    fn run_pane_act(
        &mut self,
        pane_id: String,
        act: Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_menus(cx);
        match act {
            Act::NewConversation => {
                if let Some(chat) = self.chats.get(&pane_id).cloned() {
                    chat.update(cx, |c, cx| c.new_conversation(window, cx));
                }
            }
            Act::Resume(on) => self.update(cx, |c| {
                if let Some(p) = c
                    .workspaces
                    .iter_mut()
                    .flat_map(|w| &mut w.panes)
                    .find(|p| p.id == pane_id)
                {
                    p.resume = on;
                }
            }),
            // `toggleDiff(true)`: the panel shows this pane's folder.
            Act::ShowChanges => {
                self.show_changes(&pane_id, window, cx);
            }
            Act::SwitchBranch | Act::OpenBranch => {
                self.focused = Some(pane_id);
                let a = if matches!(act, Act::OpenBranch) {
                    PaletteAction::OpenBranch
                } else {
                    PaletteAction::SwitchBranch
                };
                self.palette_git(a, window, cx);
            }
            Act::Restart => self.restart_pane(pane_id, cx),
            Act::Close => self.request_close_pane(pane_id, cx),
            Act::Util(u) => self.run_util(u, window, cx),
        }
        cx.notify();
    }

    /// The menu at the pointer, over a scrim that closes it.
    pub(super) fn pane_menu_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (pane_id, at) = self.pane_menu.clone()?;
        let size = window.viewport_size();
        let items = self.pane_menu_items(&pane_id, cx);
        let card = self.menu_card(
            "pane-menu",
            "Pane actions",
            items,
            None,
            |this, (id, act), window, cx| this.run_pane_act(id, act, window, cx),
            window,
            cx,
        );
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("pane-menu-scrim")
                        .occlude()
                        .w(size.width)
                        .h(size.height)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.close_menus(cx);
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _, _, cx| {
                                this.close_menus(cx);
                            }),
                        )
                        .child(
                            anchored()
                                .position(at)
                                .snap_to_window_with_margin(px(4.))
                                .child(card),
                        ),
                ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    /// `closePane` from the UI: a pane still working asks first.
    pub(super) fn request_close_pane(&mut self, pane_id: String, cx: &mut Context<Self>) {
        let busy = matches!(
            self.pane_status(&pane_id),
            Some("running" | "approval" | "background")
        );
        if busy {
            self.focused = Some(pane_id.clone());
            self.closing = Some(pane_id);
            self.close_focus_pending = true;
            cx.notify();
        } else {
            self.close_pane(pane_id, cx);
        }
    }

    /// "Process is still running", over the pane being closed; Cancel takes the keyboard,
    /// Esc cancels.
    pub(super) fn close_confirm(
        &mut self,
        pane_id: &str,
        command: &str,
        cwd: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.closing.as_deref() != Some(pane_id) {
            return None;
        }
        let p = pal(cx);
        let focus = self
            .close_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        if std::mem::take(&mut self.close_focus_pending) {
            window.focus(&focus, cx);
        }
        let pid = self
            .pids
            .get(pane_id)
            .filter(|p| **p > 0)
            .map_or(String::new(), |p| format!(" (pid {p})"));
        let mono = crate::terminal::mono(cx);
        let (cancel, close) = (pane_id.to_string(), pane_id.to_string());
        Some(
            div()
                .id(SharedString::from(format!("close-scrim-{pane_id}")))
                .absolute()
                .inset_0()
                .bg(hsla(0., 0., 0., 0.45))
                .flex()
                .items_center()
                .justify_center()
                .p(z(12.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id(SharedString::from(format!("close-dlg-{pane_id}")))
                        .role(Role::AlertDialog)
                        .aria_label("Close pane")
                        .w_full()
                        .max_w(z(360.))
                        .p(z(16.))
                        .rounded(z(10.))
                        .border_1()
                        .border_color(p.line2)
                        .bg(p.raise)
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .gap(z(10.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(z(8.))
                                .text_size(z(14.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(glyph(Glyph::Warn, 16., p.warn, p.raise))
                                .child("Process is still running"),
                        )
                        .child(
                            div()
                                .text_size(z(12.5))
                                .text_color(p.mu)
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .child(div().font_family(mono.clone()).text_color(p.tx).child(command.to_string()))
                                        .child("\u{a0}is still running in\u{a0}")
                                        .child(div().font_family(mono).child(cwd.to_string()))
                                        .child(format!("{pid}.")),
                                )
                                .child(
                                    "Closing the pane ends the process. Unsaved work in this session stops.",
                                ),
                        )
                        .child(
                            div()
                                .mt(z(4.))
                                .flex()
                                .justify_end()
                                .gap(z(8.))
                                .child(
                                    super::ui_btn(
                                        SharedString::from(format!("close-cancel-{pane_id}")),
                                        BtnKind::Ghost,
                                        &p,
                                    )
                                    .a11y_button("Cancel")
                                    .track_focus(&focus)
                                    .focus_visible(|s| s.border_color(p.acc))
                                    .child("Cancel")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if this.closing.as_deref() == Some(cancel.as_str()) {
                                            this.closing = None;
                                            this.focus_pane_view(&cancel.clone(), window, cx);
                                        }
                                        cx.notify();
                                    })),
                                )
                                .child(
                                    super::ui_btn(
                                        SharedString::from(format!("close-ok-{pane_id}")),
                                        BtnKind::Danger,
                                        &p,
                                    )
                                    .a11y_button("Close pane")
                                    .child("Close pane")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.closing = None;
                                        this.close_pane(close.clone(), cx);
                                    })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}
