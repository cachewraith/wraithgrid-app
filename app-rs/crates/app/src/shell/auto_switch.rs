//! Switching account at the limit (#61, #161; `autoSwitch`/`answerSwitch` in store.ts and
//! `AutoSwitchDialog.tsx`): after a turn, a chat at its account's limit carries on as the next
//! account of its CLI with room, on its own or once the user says so. Questions wait in a queue,
//! oldest first, shown only while no other dialog is open; a pane closed or switched by hand
//! meanwhile drops its question. Answered Stay, it isn't asked again on that account until the
//! limit refuses a message.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::types::AutoSwitchMode;
use wraithgrid_usage::auto_switch::{decide_switch, next_account, peak_use, SwitchDecision};
use wraithgrid_usage::format::reset_label;

use super::sidebar::{pal, provider_logo};
use super::{Modal, Shell};
use crate::a11y::A11y as _;

#[derive(Clone)]
pub(super) struct SwitchAsk {
    pane_id: String,
    from_id: String,
    /// None: no account has room (the dialog just says so).
    to_id: Option<String>,
    reason: String,
    /// The limit refused the last message: it is sent again after the switch.
    resend: bool,
}

/// One account in the dialog: its fuller window now, as a bar.
#[derive(Clone)]
struct Side {
    cli: String,
    name: String,
    pct: Option<i64>,
    window: &'static str,
    label: &'static str,
    /// The reset (from) or what is left (to).
    foot: String,
}

pub(super) enum SwitchAnswer {
    Switch { dont_ask: bool },
    Stay,
}

pub(super) struct AutoSwitchDialog {
    focus: FocusHandle,
    title: String,
    reason: String,
    where_: String,
    sides: Option<(Side, Side)>,
    /// When no account has room: the line saying so.
    none_left: String,
    resend: bool,
    dont_ask: bool,
}

impl EventEmitter<SwitchAnswer> for AutoSwitchDialog {}

impl Shell {
    /// After a turn: at the account's limit, switch, ask, or say no account has room.
    pub(super) fn maybe_switch(
        &mut self,
        pane_id: &str,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let s = &self.config().settings;
        let (mode, at) = (s.auto_switch_mode, s.auto_switch_at);
        if mode == AutoSwitchMode::Off || self.switching.contains(pane_id) {
            return;
        }
        let Some(from_id) = self.pane_account(pane_id) else {
            return;
        };
        let accounts = self.usage_accounts();
        let Some(from) = accounts.iter().find(|a| a.id == from_id) else {
            return;
        };
        let decision = decide_switch(
            &accounts,
            from,
            &self.limits,
            wraithgrid_usage::now_ms(),
            at,
            error.as_deref(),
        );
        let ask = mode == AutoSwitchMode::Ask;
        let pane = pane_id.to_string();
        match decision {
            SwitchDecision::Stay => {
                self.told_none_left.remove(pane_id);
                self.switch_declined.remove(pane_id);
            }
            SwitchDecision::NoneLeft { reason } => {
                if !self.told_none_left.insert(pane.clone()) {
                    return;
                }
                if ask {
                    self.ask_switch(
                        SwitchAsk {
                            pane_id: pane,
                            from_id,
                            to_id: None,
                            reason,
                            resend: false,
                        },
                        window,
                        cx,
                    );
                } else if let Some(chat) = self.chats.get(pane_id).cloned() {
                    let label = wraithgrid_core::agent_cli::AgentCli::parse(&from.cli)
                        .map_or(from.cli.as_str(), |c| c.spec().label);
                    let text = format!("{reason}, and no other {label} account has room.");
                    chat.update(cx, |c, cx| {
                        c.add_notice(wraithgrid_chat::ItemLevel::Error, &text, cx)
                    });
                }
            }
            SwitchDecision::Switch { to, reason, resend } => {
                self.told_none_left.remove(pane_id);
                let to_id = to.id.clone();
                if ask {
                    // Answered Stay on this account: only again when the limit refuses a message.
                    if self.switch_declined.get(pane_id) == Some(&from_id) && !resend {
                        return;
                    }
                    self.ask_switch(
                        SwitchAsk {
                            pane_id: pane,
                            from_id,
                            to_id: Some(to_id),
                            reason,
                            resend,
                        },
                        window,
                        cx,
                    );
                } else {
                    self.run_switch(&pane, &to_id, &reason, resend, window, cx);
                }
            }
        }
    }

    fn pane_account(&self, pane_id: &str) -> Option<String> {
        self.config()
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .find(|p| p.id == pane_id)
            .and_then(|p| p.account_id.clone())
    }

