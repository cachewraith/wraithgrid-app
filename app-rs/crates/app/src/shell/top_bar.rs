//! The top row over the panes (`TopBar.tsx`): on a screen, back to the project; on the project
//! page, "Open chats"; zoomed, the pane's place in the grid and Back to grid; in the grid, the
//! project link, the chat count, "+N hidden", the Git button and the layout presets. At its end,
//! the usage pill, the ⋯ menu (Keyboard shortcuts, Mods, Settings) and the window buttons. With
//! one pane on screen the pane's own header is the top row instead (#89). Its empty space moves
//! the window; its buttons never do.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::ids::new_id;
use wraithgrid_core::layout::{
    auto_grid, build_preset, detect_preset, pane_ids, shown_layout, Preset,
};
use wraithgrid_core::types::ShortcutId;

use super::icons::{glyph, small, z, Glyph};
use super::menu::MenuItem;
use super::sidebar::pal;
use super::{bounds_probe, Shell, TOP_ROW};
use crate::a11y::A11y as _;

/// A preset's icon: a small drawing of its cells.
fn preset_glyph(p: Preset) -> Glyph {
    match p {
        Preset::One => Glyph::Preset1,
        Preset::Two => Glyph::Preset2,
        Preset::Four => Glyph::Preset4,
        Preset::Three => Glyph::Preset3,
    }
}

/// What the ⋯ menu does (`utilityItems`); also the lone pane's ⋯ (#89).
#[derive(Clone, Copy)]
pub(super) enum UtilAct {
    Shortcuts,
    Mods,
    Settings,
}

/// "Usage" severity, as the pill colours it (`severity`).
fn severity(utilization: f64) -> u8 {
    if utilization >= 0.9 {
        2
    } else if utilization >= 0.7 {
        1
    } else {
        0
    }
}

