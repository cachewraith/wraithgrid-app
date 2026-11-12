//! Scheduled chats (`ScheduledView.tsx`, #77): the jobs with their next run and last results,
//! switched on and off, run now or deleted, and a form to add one. When it runs, and what a run
//! does, is the `wraithgrid-schedule` crate's; the window runs them.

use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::config::{Config, ScheduleTarget, ScheduleWhen, ScheduledJob};
use wraithgrid_core::ids::new_id;
use wraithgrid_schedule::{default_once_at, describe_when, fmt_when, is_local_datetime, is_time};

pub enum ScheduledEvent {
    Save(Box<ScheduledJob>),
    Toggle(String, bool),
    Delete(String),
    RunNow(String),
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Once,
    Weekly,
    Hours,
    Reset,
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

pub struct ScheduledView {
    focus: FocusHandle,
    config: Config,
    prompt: Entity<TextareaState>,
    at: Entity<InputState>,
    time: Entity<InputState>,
    every: Entity<InputState>,
    kind: Kind,
    days: Vec<i64>,
    account: Option<String>,
    project: Option<String>,
    switch_at_limit: bool,
    error: Option<String>,
}

impl ScheduledView {
    pub fn new(config: Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let now = wraithgrid_usage::now_ms();
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("What should the chat do?")
                .auto_grow(2, 6)
        });
        let at = cx.new(|cx| {
            InputState::new(window, cx).default_value(default_once_at(&chrono::Local, now))
        });
        let time = cx.new(|cx| InputState::new(window, cx).default_value("09:00"));
        let every = cx.new(|cx| InputState::new(window, cx).default_value("6"));
        let account = config
            .settings
            .default_account_id
            .clone()
            .or_else(|| config.accounts.first().map(|a| a.id.clone()));
        let project = Some(config.active_workspace.clone());
        ScheduledView {
            focus: cx.focus_handle(),
            config,
            prompt,
            at,
            time,
            every,
            kind: Kind::Weekly,
            days: vec![1, 2, 3, 4, 5],
            account,
            project,
            switch_at_limit: false,
            error: None,
        }
    }

    /// New data from the window (a run recorded, a job saved).
    pub fn set_config(&mut self, config: Config, cx: &mut Context<Self>) {
        self.config = config;
        cx.notify();
    }

    pub fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.prompt.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return self.fail("Write what the chat should do.".into(), cx);
        }
        let (Some(account), Some(project)) = (self.account.clone(), self.project.clone()) else {
            return self.fail("Pick an account and a project.".into(), cx);
        };
        let when = match self.kind {
            Kind::Once => {
                let at = self.at.read(cx).value().trim().to_string();
                if !is_local_datetime(&at) {
                    return self.fail("Use a date and time like 2026-11-13T09:00.".into(), cx);
                }
                ScheduleWhen::Once { at }
            }
            Kind::Weekly => {
                let time = self.time.read(cx).value().trim().to_string();
                if !is_time(&time) || self.days.is_empty() {
                    return self.fail("Pick days and a time like 09:00.".into(), cx);
                }
                ScheduleWhen::Weekly {
                    days: self.days.clone(),
                    time,
                }
            }
            Kind::Hours => {
                let every: i64 = self.every.read(cx).value().trim().parse().unwrap_or(0);
                let from = self.time.read(cx).value().trim().to_string();
                if !(1..=24).contains(&every) || !is_time(&from) {
                    return self.fail("Every 1–24 hours, from a time like 09:00.".into(), cx);
                }
                ScheduleWhen::Hours { every, from }
            }
            Kind::Reset => ScheduleWhen::Reset {
                window: "fiveHour".into(),
            },
        };
        let s = &self.config.settings;
        let job = ScheduledJob {
            id: new_id("j"),
            ws_id: project,
            account_id: account,
            prompt,
            model: s.chat_model.clone(),
            effort: s.chat_effort,
            chat_mode: s.chat_mode,
            when,
            target: ScheduleTarget::New,
            at_limit: if self.switch_at_limit {
                "switch".into()
            } else {
                "wait".into()
            },
            enabled: true,
            one_shot: false,
            next_at: None,
            runs: vec![],
        };
        self.error = None;
        self.prompt.update(cx, |p, cx| p.set_value("", window, cx));
        cx.emit(ScheduledEvent::Save(Box::new(job)));
    }
}

impl EventEmitter<ScheduledEvent> for ScheduledView {}