    /// The chat carries on as `to_id` via Continue as… (the whole conversation goes with it);
    /// a turn the limit refused is sent again there, once.
    fn run_switch(
        &mut self,
        pane_id: &str,
        to_id: &str,
        reason: &str,
        resend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = self.chats.get(pane_id).cloned();
        let to_name = self
            .config()
            .account(to_id)
            .map_or_else(|| to_id.to_string(), |a| a.name.clone());
        let resend_text = if resend {
            chat.as_ref().and_then(|c| c.read(cx).last_user_text())
        } else {
            None
        };
        if let Some(chat) = &chat {
            let text = format!("{reason}: switching to {to_name}");
            chat.update(cx, |c, cx| {
                c.add_notice(wraithgrid_chat::ItemLevel::Info, &text, cx)
            });
        }
        self.switching.insert(pane_id.to_string());
        self.continue_as(pane_id, to_id, resend_text, window, cx);
        self.switching.remove(pane_id);
    }

    /// Queues a question for a pane, replacing any it already had.
    fn ask_switch(&mut self, ask: SwitchAsk, window: &mut Window, cx: &mut Context<Self>) {
        self.switch_asks.retain(|a| a.pane_id != ask.pane_id);
        self.switch_asks.push(ask);
        self.show_switch_ask(window, cx);
    }

    /// Shows the oldest question when no other dialog is open; stale ones are dropped.
    pub(super) fn show_switch_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while self.modal.is_none() && !self.switch_asks.is_empty() {
            let ask = self.switch_asks[0].clone();
            if self.pane_account(&ask.pane_id).as_ref() != Some(&ask.from_id) {
                self.switch_asks.remove(0);
                continue;
            }
            let dialog = self.switch_dialog(&ask, cx);
            let focus = dialog.read(cx).focus.clone();
            cx.subscribe_in(
                &dialog,
                window,
                move |this, _, e: &SwitchAnswer, window, cx| {
                    let (switch, dont_ask) = match e {
                        SwitchAnswer::Switch { dont_ask } => (true, *dont_ask),
                        SwitchAnswer::Stay => (false, false),
                    };
                    this.modal = None;
                    this.answer_switch(&ask.pane_id, switch, dont_ask, window, cx);
                    this.close_modal(window, cx);
                },
            )
            .detach();
            self.modal = Some(Modal::Switch(dialog));
            window.focus(&focus, cx);
            cx.notify();
        }
    }

    fn answer_switch(
        &mut self,
        pane_id: &str,
        switch: bool,
        dont_ask: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(at) = self.switch_asks.iter().position(|a| a.pane_id == pane_id) else {
            return;
        };
        let ask = self.switch_asks.remove(at);
        let from_id = self.pane_account(pane_id);
        // A pane closed or switched by hand meanwhile: nothing to do.
        if from_id.as_ref() != Some(&ask.from_id) || ask.to_id.is_none() {
            return;
        }
        if !switch {
            self.switch_declined
                .insert(pane_id.to_string(), ask.from_id.clone());
            return;
        }
        if dont_ask {
            self.update(cx, |c| c.settings.auto_switch_mode = AutoSwitchMode::Auto);
        }
        if self.switching.contains(pane_id) {
            return;
        }
        // The asked account may have filled up or signed out while the question was open.
        let threshold = self.config().settings.auto_switch_at;
        let now = wraithgrid_usage::now_ms();
        let accounts = self.usage_accounts();
        let room = |a: &wraithgrid_usage::UsageAccount| {
            a.signed_in && peak_use(self.limits.get(&a.id), now).map_or(0, |p| p.pct) < threshold
        };
        let asked = accounts.iter().find(|a| Some(&a.id) == ask.to_id.as_ref());
        let to = match asked {
            Some(a) if room(a) => Some(a.id.clone()),
            _ => accounts
                .iter()
                .find(|a| a.id == ask.from_id)
                .and_then(|from| next_account(&accounts, from, &self.limits, now, threshold))
                .map(|a| a.id.clone()),
        };
        match to {
            Some(to) => self.run_switch(pane_id, &to, &ask.reason, ask.resend, window, cx),
            None => self.switch_asks.push(SwitchAsk { to_id: None, ..ask }),
        }
    }

    fn switch_dialog(&self, ask: &SwitchAsk, cx: &mut Context<Self>) -> Entity<AutoSwitchDialog> {
        let c = self.config();
        let now = wraithgrid_usage::now_ms();
        let side = |id: &str, label: &'static str| -> Option<Side> {
            let a = c.account(id)?;
            let limits = self.limits.get(id);
            let peak = peak_use(limits, now);
            let pct = peak.map(|p| p.pct);
            let foot = if label == "now" {
                let win = match peak.map(|p| p.window) {
                    Some(wraithgrid_usage::auto_switch::LimitWindow::Weekly) => {
                        limits.and_then(|l| l.seven_day)
                    }
                    _ => limits.and_then(|l| l.five_hour),
                };
                win.map(|w| reset_label(w.resets_at, now))
                    .unwrap_or_default()
            } else {
                pct.map(|p| format!("{}% left", 100 - p))
                    .unwrap_or_default()
            };
            Some(Side {
                cli: a.cli.as_str().to_string(),
                name: a.name.clone(),
                pct,
                window: peak.map_or("", |p| p.window.label()),
                label,
                foot,
            })
        };
        let from = side(&ask.from_id, "now");
        let to = ask.to_id.as_deref().and_then(|id| side(id, "next"));
        let where_ = c
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .find(|p| p.id == ask.pane_id)
            .map(|p| p.title.clone())
            .unwrap_or_default();
        let none_left = from.as_ref().map_or_else(String::new, |f| {
            let label = wraithgrid_core::agent_cli::AgentCli::parse(&f.cli)
                .map_or(f.cli.as_str(), |c| c.spec().label);
            let resets = if f.foot.is_empty() {
                String::new()
            } else {
                format!(" {} {}.", f.name, f.foot)
            };
            format!("No other {label} account has room.{resets}")
        });
        let title = match &to {
            Some(t) => format!("Switch to {}?", t.name),
            None => "No account has room".into(),
        };
        let reason = ask.reason.clone();
        let resend = ask.resend;
        cx.new(|cx| AutoSwitchDialog {
            focus: cx.focus_handle(),
            title,
            reason,
            where_,
            sides: from.zip(to),
            none_left,
            resend,
            dont_ask: false,
        })
    }
}

