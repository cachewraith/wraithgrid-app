//! A project with no panes (`EmptyState.tsx`): New pane, or start from a layout: a preset makes
//! the slots, then New pane asks for the first one's account and folder.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::layout::{empty_slot_ids, Preset};

use super::icons::{glyph, hex, svg_image, z, Glyph};
use super::Shell;
use crate::a11y::A11y as _;

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
        // `EmptyState`'s drawing: a dashed accent cell with a +, and three quiet ones.
        let (acc, _) = hex(theme.ring);
        let (line, _) = hex(theme.input);
        let (plus, _) = hex(theme.link);
        let art = img(svg_image(format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 64 64" fill="none" stroke-width="2"><rect x="4" y="4" width="26" height="26" rx="6" stroke="{acc}" stroke-dasharray="4 4"/><rect x="34" y="4" width="26" height="26" rx="6" stroke="{line}"/><rect x="4" y="34" width="26" height="26" rx="6" stroke="{line}"/><rect x="34" y="34" width="26" height="26" rx="6" stroke="{line}"/><path d="M17 12v10M12 17h10" stroke="{plus}" stroke-linecap="round"/></svg>"#
        )))
        .size(z(64.))
        .flex_none();
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
                .a11y_button(format!("Start from {}", p.label()))
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
                    .a11y_button("New pane")
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
                    .child(glyph(
                        Glyph::Plus,
                        16.,
                        theme.primary_foreground,
                        theme.primary,
                    ))
                    .child("New pane")
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
