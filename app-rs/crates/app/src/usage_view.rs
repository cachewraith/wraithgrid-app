//! The Usage screen (`UsageView.tsx`): plan limits first, as claude words them, then spend for
//! the chosen range: a daily chart, projects and the most expensive chats.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::a11y::A11y as _;
use wraithgrid_core::config::Config;
use wraithgrid_usage::format::{fmt_cost, fmt_tokens, reset_label, severity, Severity};
use wraithgrid_usage::model::{
    live_window, summarize_usage, UsageData, UsageRange, UsageTotals, UsageWindow,
};

const RANGES: &[(UsageRange, &str, &str)] = &[
    (UsageRange::Today, "Today", "today"),
    (UsageRange::Days7, "7 days", "in the last 7 days"),
    (UsageRange::Days30, "30 days", "in the last 30 days"),
];

pub enum UsageViewEvent {
    Close,
}

pub struct UsageView {
    focus: FocusHandle,
    data: UsageData,
    config: Config,
    range: UsageRange,
}

/// A cost, or "–" when none of it was reported (codex, agy: tokens only).
fn cost_of(t: &UsageTotals) -> String {
    if t.has_cost() {
        fmt_cost(t.cost_usd)
    } else {
        "–".into()
    }
}

fn turns(n: u64) -> String {
    format!("{n} {}", if n == 1 { "turn" } else { "turns" })
}

impl UsageView {
    pub fn new(data: UsageData, config: Config, cx: &mut Context<Self>) -> Self {
        UsageView {
            focus: cx.focus_handle(),
            data,
            config,
            range: UsageRange::Days7,
        }
    }

    /// New data from the store (a poll or a finished turn).
    pub fn set_data(&mut self, data: UsageData, cx: &mut Context<Self>) {
        self.data = data;
        cx.notify();
    }

    fn ws_name(&self, id: &Option<String>) -> String {
        id.as_ref()
            .and_then(|id| self.config.workspaces.iter().find(|w| &w.id == id))
            .map_or_else(|| "—".into(), |w| w.name.clone())
    }

    fn limit_row(&self, label: &'static str, w: UsageWindow, now: i64, cx: &App) -> Div {
        let theme = cx.theme();
        let pct = (w.utilization * 100.0).round() as i64;
        let color = match severity(w.utilization) {
            Severity::Ok => theme.primary,
            Severity::Warn => theme.warning,
            Severity::Err => theme.danger,
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().w(px(120.)).text_sm().child(label))
            .child(
                div()
                    .w(px(56.))
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{pct}%")),
            )
            .child(
                div()
                    .flex_1()
                    .h(px(6.))
                    .rounded_full()
                    .bg(theme.secondary)
                    .child(
                        div()
                            .h_full()
                            .rounded_full()
                            .bg(color)
                            .w(relative((w.utilization as f32).clamp(0.0, 1.0))),
                    ),
            )
            .child(
                div()
                    .w(px(220.))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{}{}",
                        reset_label(w.resets_at, now),
                        if severity(w.utilization) == Severity::Err {
                            " · almost out"
                        } else {
                            ""
                        }
                    )),
            )
    }
}

impl EventEmitter<UsageViewEvent> for UsageView {}

