//! A pane's menu (`Pane.tsx`'s ContextMenu): from its ⋯ button or a right-click on its header;
//! and the confirm before closing a pane whose process still works (`CloseConfirm`).

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::types::PaneView;

use super::Shell;
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
}

impl Shell {
    pub(super) fn open_pane_menu(
        &mut self,
        pane_id: String,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.pane_menu = Some((pane_id, at));
        cx.notify();
    }

    fn pane_menu_items(&self, pane_id: &str) -> Vec<(String, Act, bool)> {
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
        let agent = !pane.shell && pane.view != PaneView::Preview;
        let repo = self.git_heads.get(&pane.cwd).is_some_and(|g| g.repo);
        let remote = self.machine_of_pane(pane_id).is_some();
        let mut items = vec![];
        if chat {
            items.push(("New conversation".into(), Act::NewConversation, false));
        }
        if agent && !chat {
            items.push((
                format!(
                    "Continue conversation on restore: {}",
                    if pane.resume { "on" } else { "off" }
                ),
                Act::Resume(!pane.resume),
                false,
            ));
        }
        if repo {
            items.push(("Show changes".into(), Act::ShowChanges, false));
            items.push(("Switch branch…".into(), Act::SwitchBranch, false));
            if !remote {
                items.push((
                    "Open a branch in a new pane…".into(),
                    Act::OpenBranch,
                    false,
                ));
            }
        }
        items.push(("Restart".into(), Act::Restart, false));
        items.push(("Close pane".into(), Act::Close, true));
        items
    }

    fn run_pane_act(
        &mut self,
        pane_id: String,
        act: Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pane_menu = None;
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
            Act::ShowChanges => {
                self.focused = Some(pane_id);
                if self.changes.is_none() {
                    self.toggle_changes(window, cx);
                }
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
        }
        cx.notify();
    }

    /// The menu at the pointer, over a scrim that closes it.
    pub(super) fn pane_menu_layer(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (pane_id, at) = self.pane_menu.clone()?;
        let theme = cx.theme().clone();
        let size = window.viewport_size();
        let rows = self.pane_menu_items(&pane_id).into_iter().enumerate().map(
            |(i, (label, act, danger))| {
                let id = pane_id.clone();
                div()
                    .id(SharedString::from(format!("pane-menu-{i}")))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(danger, |d| d.text_color(theme.danger))
                    .hover(|d| d.bg(theme.secondary))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_pane_act(id.clone(), act, window, cx)
                    }))
            },
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
                                this.pane_menu = None;
                                cx.notify();
                            }),
                        )
                        .child(
                            anchored()
                                .position(at)
                                .snap_to_window_with_margin(px(4.))
                                .child(
                                    div()
                                        .w(px(260.))
                                        .p_1()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(theme.border)
                                        .bg(theme.popover)
                                        .shadow_lg()
                                        .flex()
                                        .flex_col()
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .children(rows),
                                ),
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
            self.closing = Some(pane_id);
            cx.notify();
        } else {
            self.close_pane(pane_id, cx);
        }
    }

    /// "Process is still running", over the pane being closed.
    pub(super) fn close_confirm(
        &mut self,
        pane_id: &str,
        command: &str,
        cwd: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.closing.as_deref() != Some(pane_id) {
            return None;
        }
        let theme = cx.theme().clone();
        let (cancel, close) = (pane_id.to_string(), pane_id.to_string());
        Some(
            div()
                .absolute()
                .inset_0()
                .bg(hsla(0., 0., 0., 0.45))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(380.))
                        .max_w(relative(0.9))
                        .p_4()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.warning)
                                .child("⚠ Process is still running"),
                        )
                        .child(div().text_sm().child(format!(
                            "{command} is still running in {cwd}. Closing the pane ends the process. Unsaved work in this session stops."
                        )))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    div()
                                        .id(SharedString::from(format!("close-cancel-{pane_id}")))
                                        .px_3()
                                        .py_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .hover(|d| d.bg(theme.secondary))
                                        .child("Cancel")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if this.closing.as_deref() == Some(cancel.as_str()) {
                                                this.closing = None;
                                            }
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("close-ok-{pane_id}")))
                                        .px_3()
                                        .py_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .bg(theme.danger)
                                        .text_color(gpui_kit::white())
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
