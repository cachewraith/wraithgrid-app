//! The top row's middle (`TopBar.tsx`): on a screen, back to the project; on the project page,
//! "Open chats"; zoomed, the pane's place in the grid and Back to grid; in the grid, the project
//! link, the chat count, "+N hidden", the Git button and the layout presets. At its end, the
//! usage pill and the ⋯ menu (Keyboard shortcuts, Mods, Settings).

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::ids::new_id;
use wraithgrid_core::layout::{
    auto_grid, build_preset, detect_preset, pane_ids, shown_layout, Preset,
};

use super::Shell;
use crate::a11y::A11y as _;

/// A preset as a small drawing of its cells.
fn preset_glyph(p: Preset) -> &'static str {
    match p {
        Preset::One => "▢",
        Preset::Two => "◫",
        Preset::Four => "⊞",
        Preset::Three => "▥",
    }
}

impl Shell {
    /// Every screen and the project page closed: back to the grid.
    fn back_to_grid(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_view = None;
        self.accounts_view = None;
        self.usage_view = None;
        self.mods_view = None;
        self.scheduled_view = None;
        self.project_view = None;
        self.close_modal(window, cx);
    }

    /// `applyPreset`: the shown panes, then the hidden ones, into the preset's cells.
    pub(super) fn apply_preset(&mut self, p: Preset, cx: &mut Context<Self>) {
        self.update(cx, |c| {
            let id = c.active_workspace.clone();
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == id) {
                let mut order = pane_ids(w.layout.as_ref());
                let hidden: Vec<String> = w
                    .panes
                    .iter()
                    .map(|p| p.id.clone())
                    .filter(|p| !order.contains(p))
                    .collect();
                order.extend(hidden);
                w.layout = Some(build_preset(p, &order, || new_id("s")));
            }
        });
        self.zoomed = None;
    }

    /// `swap`: two panes change places in the layout.
    pub(super) fn swap_panes(&mut self, a: &str, b: &str, cx: &mut Context<Self>) {
        let (a, b) = (a.to_string(), b.to_string());
        self.update(cx, |c| {
            let id = c.active_workspace.clone();
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == id) {
                let ids: Vec<String> = w.panes.iter().map(|p| p.id.clone()).collect();
                w.layout = wraithgrid_core::layout::shown_layout(w.layout.as_ref(), &ids)
                    .map(|l| wraithgrid_core::layout::swap_panes(&l, &a, &b));
            }
        });
    }

    /// `showAllPanes`: hidden panes come back into an even grid.
    fn show_all_panes(&mut self, cx: &mut Context<Self>) {
        self.update(cx, |c| {
            let id = c.active_workspace.clone();
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == id) {
                let mut order = pane_ids(w.layout.as_ref());
                let hidden: Vec<String> = w
                    .panes
                    .iter()
                    .map(|p| p.id.clone())
                    .filter(|p| !order.contains(p))
                    .collect();
                order.extend(hidden);
                w.layout = auto_grid(&order);
            }
        });
    }

    /// The row's left part and its right part (before the update button and window controls).
    pub(super) fn top_bar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, Vec<AnyElement>) {
        let theme = cx.theme().clone();
        let ws = self.config().active().clone();
        // What the grid shows: the saved layout, or an even grid when there is none.
        let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
        let shown_tree = shown_layout(ws.layout.as_ref(), &ids);
        let btn = |id: &'static str| {
            div()
                .id(id)
                .px_2()
                .py(px(2.))
                .rounded_md()
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
        };
        let muted = |t: String| div().text_color(theme.muted_foreground).child(t);
        let mut left: Vec<AnyElement> = vec![];
        let mut right: Vec<AnyElement> = vec![];
        let screen = [
            (self.settings_view.is_some(), "Settings"),
            (self.accounts_view.is_some(), "Accounts"),
            (self.mods_view.is_some(), "Mods"),
            (self.usage_view.is_some(), "Usage"),
            (self.scheduled_view.is_some(), "Scheduled"),
        ]
        .into_iter()
        .find_map(|(on, t)| on.then_some(t));
        if let Some(title) = screen {
            left.push(
                btn("top-back")
                    .a11y_button(format!("Back to {}", ws.name))
                    .child(format!("← {}", ws.name))
                    .on_click(cx.listener(|this, _, window, cx| this.back_to_grid(window, cx)))
                    .into_any_element(),
            );
            left.push(
                div()
                    .w(px(1.))
                    .h(px(14.))
                    .bg(theme.border)
                    .into_any_element(),
            );
            left.push(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title)
                    .into_any_element(),
            );
        } else if self.project_view.is_some() {
            left.push(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(ws.name.clone())
                    .into_any_element(),
            );
            left.push(muted("Project".into()).into_any_element());
            if !ws.panes.is_empty() {
                right.push(
                    btn("top-open-chats")
                        .a11y_button(format!("Open chats {}", ws.panes.len()))
                        .border_1()
                        .border_color(theme.border)
                        .child(format!("Open chats {}", ws.panes.len()))
                        .on_click(cx.listener(|this, _, window, cx| this.back_to_grid(window, cx)))
                        .into_any_element(),
                );
            }
        } else if let Some(z) = self
            .zoomed
            .clone()
            .and_then(|z| ws.panes.iter().find(|p| p.id == z).cloned())
        {
            left.push(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(z.title.clone())
                    .into_any_element(),
            );
            left.push(muted(format!("zoomed · {}", ws.name)).into_any_element());
            left.extend(
                self.git_button(window, cx)
                    .map(IntoElement::into_any_element),
            );
            // Where the zoomed pane sits in the grid.
            let shown = pane_ids(shown_tree.as_ref());
            let cells: Vec<String> = if shown.is_empty() {
                vec![z.id.clone()]
            } else {
                shown.into_iter().take(8).collect()
            };
            let cols = match cells.len() {
                n if n >= 5 => 4,
                3 => 3,
                n => n.clamp(1, 2),
            };
            let rows = cells.chunks(cols).map(|row| {
                div().flex().gap(px(2.)).children(row.iter().map(|id| {
                    div()
                        .w(px(7.))
                        .h(px(5.))
                        .rounded(px(1.))
                        .bg(if *id == z.id {
                            theme.primary
                        } else {
                            theme.border
                        })
                }))
            });
            right.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(rows)
                    .into_any_element(),
            );
            right.push(
                btn("top-unzoom")
                    .a11y_button("Back to grid")
                    .border_1()
                    .border_color(theme.border)
                    .child("⤡ Back to grid")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.zoomed = None;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        } else {
            let ws_id = ws.id.clone();
            left.push(
                btn("top-project")
                    .a11y_button(format!("{} project page", ws.name))
                    .child(format!("← {}", ws.name))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_project_page(ws_id.clone(), window, cx)
                    }))
                    .into_any_element(),
            );
            let empty = shown_tree.is_none();
            let n = ws.panes.len();
            left.push(
                muted(if empty {
                    "No chats yet".into()
                } else {
                    format!("{n} {}", if n == 1 { "chat" } else { "chats" })
                })
                .into_any_element(),
            );
            let hidden = n.saturating_sub(pane_ids(shown_tree.as_ref()).len());
            if hidden > 0 {
                left.push(
                    btn("top-hidden")
                        .a11y_button(format!("Show {hidden} hidden panes"))
                        .border_1()
                        .border_color(theme.border)
                        .child(format!("+{hidden} hidden"))
                        .on_click(cx.listener(|this, _, _, cx| this.show_all_panes(cx)))
                        .into_any_element(),
                );
            }
            left.extend(
                self.git_button(window, cx)
                    .map(IntoElement::into_any_element),
            );
            if !empty {
                let current = detect_preset(shown_tree.as_ref());
                let presets = Preset::ALL.map(|p| {
                    div()
                        .id(SharedString::from(format!("preset-{}", p.slots())))
                        .a11y_toggle(p.label(), current == Some(p))
                        .px_2()
                        .cursor_pointer()
                        .when(current == Some(p), |d| {
                            d.bg(theme.secondary).text_color(theme.foreground)
                        })
                        .hover(|d| d.bg(theme.secondary))
                        .tooltip(move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(p.label()).build(window, cx)
                        })
                        .child(preset_glyph(p))
                        .on_click(cx.listener(move |this, _, _, cx| this.apply_preset(p, cx)))
                });
                right.push(
                    div()
                        .flex()
                        .items_center()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .text_color(theme.muted_foreground)
                        .children(presets)
                        .child(div().w(px(1.)).h(px(12.)).bg(theme.border))
                        .child(
                            div()
                                .id("top-zoom")
                                .a11y_button("Zoom focused pane")
                                .px_2()
                                .cursor_pointer()
                                .hover(|d| d.bg(theme.secondary))
                                .child("⤢")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.zoomed = this.focused.clone().or_else(|| {
                                        this.config().active().panes.first().map(|p| p.id.clone())
                                    });
                                    cx.notify();
                                })),
                        )
                        .into_any_element(),
                );
            }
        }
        right.push(self.usage_pill(cx).into_any_element());
        right.push(
            btn("top-more")
                .a11y_button("More")
                .child("⋯")
                .on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                    this.utility_menu = Some(e.position());
                    cx.notify();
                }))
                .into_any_element(),
        );
        (left, right)
    }

    /// The gauge and the session limit of the account new chats use ("42%", "work · 42%").
    fn usage_pill(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let c = self.config();
        let id = c
            .settings
            .default_account_id
            .clone()
            .or_else(|| c.accounts.first().map(|a| a.id.clone()));
        let named = c.accounts.len() > 1;
        let account = id.as_deref().and_then(|id| c.account(id));
        let pct = id
            .as_deref()
            .and_then(|id| self.limits.get(id))
            .and_then(|l| {
                wraithgrid_usage::model::live_window(l.five_hour, wraithgrid_usage::now_ms())
                    .map(|w| (w.utilization * 100.0).round() as i64)
            });
        let text = match (named, account, pct) {
            (true, Some(a), p) => {
                format!("{} · {}", a.name, p.map_or("–".into(), |p| format!("{p}%")))
            }
            (_, _, Some(p)) => format!("{p}%"),
            _ => "Usage".into(),
        };
        let on = self.usage_view.is_some();
        div()
            .id("top-usage")
            .a11y_toggle(format!("Usage: {text}"), on)
            .px_2()
            .py(px(2.))
            .rounded_md()
            .cursor_pointer()
            .text_color(theme.muted_foreground)
            .when(on, |d| d.bg(theme.secondary))
            .hover(|d| d.bg(theme.secondary))
            .child(format!("◔ {text}"))
            .on_click(cx.listener(move |this, _, window, cx| {
                if on {
                    this.back_to_grid(window, cx)
                } else {
                    this.open_usage(window, cx)
                }
            }))
    }

    /// Shortcuts, Mods and Settings behind ⋯ (`UtilityButtons`).
    pub(super) fn utility_menu_layer(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let at = self.utility_menu?;
        let theme = cx.theme().clone();
        let size = window.viewport_size();
        let item = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .role(Role::MenuItem)
                .aria_label(label)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(label)
        };
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("utility-scrim")
                        .occlude()
                        .w(size.width)
                        .h(size.height)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.utility_menu = None;
                                cx.notify();
                            }),
                        )
                        .child(
                            anchored()
                                .position(at - point(px(240.), px(0.)))
                                .snap_to_window_with_margin(px(4.))
                                .child(
                                    div()
                                        .w(px(240.))
                                        .mt_4()
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
                                        .child(item("util-keys", "Keyboard shortcuts").on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.utility_menu = None;
                                                this.toggle_shortcuts(window, cx);
                                            }),
                                        ))
                                        .child(item("util-mods", "Mods").on_click(cx.listener(
                                            |this, _, window, cx| {
                                                this.utility_menu = None;
                                                this.open_mods(window, cx);
                                            },
                                        )))
                                        .child(item("util-settings", "Settings").on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.utility_menu = None;
                                                this.open_settings(window, cx);
                                            }),
                                        )),
                                ),
                        ),
                ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }
}
