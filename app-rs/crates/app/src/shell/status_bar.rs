//! The footer (`StatusBar.tsx`): "Updated to X · See what's new" once after an update,
//! "Update available", and the focus and shortcuts hints.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::shortcuts::bindings;
use wraithgrid_core::types::ShortcutId;

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
        let theme = cx.theme().clone();
        let mac = cfg!(target_os = "macos");
        let sheet = bindings(&self.config().settings.shortcuts)
            .into_iter()
            .find(|(id, _)| *id == ShortcutId::Shortcuts)
            .map(|(_, b)| super::sidebar::binding_hint(&b))
            .unwrap_or_default();
        let focus = if mac { "⌘⌥Arrow" } else { "Ctrl+Alt+Arrow" };
        let available = match &self.update {
            UpdateState::Available(r) => Some(r.version.clone()),
            _ => None,
        };
        let link = |id: &'static str, label: String| {
            div()
                .id(id)
                .a11y_button(label.clone())
                .px_2()
                .rounded_sm()
                .cursor_pointer()
                .text_color(theme.primary)
                .hover(|d| d.bg(theme.secondary))
                .child(label)
        };
        div()
            .h(px(22.))
            .flex_shrink_0()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(div().flex_1())
            .when_some(self.updated_to.clone(), |d, v| {
                d.child(
                    link("sbar-updated", format!("Updated to {v} · See what’s new")).on_click(
                        cx.listener(|this, _, window, cx| {
                            this.updated_to = None;
                            this.show_updates(window, cx);
                        }),
                    ),
                )
                .child(
                    div()
                        .id("sbar-updated-x")
                        .a11y_button("Dismiss")
                        .px_1()
                        .cursor_pointer()
                        .hover(|d| d.text_color(theme.foreground))
                        .child("×")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.updated_to = None;
                            cx.notify();
                        })),
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
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.secondary))
                    .child(format!("{sheet} All shortcuts"))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_shortcuts(window, cx))),
            )
    }
}
