//! The footer (`StatusBar.tsx`): "Updated to X · See what's new" once after an update,
//! "Update available", and the focus and shortcuts hints, at the bottom of the main card.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::types::ShortcutId;

use super::icons::z;
use super::sidebar::pal;
use super::{Shell, UpdateState};
use crate::a11y::A11y as _;

impl Shell {
    /// Shows Settings → About (what's new and the updater).
    fn show_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
        if let Some(view) = self.settings_view.clone() {
            view.update(cx, |s, cx| s.show_tab("About", cx));
        }
    }

    pub(super) fn status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        let mono = crate::terminal::mono(cx);
        let mac = cfg!(target_os = "macos");
        let sheet = self.hint(ShortcutId::Shortcuts);
        let focus = if mac { "⌘⌥Arrow" } else { "Ctrl+Alt+Arrow" };
        let available = match &self.update {
            UpdateState::Available(r) => Some(r.version.clone()),
            _ => None,
        };
        // `.sbar button`: quiet text that brightens on hover.
        let link = |id: &'static str, label: String| {
            div()
                .id(id)
                .a11y_button(label.clone())
                .px(z(4.))
                .py(px(2.))
                .rounded(z(4.))
                .cursor_pointer()
                .text_color(p.acc)
                .hover(|s| s.text_color(p.tx))
                .child(label)
        };
        // `kbd` in the bar: the keys in the mono face, no frame (ui.css).
        let kbd = |k: String| div().px(z(5.)).text_size(z(10.5)).text_color(p.mu).child(k);
        div()
            .min_h(z(28.))
            .flex_none()
            .px(z(12.))
            .flex()
            .items_center()
            .gap(z(14.))
            .bg(p.side)
            .font_family(mono)
            .text_size(z(11.))
            .text_color(p.mu)
            .child(div().flex_1())
            .when_some(self.updated_to.clone(), |d, v| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            link("sbar-updated", format!("Updated to {v} · See what’s new"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.updated_to = None;
                                    this.show_updates(window, cx);
                                })),
                        )
                        .child(
                            div()
                                .id("sbar-updated-x")
                                .a11y_button("Dismiss")
                                .px(z(4.))
                                .cursor_pointer()
                                .hover(|s| s.text_color(p.tx))
                                .child("×")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.updated_to = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(available, |d, v| {
                d.child(
                    link("sbar-available", format!("Update available: {v}"))
                        .on_click(cx.listener(|this, _, window, cx| this.show_updates(window, cx))),
                )
            })
            .child(format!("{focus} focus"))
            .child(
                div()
                    .id("sbar-shortcuts")
                    .a11y_button("All shortcuts")
                    .flex()
                    .items_center()
                    .gap(z(6.))
                    .px(z(4.))
                    .py(px(2.))
                    .rounded(z(4.))
                    .cursor_pointer()
                    .hover(|s| s.text_color(p.tx))
                    .when(!sheet.is_empty(), |d| d.child(kbd(sheet.clone())))
                    .child("All shortcuts")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_shortcuts(window, cx))),
            )
    }
}