impl Shell {
    /// Every screen and the project page closed: back to the grid.
    pub(super) fn back_to_grid(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.side_show_grid();
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

    /// `toggleZoom`: the focused pane (else the first) alone, or back to the grid.
    pub(super) fn toggle_zoom(&mut self, pane: Option<String>, cx: &mut Context<Self>) {
        if self.zoomed.is_some() && (pane.is_none() || pane == self.zoomed) {
            self.zoomed = None;
            return cx.notify();
        }
        let ws = self.config().active();
        let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
        let target = pane.or_else(|| self.focused.clone()).or_else(|| {
            pane_ids(shown_layout(ws.layout.as_ref(), &ids).as_ref())
                .into_iter()
                .next()
        });
        if let Some(t) = target.filter(|t| ids.contains(t)) {
            self.zoomed = Some(t.clone());
            self.focused = Some(t);
            self.side_show_grid();
        }
        cx.notify();
    }

    /// The top row, unless one pane is on screen and its header is the row (#89).
    pub(super) fn top_row(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.solo_pane().is_some() {
            return None;
        }
        let p = pal(cx);
        let theme = cx.theme().clone();
        let ws = self.config().active().clone();
        // What the grid shows: the saved layout, or an even grid when there is none.
        let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
        let shown_tree = shown_layout(ws.layout.as_ref(), &ids);
        let mut left: Vec<AnyElement> = vec![];
        let mut right: Vec<AnyElement> = vec![];
        let title = |t: String| {
            div()
                .flex_none()
                .text_size(z(14.5))
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(t)
                .into_any_element()
        };
        let sub = |t: String| {
            div()
                .text_size(z(12.))
                .text_color(p.fa)
                .truncate()
                .child(t)
                .into_any_element()
        };
        let vsep = || {
            div()
                .flex_none()
                .w(px(1.))
                .h(z(22.))
                .bg(p.line)
                .into_any_element()
        };
        let screen = [
            (self.settings_view.is_some(), "Settings"),
            (self.accounts_view.is_some(), "Accounts"),
            (self.mods_view.is_some(), "Mods"),
            (self.usage_view.is_some(), "Usage"),
            (self.scheduled_view.is_some(), "Scheduled"),
        ]
        .into_iter()
        .find_map(|(on, t)| on.then_some(t));
        let zoom_hint = self.hint(ShortcutId::ToggleZoom);
        let in_grid = screen.is_none() && self.project_view.is_none();
        let zoomed = self
            .zoomed
            .clone()
            .and_then(|z| ws.panes.iter().find(|p| p.id == z).cloned());
        if let Some(t) = screen {
            left.push(
                div()
                    .id("top-back")
                    .a11y_button(format!("Back to {}", ws.name))
                    .occlude()
                    .h(z(34.))
                    .px(z(8.))
                    .flex()
                    .items_center()
                    .gap(z(8.))
                    .rounded(z(6.))
                    .text_size(z(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.raise))
                    .child(glyph(Glyph::Back, 16., p.mu, p.bg))
                    .child(ws.name.clone())
                    .on_click(cx.listener(|this, _, window, cx| this.back_to_grid(window, cx)))
                    .into_any_element(),
            );
            left.push(vsep());
            left.push(title(t.into()));
        } else if self.project_view.is_some() {
            left.push(title(ws.name.clone()));
            left.push(sub("Project".into()));
            if !ws.panes.is_empty() {
                right.push(
                    super::ui_btn("top-open-chats", super::BtnKind::Plain, &p)
                        .a11y_button(format!("Open chats ({})", ws.panes.len()))
                        .child("Open chats")
                        .child(
                            div()
                                .min_w(z(18.))
                                .px(z(5.))
                                .flex()
                                .justify_center()
                                .rounded_full()
                                .bg(p.sel)
                                .text_size(z(11.5))
                                .text_color(p.mu)
                                .child(ws.panes.len().to_string()),
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.back_to_grid(window, cx)))
                        .into_any_element(),
                );
            }
        } else if let Some(zp) = zoomed {
            left.push(title(zp.title.clone()));
            left.push(sub(format!("zoomed · {}", ws.name)));
            left.extend(self.git_button(window, cx));
            // Where the zoomed pane sits in the grid.
            let shown = pane_ids(shown_tree.as_ref());
            let cells: Vec<String> = if shown.is_empty() {
                vec![zp.id.clone()]
            } else {
                shown.into_iter().take(8).collect()
            };
            let cols = match cells.len() {
                n if n >= 5 => 4,
                3 => 3,
                n => n.clamp(1, 2),
            };
            let rows = cells.chunks(cols).map(|row| {
                div()
                    .flex_1()
                    .flex()
                    .gap(px(2.))
                    .children(row.iter().map(|id| {
                        div().flex_1().rounded(px(2.)).bg(if *id == zp.id {
                            p.acc
                        } else {
                            p.line2
                        })
                    }))
            });
            right.push(
                div()
                    .flex_none()
                    .w(z(44.))
                    .h(z(26.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(rows)
                    .into_any_element(),
            );
            let tip = format!("Back to grid ({zoom_hint})");
            right.push(
                super::ui_btn("top-unzoom", super::BtnKind::Primary, &p)
                    .a11y_button(tip.clone())
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(glyph(
                        Glyph::Unzoom,
                        16.,
                        theme.primary_foreground,
                        theme.primary,
                    ))
                    .child("Back to grid")
                    .child(
                        div()
                            .font_family(crate::terminal::mono(cx))
                            .text_size(z(10.5))
                            .opacity(0.8)
                            .child(zoom_hint.clone()),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_zoom(None, cx)))
                    .into_any_element(),
            );
            right.push(vsep());
        } else {
            left.push(self.project_link(&ws.id, &ws.name, cx));
            let empty = shown_tree.is_none();
            let n = ws.panes.len();
            left.push(
                div()
                    .flex_none()
                    .font_family(crate::terminal::mono(cx))
                    .text_size(z(12.))
                    .text_color(p.mu)
                    .child(if empty {
                        "No chats yet".to_string()
                    } else {
                        format!("{n} {}", if n == 1 { "chat" } else { "chats" })
                    })
                    .into_any_element(),
            );
            let hidden = n.saturating_sub(pane_ids(shown_tree.as_ref()).len());
            left.extend(self.hidden_chip(hidden, cx));
            left.extend(self.git_button(window, cx));
            if !empty {
                right.push(self.layout_presets(true, cx));
            }
        }
        right.extend(self.utility_buttons(window, cx));
        let lined = in_grid && (shown_tree.is_some() || self.zoomed.is_some());
        Some(
            self.drag_area(div().id("top-row"))
                .h(z(TOP_ROW))
                .flex_none()
                .flex()
                .items_center()
                .gap(z(8.))
                .pl(z(16.))
                .pr(z(8.))
                .bg(p.bg)
                .border_b_1()
                .border_color(if lined {
                    p.line
                } else {
                    gpui_kit::transparent_black()
                })
                .text_color(p.tx)
                .children(left)
                .child(div().flex_1().h_full())
                .children(right)
                .into_any_element(),
        )
    }

    /// `ProjectLink`: back to the project's overview, under its name.
    pub(super) fn project_link(
        &mut self,
        ws_id: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = pal(cx);
        let id = ws_id.to_string();
        div()
            .id("top-project")
            .a11y_button(format!("{name} project page"))
            .occlude()
            .flex_none()
            .h(z(30.))
            .pl(z(4.))
            .pr(z(8.))
            .ml(z(-6.))
            .flex()
            .items_center()
            .gap(z(4.))
            .rounded(z(6.))
            .cursor_pointer()
            .hover(|s| s.bg(p.hover))
            .tooltip(|window, cx| {
                Tooltip::new("Project overview: open chats and history").build(window, cx)
            })
            .child(glyph(Glyph::Back, 16., p.fa, p.bg))
            .child(
                div()
                    .text_size(z(14.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(name.to_string()),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_project_page(id.clone(), window, cx)
            }))
            .into_any_element()
    }

    /// `HiddenChip`: panes left out of the layout, one click from coming back.
    pub(super) fn hidden_chip(
        &mut self,
        hidden: usize,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if hidden == 0 {
            return None;
        }
        let p = pal(cx);
        Some(
            div()
                .id("top-hidden")
                .a11y_button(format!("Show {hidden} hidden panes"))
                .occlude()
                .flex_none()
                .h(z(22.))
                .px(z(8.))
                .flex()
                .items_center()
                .rounded(z(11.))
                .border_1()
                .border_color(p.line2)
                .bg(p.side)
                .text_size(z(11.5))
                .text_color(p.mu)
                .cursor_pointer()
                .hover(|s| s.text_color(p.tx).border_color(p.acc))
                .tooltip(|window, cx| Tooltip::new("Show all panes in the grid").build(window, cx))
                .child(format!("+{hidden} hidden"))
                .on_click(cx.listener(|this, _, _, cx| this.show_all_panes(cx)))
                .into_any_element(),
        )
    }

    /// `LayoutPresets`: the preset buttons, and with `zoom` the zoom button after them.
    pub(super) fn layout_presets(&mut self, zoom: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let ws = self.config().active();
        let ids: Vec<String> = ws.panes.iter().map(|p| p.id.clone()).collect();
        let current = detect_preset(shown_layout(ws.layout.as_ref(), &ids).as_ref());
        let any = !ws.panes.is_empty();
        let seg = |id: SharedString, on: bool| {
            div()
                .id(id)
                .occlude()
                .h(z(26.))
                .min_w(z(30.))
                .px(z(7.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(z(7.))
                .cursor_pointer()
                .when(on, |d| d.bg(p.sel))
                .when(!on, |d| d.hover(|s| s.bg(p.hover)))
        };
        let presets = Preset::ALL.map(|pr| {
            let on = current == Some(pr);
            seg(SharedString::from(format!("preset-{}", pr.slots())), on)
                .a11y_toggle(pr.label(), on)
                .tooltip(move |window, cx| Tooltip::new(pr.label()).build(window, cx))
                .child(glyph(
                    preset_glyph(pr),
                    16.,
                    if on { p.tx } else { p.mu },
                    p.bg,
                ))
                .on_click(cx.listener(move |this, _, _, cx| this.apply_preset(pr, cx)))
        });
        let tip = format!("Zoom focused pane ({})", self.hint(ShortcutId::ToggleZoom));
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(1.))
            .p(px(2.))
            .children(presets)
            .when(zoom, |d| {
                d.child(div().w(px(1.)).h(z(16.)).mx(z(3.)).bg(p.line2))
                    .child(
                        seg("top-zoom".into(), false)
                            .a11y_button(tip.clone())
                            .when(!any, |d| d.opacity(0.4))
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                            .child(glyph(Glyph::Zoom, 16., p.mu, p.bg))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_zoom(None, cx))),
                    )
            })
            .into_any_element()
    }

