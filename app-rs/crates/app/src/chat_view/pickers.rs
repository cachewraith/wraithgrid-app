//! The chips in the box (`ModelPicker.tsx`): account, model, effort and permission mode. Each
//! opens a short list above it (`ChipMenu`, drawn over the window so the pane never clips it);
//! a click outside or Esc closes it. Lists come from the CLI; nothing is hard-coded per model.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_chat::{cycle_modes, effort_label, ChatMode, CHAT_MODES};

use super::{ChatView, Picker};

/// What each effort level does (`EFFORT_HINT`).
fn effort_hint(effort: &str) -> &'static str {
    match effort {
        "low" => "Quickest answers, least thinking",
        "medium" => "Balanced",
        "high" => "Thinks longer on hard problems",
        "xhigh" => "Even longer",
        "max" => "As long as it takes",
        _ => "",
    }
}

/// A list above the element it is a child of (`ChipMenu`): its bottom edge at the parent's
/// top, at the parent's left (or right) edge, kept on screen.
pub(super) fn above(menu: impl IntoElement, right: bool) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .when(right, |d| d.right_0())
        .when(!right, |d| d.left_0())
        .child(
            deferred(
                anchored()
                    .anchor(if right {
                        Anchor::BottomRight
                    } else {
                        Anchor::BottomLeft
                    })
                    .offset(point(px(0.), px(-6.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(menu),
            )
            .with_priority(2),
        )
        .into_any_element()
}

/// A list's frame: a popover that scrolls past its height and keeps clicks to itself.
pub(super) fn pop_frame(id: impl Into<ElementId>, width: f32, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .occlude()
        .w(px(width))
        .max_h(px(420.))
        .overflow_y_scroll()
        .p_1()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .shadow_lg()
        .flex()
        .flex_col()
        .text_sm()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// One choice in a list (`Option`): name and hint, a tick when it is the current one.
fn option(
    id: String,
    name: String,
    hint: String,
    on: bool,
    disabled: bool,
    danger: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(SharedString::from(id))
        .role(Role::ListItem)
        .aria_label(name.clone())
        .aria_selected(on)
        .px_2()
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .when(on, |d| d.bg(theme.secondary))
        .when(disabled, |d| d.opacity(0.5))
        .when(!disabled, |d| {
            d.cursor_pointer().hover(|d| d.bg(theme.secondary))
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .when(danger, |d| d.text_color(theme.danger))
                        .child(name),
                )
                .when(!hint.is_empty(), |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(hint),
                    )
                }),
        )
        .child(div().flex_none().w(px(12.)).when(on, |d| d.child("✓")))
}

/// The quiet line under a list.
fn foot(text: String, cx: &App) -> Div {
    div()
        .px_2()
        .pt_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

impl ChatView {
    /// The effort levels the chosen model takes (the CLI says); empty: no effort chip.
    pub(super) fn efforts(&self) -> Vec<String> {
        self.current_model()
            .map(|m| m.efforts)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| effort_label(e).is_some())
            .collect()
    }

    /// The modes this CLI has (all, unless it said otherwise); auto only where the model has
    /// it (or it is on already).
    fn modes(&self) -> Vec<ChatMode> {
        let auto = self.current_model().is_some_and(|m| m.auto_mode);
        let now = self.current_mode();
        CHAT_MODES
            .into_iter()
            .filter(|m| {
                self.thread
                    .controls
                    .as_ref()
                    .is_none_or(|c| c.modes.contains(m))
            })
            .filter(|m| *m != ChatMode::Auto || auto || now == ChatMode::Auto)
            .collect()
    }

    /// The modes Shift+Tab steps through: Claude Code's, as far as this CLI has them.
    fn cycle_list(&self) -> Vec<ChatMode> {
        let auto = self.current_model().is_some_and(|m| m.auto_mode);
        cycle_modes(auto)
            .into_iter()
            .filter(|m| {
                self.thread
                    .controls
                    .as_ref()
                    .is_none_or(|c| c.modes.contains(m))
            })
            .collect()
    }

    /// Shift+Tab steps through the modes, as in Claude Code.
    pub(super) fn cycle_mode(&mut self, cx: &mut Context<Self>) {
        let modes = self.cycle_list();
        let Some(first) = modes.first().copied() else {
            return;
        };
        let now = self.current_mode();
        let next = modes
            .iter()
            .position(|x| *x == now)
            .map_or(first, |i| modes[(i + 1) % modes.len()]);
        self.choose_mode(next, cx);
    }

    /// A click anywhere outside an open list closes it.
    pub(super) fn popover_scrim(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.popover_open() {
            return None;
        }
        let size = window.viewport_size();
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id(SharedString::from(format!("scrim-{}", self.pane_id)))
                        .occlude()
                        .w(size.width)
                        .h(size.height)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                this.close_popovers(cx);
                                this.composer.update(cx, |c, cx| c.focus(window, cx));
                            }),
                        ),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The model list (`ModelPicker`).
    fn model_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let models = self.models();
        let want = self.setup.model.clone().unwrap_or_else(|| "default".into());
        let frame = pop_frame("model-pop", 320., cx)
            .role(Role::Dialog)
            .aria_label("Model");
        if models.is_empty() {
            return frame
                .child(foot(
                    "The model list appears once the chat has started.".into(),
                    cx,
                ))
                .into_any_element();
        }
        frame
            .children(models.into_iter().map(|m| {
                let value = m.value.clone();
                option(
                    format!("model-{}", m.value),
                    m.label.clone(),
                    m.description.clone(),
                    m.value == want,
                    false,
                    false,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.choose_model(value.clone(), cx)))
            }))
            .into_any_element()
    }

    /// The effort list (`EffortPicker`): it restarts the process, so not while a turn runs.
    fn effort_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.thread.busy;
        let levels = self.efforts();
        pop_frame("effort-pop", 280., cx)
            .role(Role::Dialog)
            .aria_label("Effort")
            .children(
                std::iter::once(None)
                    .chain(levels.into_iter().map(Some))
                    .map(|e| {
                        let (name, hint) = match &e {
                            None => ("Auto".to_string(), "The model's own default"),
                            Some(l) => (
                                effort_label(l).unwrap_or(l.as_str()).to_string(),
                                effort_hint(l),
                            ),
                        };
                        let on = self.setup.effort == e;
                        option(
                            format!("effort-{}", e.as_deref().unwrap_or("auto")),
                            name,
                            hint.to_string(),
                            on,
                            busy,
                            false,
                            cx,
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.choose_effort(e.clone(), cx)),
                        )
                    }),
            )
            .child(foot(
                if busy {
                    "Can change after this turn.".into()
                } else {
                    "Changing it restarts the chat; it carries on.".into()
                },
                cx,
            ))
            .into_any_element()
    }

    /// The mode list (`ModeList`): Full access takes a second click.
    fn mode_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.current_mode();
        let asks = self.thread.controls.as_ref().is_none_or(|c| c.approvals);
        let modes = self.modes();
        let cycle = modes
            .iter()
            .filter(|m| !matches!(m, ChatMode::DontAsk | ChatMode::BypassPermissions))
            .count();
        pop_frame("mode-pop", 320., cx)
            .role(Role::Dialog)
            .aria_label("Mode")
            .children(modes.into_iter().map(|m| {
                let bypass = m == ChatMode::BypassPermissions;
                let confirming = bypass && self.x.bypass_confirm && current != m;
                // A CLI that can't ask in a chat refuses what would ask instead.
                let hint = match m {
                    ChatMode::Default if !asks => {
                        "Refuses edits and commands that need approval (it can't ask here)"
                    }
                    ChatMode::AcceptEdits if !asks => {
                        "Edits files on its own; refuses commands that need approval"
                    }
                    _ => m.hint(),
                };
                option(
                    format!("mode-{}", m.as_str()),
                    if confirming {
                        "Click again for full access".into()
                    } else {
                        m.label().into()
                    },
                    hint.into(),
                    m == current,
                    false,
                    bypass,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    // Running everything unasked takes a second click.
                    if bypass && this.current_mode() != m && !this.x.bypass_confirm {
                        this.x.bypass_confirm = true;
                        return cx.notify();
                    }
                    this.choose_mode(m, cx)
                }))
            }))
            .child(foot(
                format!(
                    "Shift+Tab switches between the first {}.",
                    match cycle {
                        1 => "one".to_string(),
                        2 => "two".to_string(),
                        3 => "three".to_string(),
                        4 => "four".to_string(),
                        n => n.to_string(),
                    }
                ),
                cx,
            ))
            .into_any_element()
    }

    /// Account, model, effort and mode chips in the box; each opens its list above it.
    pub(super) fn choices_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let account = self.account_chip(cx);
        let theme = cx.theme().clone();
        let mut chip =
            |id: &'static str, label: String, aria: String, which: Picker, danger: bool| {
                let open = self.picker == Some(which);
                let list = open.then(|| match which {
                    Picker::Model => self.model_list(cx),
                    Picker::Effort => self.effort_list(cx),
                    Picker::Mode => self.mode_list(cx),
                });
                div()
                    .id(id)
                    .relative()
                    .role(Role::Button)
                    .aria_label(aria)
                    .aria_expanded(open)
                    .flex_none()
                    .px_2()
                    .py(px(1.))
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(if danger {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .hover(|d| d.bg(theme.secondary))
                    .when(open, |d| d.bg(theme.secondary))
                    .child(format!("{label} ▾"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let was = this.picker == Some(which);
                        this.close_popovers(cx);
                        if !was {
                            this.picker = Some(which);
                        }
                        cx.notify();
                    }))
                    .children(list.map(|l| above(l, false)))
            };
        let model = self.model_label();
        let model_chip = chip(
            "pick-model",
            model.clone(),
            format!("Model: {model}. Change"),
            Picker::Model,
            false,
        );
        let effort_chip = (!self.efforts().is_empty()).then(|| {
            let label = self
                .setup
                .effort
                .as_deref()
                .and_then(effort_label)
                .unwrap_or("Auto effort")
                .to_string();
            chip(
                "pick-effort",
                label.clone(),
                format!("Effort: {label}. Change"),
                Picker::Effort,
                false,
            )
        });
        let mode = self.current_mode();
        let mode_chip = chip(
            "pick-mode",
            mode.label().to_string(),
            format!("Mode: {}. Change (Shift+Tab)", mode.label()),
            Picker::Mode,
            mode == ChatMode::BypassPermissions,
        );
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .children(account)
            .child(model_chip)
            .children(effort_chip)
            .child(mode_chip)
            .into_any_element()
    }
}