impl Focusable for ScheduledView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ScheduledView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tz = chrono::Local;
        let now = wraithgrid_usage::now_ms();
        let chip = |id: String, label: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .px_2()
                .py(px(2.))
                .rounded_md()
                .border_1()
                .text_sm()
                .cursor_pointer()
                .border_color(if on { theme.ring } else { theme.border })
                .when(on, |d| d.bg(theme.secondary))
                .child(label)
        };
        let jobs = self.config.schedules.iter().map(|j| {
            let account = self.config.account(&j.account_id).map(|a| a.name.clone());
            let project = self
                .config
                .workspaces
                .iter()
                .find(|w| w.id == j.ws_id)
                .map_or_else(|| "—".into(), |w| w.name.clone());
            let next = match (j.enabled, j.next_at) {
                (false, _) => "off".to_string(),
                (true, Some(n)) => format!("next {}", fmt_when(&tz, n, now)),
                (true, None) => "waiting for its time".to_string(),
            };
            let last = j.runs.first().map(|r| {
                format!(
                    "last: {}{}",
                    r.result,
                    if r.note.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", r.note)
                    }
                )
            });
            let (toggle, delete, run) = (j.id.clone(), j.id.clone(), j.id.clone());
            let enabled = j.enabled;
            div()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().child(j.prompt.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{} · {} · {} · {}",
                            describe_when(&tz, &j.when, account.as_deref()),
                            account.clone().unwrap_or_else(|| "account missing".into()),
                            project,
                            next
                        )),
                )
                .when_some(last, |d, l| {
                    d.child(div().text_xs().text_color(theme.muted_foreground).child(l))
                })
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            chip(
                                format!("toggle-{}", j.id),
                                if enabled {
                                    "Turn off".into()
                                } else {
                                    "Turn on".into()
                                },
                                false,
                            )
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.emit(ScheduledEvent::Toggle(toggle.clone(), !enabled))
                                },
                            )),
                        )
                        .child(
                            chip(format!("run-{}", j.id), "Run now".into(), false).on_click(
                                cx.listener(move |_, _, _, cx| {
                                    cx.emit(ScheduledEvent::RunNow(run.clone()))
                                }),
                            ),
                        )
                        .child(
                            chip(format!("delete-{}", j.id), "Delete".into(), false).on_click(
                                cx.listener(move |_, _, _, cx| {
                                    cx.emit(ScheduledEvent::Delete(delete.clone()))
                                }),
                            ),
                        ),
                )
        });
        let kinds = [
            (Kind::Weekly, "Days of the week"),
            (Kind::Hours, "Every few hours"),
            (Kind::Once, "Once"),
            (Kind::Reset, "When the 5-hour limit resets"),
        ]
        .map(|(k, label)| {
            chip(format!("kind-{label}"), label.into(), self.kind == k).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.kind = k;
                    cx.notify();
                },
            ))
        });
        let days = DAYS.iter().enumerate().map(|(i, d)| {
            let i = i as i64;
            chip(format!("day-{d}"), d.to_string(), self.days.contains(&i)).on_click(cx.listener(
                move |this, _, _, cx| {
                    if let Some(at) = this.days.iter().position(|x| *x == i) {
                        this.days.remove(at);
                    } else {
                        this.days.push(i);
                        this.days.sort();
                    }
                    cx.notify();
                },
            ))
        });
        let accounts = self.config.accounts.iter().map(|a| {
            let id = a.id.clone();
            chip(
                format!("acct-{}", a.id),
                a.name.clone(),
                self.account.as_ref() == Some(&a.id),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.account = Some(id.clone());
                cx.notify();
            }))
        });
        let projects = self.config.workspaces.iter().map(|w| {
            let id = w.id.clone();
            chip(
                format!("ws-{}", w.id),
                w.name.clone(),
                self.project.as_ref() == Some(&w.id),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.project = Some(id.clone());
                cx.notify();
            }))
        });
        let label = |t: &'static str| div().text_xs().text_color(theme.muted_foreground).child(t);
        div()
            .id("scheduled")
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
                    cx.emit(ScheduledEvent::Close);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Scheduled chats"),
                    )
                    .child(
                        div()
                            .id("scheduled-close")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.secondary))
                            .child("← Back to panes")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ScheduledEvent::Close))),
                    ),
            )
            .child(
                div()
                    .max_w(px(820.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(self.config.schedules.is_empty(), |d| {
                        d.child(
                            div().text_sm().text_color(theme.muted_foreground).child(
                                "No scheduled chats yet. They run while Wraithgrid is open.",
                            ),
                        )
                    })
                    .children(jobs),
            )
            .child(
                div()
                    .max_w(px(820.))
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child("New scheduled chat"),
                    )
                    .child(Textarea::new(&self.prompt))
                    .child(label("When"))
                    .child(div().flex().flex_wrap().gap_2().children(kinds))
                    .when(self.kind == Kind::Weekly, |d| {
                        d.child(div().flex().gap_1().children(days))
                            .child(div().w(px(120.)).child(Input::new(&self.time)))
                    })
                    .when(self.kind == Kind::Hours, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child("every")
                                .child(div().w(px(60.)).child(Input::new(&self.every)))
                                .child("hours from")
                                .child(div().w(px(100.)).child(Input::new(&self.time))),
                        )
                    })
                    .when(self.kind == Kind::Once, |d| {
                        d.child(div().w(px(220.)).child(Input::new(&self.at)))
                    })
                    .child(label("Account"))
                    .child(div().flex().flex_wrap().gap_2().children(accounts))
                    .child(label("Project (a new chat starts in its folder)"))
                    .child(div().flex().flex_wrap().gap_2().children(projects))
                    .child(label("At the account's limit"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                chip(
                                    "limit-wait".into(),
                                    "Wait for the reset".into(),
                                    !self.switch_at_limit,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.switch_at_limit = false;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                chip(
                                    "limit-switch".into(),
                                    "Switch to another account".into(),
                                    self.switch_at_limit,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.switch_at_limit = true;
                                        cx.notify();
                                    },
                                )),
                            ),
                    )
                    .when_some(self.error.clone(), |d, e| {
                        d.child(div().text_sm().text_color(theme.danger).child(e))
                    })
                    .child(
                        div().flex().justify_end().child(
                            div()
                                .id("schedule-add")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(theme.primary)
                                .text_color(theme.primary_foreground)
                                .cursor_pointer()
                                .child("Schedule")
                                .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                        ),
                    ),
            )
    }
}
