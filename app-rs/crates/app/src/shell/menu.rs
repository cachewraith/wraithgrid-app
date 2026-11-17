//! The one menu every popup uses (`ContextMenu.tsx`): the pane menu, the top row's ⋯, the Git
//! menu and the sidebar's project and account menus. Rows with an icon, a second line, a
//! shortcut and a check; headings and dividers. It takes the keyboard when it opens: ↑/↓ move,
//! Enter picks, Esc or Tab closes; then the keyboard goes back to the focused pane.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::icons::{small, z, Glyph};
use super::sidebar::pal;
use super::Shell;

/// One row (`MenuItem`).
pub(super) struct MenuItem<A> {
    pub label: String,
    pub icon: Option<AnyElement>,
    /// A quieter second line (an account's plan); `sub_warn` draws it amber.
    pub sub: Option<String>,
    pub sub_warn: bool,
    /// The item's shortcut, at the end.
    pub kbd: Option<String>,
    /// The current choice: filled, with a check at the end.
    pub checked: bool,
    /// A small heading above this row, starting a section.
    pub heading: Option<&'static str>,
    /// A line above this row.
    pub divider: bool,
    pub danger: bool,
    pub disabled: bool,
    pub act: A,
}

impl<A> MenuItem<A> {
    pub fn new(label: impl Into<String>, act: A) -> Self {
        MenuItem {
            label: label.into(),
            icon: None,
            sub: None,
            sub_warn: false,
            kbd: None,
            checked: false,
            heading: None,
            divider: false,
            danger: false,
            disabled: false,
            act,
        }
    }

    pub fn icon(mut self, icon: impl IntoElement) -> Self {
        self.icon = Some(icon.into_any_element());
        self
    }

    pub fn kbd(mut self, kbd: impl Into<String>) -> Self {
        let kbd = kbd.into();
        self.kbd = (!kbd.is_empty()).then_some(kbd);
        self
    }

    pub fn sub(mut self, sub: impl Into<String>) -> Self {
        self.sub = Some(sub.into());
        self
    }
}

/// The open menu's keyboard: which row is lit, and focus handed over or back.
#[derive(Default)]
pub(super) struct MenuNav {
    focus: Option<FocusHandle>,
    /// The lit row, by index among all rows.
    lit: Option<usize>,
    /// Lit rows show only once the keyboard moved (`:focus-visible`).
    keys: bool,
    /// A menu just opened: focus it when it is drawn.
    take_focus: bool,
    /// A menu just closed: the keyboard goes back to the focused pane.
    give_back: bool,
}

/// What a row does, once picked.
pub(super) type MenuRun<A> = fn(&mut Shell, A, &mut Window, &mut Context<Shell>);

impl Shell {
    /// A menu opened: it takes the keyboard on its first frame. `keys`: opened from the
    /// keyboard (Shift+F10), so its first row shows lit.
    pub(super) fn menu_opened(&mut self, keys: bool) {
        self.menu_nav.take_focus = true;
        self.menu_nav.lit = None;
        self.menu_nav.keys = keys;
    }