    /// Usage, ⋯ and the window buttons, at the row's end.
    fn utility_buttons(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = vec![
            self.usage_pill(cx).into_any_element(),
            self.more_button("top-more", cx),
        ];
        out.extend(self.update_chip(cx));
        out.extend(self.window_controls(window, cx));
        out
    }

    /// The ⋯ button: the More menu, right-aligned under it.
    pub(super) fn more_button(&mut self, id: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let open = self.utility_menu.is_some();
        let bounds = self.more_bounds.clone();
        div()
            .id(id)
            .a11y_button("More")
            .occlude()
            .flex_none()
            .size(z(30.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(z(6.))
            .cursor_pointer()
            .when(open, |d| d.bg(p.raise))
            .hover(|s| s.bg(p.raise))
            .tooltip(|window, cx| Tooltip::new("More").build(window, cx))
            .child(bounds_probe(self.more_bounds.clone()))
            .child(glyph(Glyph::More, 16., p.mu, p.bg))
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                if open {
                    this.close_menus(cx);
                    return;
                }
                this.close_menus(cx);
                let b = *bounds.borrow();
                let at = b.map_or(e.position() + point(px(-240.), px(16.)), |b| {
                    point(b.right() - px(260.), b.bottom() + px(6.))
                });
                this.utility_menu = Some(at);
                this.menu_opened(e.is_keyboard());
                cx.notify();
            }))
            .into_any_element()
    }

    /// Shortcuts, Mods and Settings (`utilityItems`).
    pub(super) fn utility_items<A>(
        &self,
        wrap: impl Fn(UtilAct) -> A,
        cx: &App,
    ) -> Vec<MenuItem<A>> {
        let p = pal(cx);
        vec![
            MenuItem::new("Keyboard shortcuts", wrap(UtilAct::Shortcuts))
                .icon(small(Glyph::Keyboard, p.mu, p.panel))
                .kbd(self.hint(ShortcutId::Shortcuts)),
            MenuItem::new("Mods", wrap(UtilAct::Mods)).icon(small(Glyph::Mods, p.mu, p.panel)),
            MenuItem::new("Settings", wrap(UtilAct::Settings)).icon(small(
                Glyph::Settings,
                p.mu,
                p.panel,
            )),
        ]
    }

    pub(super) fn run_util(&mut self, act: UtilAct, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menus(cx);
        match act {
            UtilAct::Shortcuts => self.toggle_shortcuts(window, cx),
            UtilAct::Mods => self.open_mods(window, cx),
            UtilAct::Settings => self.open_settings(window, cx),
        }
    }

    /// `limitAccountId`: the focused pane's account when its CLI reports limits, else the one
    /// new chats use, else the first such.
    fn limit_account(&self) -> Option<(String, bool)> {
        let c = self.config();
        let reports = |id: &str| c.account(id).is_some_and(|a| a.cli.spec().reports_limits);
        let focused = self.focused.as_ref().and_then(|f| {
            c.workspaces
                .iter()
                .flat_map(|w| &w.panes)
                .find(|p| &p.id == f)
        });
        if let Some(id) = focused
            .filter(|p| !p.shell)
            .and_then(|p| p.account_id.clone())
            .filter(|id| reports(id))
        {
            return Some((id, true));
        }
        c.settings
            .default_account_id
            .clone()
            .filter(|id| reports(id))
            .or_else(|| {
                c.accounts
                    .iter()
                    .find(|a| a.cli.spec().reports_limits)
                    .map(|a| a.id.clone())
            })
            .map(|id| (id, false))
    }

    /// `UsageButton`: the gauge and the session limit ("42%", "work · 42%"), else "Usage".
    pub(super) fn usage_pill(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = pal(cx);
        let c = self.config();
        let limit = self.limit_account();
        // Whose limit it is, when more than one account reports limits (same as the sidebar).
        let named = c
            .accounts
            .iter()
            .filter(|a| a.cli.spec().reports_limits)
            .count()
            > 1;
        let account = limit.as_ref().and_then(|(id, _)| c.account(id));
        let five = limit
            .as_ref()
            .and_then(|(id, _)| self.limits.get(id))
            .and_then(|l| {
                wraithgrid_usage::model::live_window(l.five_hour, wraithgrid_usage::now_ms())
            });
        let pct = five.map(|w| (w.utilization * 100.0).round() as i64);
        let sev = five.map_or(0, |w| severity(w.utilization));
        let color = match sev {
            2 => p.err,
            1 => p.warn,
            _ => p.mu,
        };
        let label = match (named, account, pct) {
            (n, a, Some(pct)) => {
                let who = a
                    .filter(|_| n)
                    .map_or(String::new(), |a| format!("{}: ", a.name));
                let whose = match (n, limit.as_ref().is_some_and(|(_, f)| *f)) {
                    (false, _) => "",
                    (true, true) => " (the focused chat’s account)",
                    (true, false) => " (the account new chats use)",
                };
                format!("Usage: {who}{pct}% of the current session limit{whose}")
            }
            (true, Some(a), None) => format!(
                "Usage: no limits from {} yet (they show after its first chat turn)",
                a.name
            ),
            _ => "Usage".into(),
        };
        let on = self.usage_view.is_some();
        let tip = label.clone();
        div()
            .id("top-usage")
            .a11y_toggle(label, on)
            .occlude()
            .flex_none()
            .h(z(26.))
            .min_w(z(26.))
            .px(z(9.))
            .flex()
            .items_center()
            .gap(z(5.))
            .rounded_full()
            .cursor_pointer()
            .text_size(z(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if sev > 0 { color } else { p.mu })
            .when(on, |d| d.bg(p.hover).text_color(p.tx))
            .hover(|s| s.bg(p.hover))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(small(
                Glyph::Gauge,
                if sev > 0 { color } else { p.mu },
                p.bg,
            ))
            .map(|d| match (named, account) {
                (true, Some(a)) => d
                    .child(
                        div()
                            .max_w(z(96.))
                            .truncate()
                            .text_color(p.mu)
                            .child(a.name.clone()),
                    )
                    .child("·")
                    .child(pct.map_or("–".into(), |p| format!("{p}%"))),
                _ => d.child(pct.map_or("Usage".into(), |p| format!("{p}%"))),
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if on {
                    this.back_to_grid(window, cx)
                } else {
                    this.open_usage(window, cx)
                }
            }))
    }

    /// The More menu (`UtilityButtons`), right-aligned under ⋯.
    pub(super) fn utility_menu_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let at = self.utility_menu?;
        let size = window.viewport_size();
        let items = self.utility_items(|a| a, cx);
        let card = self.menu_card(
            "util-menu",
            "More",
            items,
            Some(260.),
            |this, act, window, cx| this.run_util(act, window, cx),
            window,
            cx,
        );
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
}
