//! The shortcuts sheet (`ShortcutsSheet.tsx`): every app shortcut, rebound ones with their
//! new keys. Opened by its own shortcut (which also closes it) and the command palette.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use wraithgrid_core::shortcuts::{binding_keys, bindings, chord_keys, Binding};
use wraithgrid_core::types::ShortcutId;

pub enum ShortcutsSheetEvent {
    Close,
}

/// A row's keys: a rebindable shortcut, or fixed keys (`mod` is Ctrl, or ⌘ on macOS).
enum Keys {
    Bound(ShortcutId),
    Fixed(&'static [&'static str]),
}

const ROWS: &[(&str, Keys)] = &[
    (
        "Search panes and commands",
        Keys::Bound(ShortcutId::Palette),
    ),
    ("New pane", Keys::Bound(ShortcutId::NewPane)),
    ("Close pane", Keys::Bound(ShortcutId::ClosePane)),
    (
        "Zoom pane / back to grid",
        Keys::Bound(ShortcutId::ToggleZoom),
    ),
    (
        "Focus pane left / up / right / down",
        Keys::Fixed(&["mod", "alt", "←", "↑", "→", "↓"]),
    ),
    (
        "Switch project",
        Keys::Fixed(&["mod", "shift", "1", "…", "9"]),
    ),
    ("Show / hide the git diff", Keys::Bound(ShortcutId::Diff)),
    (
        "Zoom the app in / out / reset",
        Keys::Fixed(&["mod", "+", "-", "0"]),
    ),
    ("New line in claude", Keys::Fixed(&["Shift", "Enter"])),
    (
        "Reply to the selected chat text",
        Keys::Fixed(&["mod", "shift", "R"]),
    ),
    (
        "Copy / paste in a terminal",
        Keys::Fixed(if cfg!(target_os = "macos") {
            &["⌘", "C", "/", "V"]
        } else {
            &["Ctrl", "Shift", "C", "/", "V"]
        }),
    ),
    ("Show this sheet", Keys::Bound(ShortcutId::Shortcuts)),
];

pub struct ShortcutsSheet {
    focus: FocusHandle,
    bindings: Vec<(ShortcutId, Binding)>,
}

impl ShortcutsSheet {
    pub fn new(saved: &[(ShortcutId, String)], cx: &mut Context<Self>) -> Self {
        ShortcutsSheet {
            focus: cx.focus_handle(),
            bindings: bindings(saved),
        }
    }
}

impl EventEmitter<ShortcutsSheetEvent> for ShortcutsSheet {}

impl Focusable for ShortcutsSheet {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// One key cap.
pub(crate) fn kbd(key: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .min_w(px(22.))
        .px_1p5()
        .py(px(1.))
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .text_xs()
        .text_center()
        .font_family(crate::terminal::mono(cx))
        .child(key.into())
}

/// Key caps for a list of keys; `…` and `/` stay plain text between them.
pub(crate) fn key_caps(keys: &[String], cx: &App) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .children(keys.iter().map(|k| {
            if k == "…" || k == "/" {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(k.clone())
            } else {
                kbd(k.clone(), cx)
            }
        }))
}

impl Render for ShortcutsSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mac = cfg!(target_os = "macos");
        let rows = ROWS.iter().map(|(label, keys)| {
            let keys = match keys {
                Keys::Bound(id) => self
                    .bindings
                    .iter()
                    .find(|(i, _)| i == id)
                    .map(|(_, b)| binding_keys(b, mac))
                    .unwrap_or_default(),
                Keys::Fixed(k) => chord_keys(k, mac),
            };
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .py(px(7.))
                .border_b_1()
                .border_color(theme.border)
                .child(div().text_sm().child(*label))
                .child(key_caps(&keys, cx))
        });
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(label)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ShortcutsSheetEvent::Close)))
        };
        div()
            .track_focus(&self.focus)
            .w(px(520.))
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
                    cx.emit(ShortcutsSheetEvent::Close);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Keyboard shortcuts"),
                    )
                    .child(
                        div()
                            .id("shortcuts-x")
                            .px_2()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.secondary))
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new("Close (Esc)")
                                    .build(window, cx)
                            })
                            .child("✕")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(ShortcutsSheetEvent::Close)),
                            ),
                    ),
            )
            .child(div().flex().flex_col().children(rows))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Change them in Settings → Keyboard shortcuts."),
                    )
                    .child(button("shortcuts-done", "Done")),
            )
    }
}