impl Focusable for UsageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for UsageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let now = wraithgrid_usage::now_ms();
        let sum = summarize_usage(&self.data, self.range, now);
        let sentence = RANGES
            .iter()
            .find(|r| r.0 == self.range)
            .map_or("", |r| r.2);
        let ranges = RANGES.iter().map(|(r, label, _)| {
            let r = *r;
            div()
                .id(SharedString::from(format!("range-{label}")))
                .a11y_toggle(*label, self.range == r)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(self.range == r, |d| d.bg(theme.secondary))
                .child(*label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.range = r;
                    cx.notify();
                }))
        });
        let limits = self
            .config
            .accounts
            .iter()
            .filter(|a| a.cli.spec().reports_limits)
            .map(|a| {
                let l = self.data.limits.get(&a.id);
                let five = l.and_then(|l| live_window(l.five_hour, now));
                let week = l.and_then(|l| live_window(l.seven_day, now));
                div()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().font_weight(FontWeight::MEDIUM).child(a.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(a.cli.spec().label),
                            ),
                    )
                    .when(five.is_none() && week.is_none(), |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("No limits yet: they show after its first chat turn."),
                        )
                    })
                    .when_some(five, |d, w| {
                        d.child(self.limit_row("Current session", w, now, cx))
                    })
                    .when_some(week, |d, w| d.child(self.limit_row("Weekly", w, now, cx)))
            });
        // Daily cost of all accounts together, oldest first.
        let mut daily: Vec<(String, f64)> = Vec::new();
        for acct in sum.accounts.values() {
            for (i, d) in acct.daily.iter().enumerate() {
                if daily.len() <= i {
                    daily.push((d.day.clone(), 0.0));
                }
                daily[i].1 += d.cost_usd;
            }
        }
        let max = daily
            .iter()
            .map(|d| d.1)
            .fold(0.0_f64, f64::max)
            .max(0.0001);
        let bars = daily.iter().map(|(day, cost)| {
            div()
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .justify_end()
                .child(
                    div()
                        .w_full()
                        .rounded_sm()
                        .bg(theme.primary)
                        .h(relative((*cost / max) as f32)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(day.get(8..10).unwrap_or("").to_string()),
                )
        });
        let table_row = |cells: Vec<String>, head: bool| {
            div()
                .flex()
                .py_1()
                .border_b_1()
                .border_color(theme.border)
                .text_sm()
                .when(head, |d| d.text_color(theme.muted_foreground))
                .children(cells.into_iter().enumerate().map(|(i, c)| {
                    div()
                        .when(i == 0, |d| d.flex_1())
                        .when(i > 0, |d| d.w(px(110.)).text_right())
                        .truncate()
                        .child(c)
                }))
        };
        let projects: Vec<Div> = sum
            .projects
            .iter()
            .map(|p| {
                table_row(
                    vec![
                        self.ws_name(&p.workspace_id),
                        p.totals.turns.to_string(),
                        fmt_tokens(p.totals.tokens),
                        cost_of(&p.totals),
                    ],
                    false,
                )
            })
            .collect();
        let chats: Vec<Div> = sum
            .chats
            .iter()
            .map(|c| {
                let cost = if c.has_cost() {
                    fmt_cost(c.cost_usd)
                } else {
                    "–".into()
                };
                table_row(
                    vec![
                        if c.title.is_empty() {
                            "Untitled chat".into()
                        } else {
                            c.title.clone()
                        },
                        self.ws_name(&c.workspace_id),
                        cost,
                    ],
                    false,
                )
            })
            .collect();
        div()
            .id("usage")
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .bg(theme.background)
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(UsageViewEvent::Close);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Usage"),
                    )
                    .child(div().flex().gap_1().children(ranges))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("usage-close")
                            .a11y_button("Back to panes")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.secondary))
                            .child("← Back to panes")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(UsageViewEvent::Close))),
                    ),
            )
            .child(
                div()
                    .max_w(px(860.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Plan limits"))
                    .children(limits),
            )
            .child(
                div()
                    .max_w(px(860.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Spend"))
                    .child(div().text_sm().child(format!(
                        "{} over {} and {} tokens {sentence}.",
                        cost_of(&sum.total),
                        turns(sum.total.turns),
                        fmt_tokens(sum.total.tokens)
                    )))
                    .when(!daily.is_empty(), |d| {
                        d.child(div().h(px(120.)).flex().gap_1().children(bars))
                    })
                    .when(projects.is_empty(), |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!("No chat turns {sentence}.")),
                        )
                    })
                    .when(!projects.is_empty(), |d| {
                        d.child(table_row(
                            vec![
                                "Project".into(),
                                "Turns".into(),
                                "Tokens".into(),
                                "Cost".into(),
                            ],
                            true,
                        ))
                        .children(projects)
                    })
                    .when(!chats.is_empty(), |d| {
                        d.child(div().h(px(8.)))
                            .child(table_row(
                                vec!["Chat".into(), "Project".into(), "Cost".into()],
                                true,
                            ))
                            .children(chats)
                    }),
            )
    }
}
