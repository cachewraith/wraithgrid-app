//! A project with no panes (`EmptyState.tsx`): New pane, or start from a layout: a preset makes
//! the slots, then New pane asks for the first one's account and folder.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::layout::{empty_slot_ids, Preset};

use super::Shell;

/// A preset's cells as a small grid: columns, rows.
fn mini(p: Preset) -> (usize, usize) {
    match p {
        Preset::One => (1, 1),
        Preset::Two => (2, 1),
        Preset::Four => (2, 2),
        Preset::Three => (3, 1),
    }
}

impl Shell {
    fn start_from(&mut self, p: Preset, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_preset(p, cx);
        self.pending_slot = empty_slot_ids(self.config().active().layout.as_ref())
            .into_iter()
            .next();
        self.open_new_pane(window, cx);
    }

    pub(super) fn empty_project(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let name = self.config().active().name.clone();
        let (new_pane_hint, _) = self.hints();
        let cell = |accent: bool| {
            div()
                .size(px(26.))
                .rounded(px(6.))
                .border_2()
                .border_color(if accent { theme.ring } else { theme.border })
                .when(accent, |d| {
                    d.border_dashed()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.ring)
                        .child("+")
                })
        };
        let art = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .gap(px(4.))
                    .child(cell(true))
                    .child(cell(false)),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.))
                    .child(cell(false))
                    .child(cell(false)),
            );
        let cards = Preset::ALL.into_iter().map(|p| {
            let (cols, rows) = mini(p);
            let grid = div()
                .w(px(44.))
                .h(px(30.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .children((0..rows).map(|_| {
                    div().flex_1().flex().gap(px(2.)).children(
                        (0..cols).map(|_| div().flex_1().rounded(px(2.)).bg(theme.border)),
                    )
                }));
            div()
                .id(SharedString::from(format!("start-{}", p.slots())))
                .w(px(108.))
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .text_xs()
                .child(grid)
                .child(p.label())
                .on_click(cx.listener(move |this, _, window, cx| this.start_from(p, window, cx)))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(art)
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{name} has no panes yet")),
            )
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Each pane runs claude in one project folder, signed in with the account you pick.",
            ))
            .child(
                div()
                    .id("empty-new-pane")
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(theme.primary)
                    .text_color(theme.primary_foreground)
                    .hover(|d| d.bg(theme.primary_hover))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child("+ New pane")
                    .when(!new_pane_hint.is_empty(), |d| {
                        d.child(div().text_xs().opacity(0.8).child(new_pane_hint.clone()))
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.pending_slot = None;
                        this.open_new_pane(window, cx)
                    })),
            )
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Or start from a layout"),
            )
            .child(div().flex().gap_2().children(cards))
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                "A preset creates the slots; you choose the account and folder for each one.",
            ))
            .into_any_element()
    }
}