    /// Every popup menu closed (pane, ⋯, Git, sidebar); the keyboard goes back to the pane.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.pane_menu.take().is_some()
            | self.utility_menu.take().is_some()
            | self.git_menu.take().is_some()
            | self.close_side_menu();
        if open {
            self.menu_nav.give_back = true;
            self.menu_nav.lit = None;
            cx.notify();
        }
        open
    }

    /// Per frame, after the menus are drawn: a closed menu hands the keyboard back.
    pub(super) fn menu_focus_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.menu_nav.give_back) || self.modal.is_some() {
            return;
        }
        // Only when nothing else took it: a picked item may have opened a screen.
        let mine = self
            .menu_nav
            .focus
            .as_ref()
            .is_some_and(|f| f.is_focused(window));
        if !mine && window.focused(cx).is_some() {
            return;
        }
        if let Some(id) = self.focused.clone() {
            self.focus_pane_view(&id, window, cx);
        }
    }

    /// The menu card: rows from `items`, `width` CSS px (else at least 210), picks run `run`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn menu_card<A: Clone + 'static>(
        &mut self,
        id: &'static str,
        label: &'static str,
        items: Vec<MenuItem<A>>,
        width: Option<f32>,
        run: MenuRun<A>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let p = pal(cx);
        let focus = self
            .menu_nav
            .focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        if std::mem::take(&mut self.menu_nav.take_focus) {
            window.focus(&focus, cx);
        }
        let acts: Rc<Vec<Option<A>>> = Rc::new(
            items
                .iter()
                .map(|it| (!it.disabled).then(|| it.act.clone()))
                .collect(),
        );
        // The first enabled row is lit to start with, as the first item takes focus.
        let lit = self
            .menu_nav
            .lit
            .filter(|i| acts.get(*i).is_some_and(Option::is_some))
            .or_else(|| acts.iter().position(Option::is_some));
        self.menu_nav.lit = lit;
        let show_lit = self.menu_nav.keys;
        let rows = items.into_iter().enumerate().map(|(i, it)| {
            let act = it.act.clone();
            let color = if it.disabled {
                p.fa
            } else if it.danger {
                p.err
            } else {
                p.tx
            };
            let two = it.sub.is_some();
            let on = show_lit && lit == Some(i);
            div()
                .flex()
                .flex_col()
                .when(it.divider, |d| {
                    d.child(div().h(px(1.)).mx(z(4.)).my(z(5.)).bg(p.line))
                })
                .when_some(it.heading, |d, h| {
                    d.child(
                        div()
                            .px(z(8.))
                            .pt(z(6.))
                            .pb(z(4.))
                            .text_size(z(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.fa)
                            .child(h),
                    )
                })
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-{i}")))
                        .role(Role::MenuItem)
                        .aria_label(it.label.clone())
                        .min_h(z(if two { 44. } else { 34. }))
                        .flex()
                        .items_center()
                        .gap(z(10.))
                        .py(z(5.))
                        .pl(z(if two { 6. } else { 8. }))
                        .pr(z(if two { 10. } else { 12. }))
                        .rounded(z(7.))
                        .text_size(z(13.))
                        .text_color(color)
                        .when(it.checked, |d| d.bg(p.sel))
                        .when(on, |d| d.bg(p.raise))
                        .when(!it.disabled, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(p.raise))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    run(this, act.clone(), window, cx)
                                }))
                        })
                        .child(
                            div()
                                .w(z(if two { 26. } else { 16. }))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .children(it.icon),
                        )
                        .child(if two {
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap(px(1.))
                                .child(
                                    div()
                                        .truncate()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(it.label),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(z(11.5))
                                        .text_color(if it.sub_warn { p.warn } else { p.fa })
                                        .child(it.sub.unwrap_or_default()),
                                )
                        } else {
                            div().flex_1().min_w(px(0.)).truncate().child(it.label)
                        })
                        .when_some(it.kbd, |d, k| {
                            d.child(
                                div()
                                    .flex_none()
                                    .pl(z(16.))
                                    .text_size(z(11.))
                                    .text_color(p.fa)
                                    .child(k),
                            )
                        })
                        .when(it.checked, |d| d.child(small(Glyph::Check, p.tx, p.panel))),
                )
        });
        let keys_acts = acts.clone();
        div()
            .id(id)
            .role(Role::Menu)
            .aria_label(label)
            .track_focus(&focus)
            .occlude()
            .min_w(z(210.))
            .when_some(width, |d, w| d.w(z(w)))
            .p(z(5.))
            .flex()
            .flex_col()
            .rounded(z(12.))
            .border_1()
            .border_color(p.line2)
            .bg(p.panel)
            .shadow_lg()
            .text_color(p.tx)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                let enabled: Vec<usize> = keys_acts
                    .iter()
                    .enumerate()
                    .filter_map(|(i, a)| a.as_ref().map(|_| i))
                    .collect();
                let at = this
                    .menu_nav
                    .lit
                    .and_then(|l| enabled.iter().position(|i| *i == l));
                match e.keystroke.key.as_str() {
                    "escape" | "tab" => {
                        this.close_menus(cx);
                    }
                    "down" | "up" if !enabled.is_empty() => {
                        let n = enabled.len();
                        let next = match (at, e.keystroke.key == "down") {
                            (Some(a), true) => (a + 1) % n,
                            (Some(a), false) => (a + n - 1) % n,
                            (None, true) => 0,
                            (None, false) => n - 1,
                        };
                        this.menu_nav.lit = Some(enabled[next]);
                        this.menu_nav.keys = true;
                        cx.notify();
                    }
                    "enter" | "space" => {
                        if let Some(act) = this
                            .menu_nav
                            .lit
                            .and_then(|l| keys_acts.get(l).cloned().flatten())
                        {
                            run(this, act, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .children(rows)
    }
}