impl Render for AutoSwitchDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = pal(cx);
        let side = |s: &Side| {
            let pct = s.pct.unwrap_or(0).clamp(0, 100) as f32;
            let fill = match s.pct {
                Some(n) if n >= 95 => theme.danger,
                Some(n) if n >= 80 => theme.warning,
                _ => theme.primary,
            };
            div()
                .flex_1()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(provider_logo(&s.cli, &s.name, 16., &p))
                        .child(div().font_weight(FontWeight::MEDIUM).child(s.name.clone())),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(match s.pct {
                            None => "usage not known yet".to_string(),
                            Some(n) => format!("{n}% · {}", s.window),
                        }),
                )
                .child(
                    div()
                        .h(px(4.))
                        .w_full()
                        .rounded_full()
                        .bg(theme.secondary)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(fill)
                                .w(relative(pct / 100.)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(s.label)
                        .child(s.foot.clone()),
                )
        };
        let has_to = self.sides.is_some();
        let body = match &self.sides {
            Some((from, to)) => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(side(from))
                        .child(div().text_color(theme.muted_foreground).child("→"))
                        .child(side(to)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .text_sm()
                        .child("• The whole conversation is copied over.")
                        .child(if self.resend {
                            "• Your last message is sent again."
                        } else {
                            "• Nothing is sent again: the last answer finished."
                        })
                        .child("• Same model and settings."),
                ),
            None => div().text_sm().child(self.none_left.clone()),
        };
        let button = |id: &'static str, text: &'static str, primary: bool| {
            div()
                .id(id)
                .a11y_button(text)
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .text_sm()
                .cursor_pointer()
                .border_color(if primary { theme.primary } else { theme.border })
                .when(primary, |d| {
                    d.bg(theme.primary).text_color(theme.primary_foreground)
                })
                .when(!primary, |d| d.hover(|d| d.bg(theme.secondary)))
                .child(text)
        };
        let dont_ask = self.dont_ask;
        div()
            .id("auto-switch")
            .a11y_dialog(self.title.clone())
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
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => cx.emit(SwitchAnswer::Stay),
                    "enter" if has_to => cx.emit(SwitchAnswer::Switch {
                        dont_ask: this.dont_ask,
                    }),
                    "enter" => cx.emit(SwitchAnswer::Stay),
                    _ => {}
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        if self.where_.is_empty() {
                            self.reason.clone()
                        } else {
                            format!("{} · in “{}”", self.reason, self.where_)
                        },
                    )),
            )
            .child(body)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .when(has_to, |d| {
                        d.child(
                            div().flex_1().child(
                                Checkbox::new("as-dont")
                                    .checked(dont_ask)
                                    .label("Don’t ask again")
                                    .on_click(cx.listener(|this, on: &bool, _, cx| {
                                        this.dont_ask = *on;
                                        cx.notify();
                                    })),
                            ),
                        )
                        .child(
                            button("as-stay", "Stay", false)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(SwitchAnswer::Stay))),
                        )
                        .child(
                            button(
                                "as-switch",
                                if self.resend {
                                    "Switch & resend"
                                } else {
                                    "Switch"
                                },
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.emit(SwitchAnswer::Switch {
                                    dont_ask: this.dont_ask,
                                })
                            })),
                        )
                    })
                    .when(!has_to, |d| {
                        d.child(
                            button("as-ok", "OK", true)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(SwitchAnswer::Stay))),
                        )
                    }),
            )
    }
}
