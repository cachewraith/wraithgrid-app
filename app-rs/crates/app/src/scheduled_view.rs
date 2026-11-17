//! Scheduled chats (`ScheduledView.tsx`, #77): queued "send later" messages, the jobs with
//! their next and last run, and the dialog that makes or edits one. When a job runs, and what a
//! run does, is the `wraithgrid-schedule` crate's; the window runs them.

use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::a11y::A11y as _;
use wraithgrid_core::config::{
    is_chat_model, Config, ScheduleTarget, ScheduleWhen, ScheduledJob, Workspace,
};
use wraithgrid_core::ids::new_id;
use wraithgrid_core::types::{ChatEffort, ChatMode};
use wraithgrid_schedule::{
    default_once_at, describe_when, fmt_when, is_time, parse_local, upcoming,
};
use wraithgrid_usage::format::fmt_until;

pub enum ScheduledEvent {
    /// Save a job; `folder`: also give its project this folder (it had none).
    Save(Box<ScheduledJob>, Option<String>),
    Toggle(String, bool),
    Delete(String),
    RunNow(String),
    /// Show a chat a job ran in: (project, pane).
    OpenChat(String, String),
    Close,
}

const DAYS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

/// `ago`: `now`, `5m`, `3h`, `2d`, `4w`.
fn ago(then: i64, now: i64) -> String {
    let s = ((now - then) / 1000).max(0);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=604_799 => format!("{}d", s / 86_400),
        _ => format!("{}w", s / 604_800),
    }
}

/// The machine's time zone name (`Europe/Paris`), else its UTC offset.
fn zone_name() -> String {
    if let Some(tz) = std::env::var("TZ").ok().filter(|t| !t.is_empty()) {
        return tz.trim_start_matches(':').to_string();
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| {
            let p = p.to_string_lossy().into_owned();
            p.split_once("zoneinfo/").map(|(_, z)| z.to_string())
        })
        .unwrap_or_else(|| chrono::Local::now().format("UTC%:z").to_string())
}

fn mode_label(m: ChatMode) -> &'static str {
    match m {
        ChatMode::Default => "Ask before edits",
        ChatMode::AcceptEdits => "Accept edits",
        ChatMode::Plan => "Plan",
        ChatMode::Auto => "Auto",
        ChatMode::DontAsk => "Don't ask",
        ChatMode::BypassPermissions => "Full access",
    }
}

/// Where a job's run goes, as its card says it.
fn target_text(job: &ScheduledJob, ws: Option<&Workspace>) -> String {
    match &job.target {
        ScheduleTarget::New => "new chat".into(),
        ScheduleTarget::Chat { pane_id } => {
            let title = ws.and_then(|w| {
                w.panes
                    .iter()
                    .find(|p| &p.id == pane_id)
                    .or_else(|| w.history.iter().map(|h| &h.pane).find(|p| &p.id == pane_id))
                    .map(|p| p.title.clone())
            });
            format!("continues “{}”", title.as_deref().unwrap_or("a chat"))
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

pub struct ScheduledView {
    focus: FocusHandle,
    config: Config,
    /// The job whose actions are open.
    menu: Option<String>,
    dialog: Option<Entity<JobDialog>>,
}

impl ScheduledView {
    pub fn new(config: Config, cx: &mut Context<Self>) -> Self {
        // Next/last times move on their own: draw again every 30s.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(30))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        })
        .detach();
        ScheduledView {
            focus: cx.focus_handle(),
            config,
            menu: None,
            dialog: None,
        }
    }

    /// New data from the window (a run recorded, a job saved).
    pub fn set_config(&mut self, config: Config, cx: &mut Context<Self>) {
        self.config = config;
        cx.notify();
    }

    pub fn open_job(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let existing = id.and_then(|id| self.config.schedules.iter().find(|j| j.id == id).cloned());
        let config = self.config.clone();
        let dialog = cx.new(|cx| JobDialog::new(&config, existing, window, cx));
        cx.subscribe_in(&dialog, window, |this, _, event: &JobEvent, window, cx| {
            if let JobEvent::Save(job, folder) = event {
                cx.emit(ScheduledEvent::Save(job.clone(), folder.clone()));
            }
            this.dialog = None;
            window.focus(&this.focus, cx);
            cx.notify();
        })
        .detach();
        dialog.update(cx, |d, cx| d.focus(window, cx));
        self.dialog = Some(dialog);
        cx.notify();
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
        let small = |id: String, label: &str| {
            div()
                .id(SharedString::from(id))
                .a11y_button(label.to_string())
                .px_2()
                .py(px(2.))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .text_xs()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(label.to_string())
        };
        let muted = |t: String| div().text_xs().text_color(theme.muted_foreground).child(t);
        let next_text = |j: &ScheduledJob| match j.next_at {
            Some(n) => format!(
                "{} · {}",
                fmt_when(&tz, n, now),
                if n > now {
                    format!("in {}", fmt_until(n, now))
                } else {
                    "now".into()
                }
            ),
            None if matches!(j.when, ScheduleWhen::Reset { .. }) => "after the reset".into(),
            None => "–".into(),
        };
        let ws_of = |id: &str| self.config.workspaces.iter().find(|w| w.id == id);
        let jobs: Vec<&ScheduledJob> = self
            .config
            .schedules
            .iter()
            .filter(|j| !j.one_shot)
            .collect();
        let queued: Vec<&ScheduledJob> = self
            .config
            .schedules
            .iter()
            .filter(|j| j.one_shot)
            .collect();

        let queued_cards = queued.iter().map(|j| {
            let ws = ws_of(&j.ws_id);
            let title = match &j.target {
                ScheduleTarget::Chat { pane_id } => ws
                    .and_then(|w| w.panes.iter().find(|p| &p.id == pane_id))
                    .map(|p| p.title.clone()),
                ScheduleTarget::New => None,
            };
            let (cancel, send) = (j.id.clone(), j.id.clone());
            div()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .flex()
                .items_center()
                .gap_3()
                .child("◷")
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .child(div().text_sm().truncate().child(first_line(&j.prompt)))
                        .child(muted(format!(
                            "{} · “{}” · {}",
                            ws.map_or("project missing", |w| w.name.as_str()),
                            title.as_deref().unwrap_or("a chat"),
                            next_text(j)
                        ))),
                )
                .child(small(format!("q-cancel-{}", j.id), "Cancel").on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(ScheduledEvent::Delete(cancel.clone()))),
                ))
                .child(small(format!("q-send-{}", j.id), "Send now").on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(ScheduledEvent::RunNow(send.clone()))),
                ))
        });

        let cards = jobs.iter().map(|j| {
            let ws = ws_of(&j.ws_id);
            let account = self.config.account(&j.account_id);
            let last = j.runs.first();
            let id = j.id.clone();
            let enabled = j.enabled;
            let tone = |r: &str| match r {
                "done" => theme.success,
                "failed" | "missed" => theme.danger,
                "running" => theme.primary,
                _ => theme.warning,
            };
            let menu_open = self.menu.as_ref() == Some(&j.id);
            let last_chat = j
                .runs
                .iter()
                .find_map(|r| r.pane_id.clone())
                .map(|p| (j.ws_id.clone(), p));
            let switch = {
                let id = id.clone();
                div()
                    .id(SharedString::from(format!("sw-{}", j.id)))
                    .role(Role::Switch)
                    .aria_label(format!("Run {}", first_line(&j.prompt)))
                    .aria_toggled(if enabled {
                        Toggled::True
                    } else {
                        Toggled::False
                    })
                    .w(px(30.))
                    .h(px(16.))
                    .rounded_full()
                    .p(px(2.))
                    .flex()
                    .when(enabled, |d| d.justify_end())
                    .bg(if enabled {
                        theme.primary
                    } else {
                        theme.muted_foreground.opacity(0.35)
                    })
                    .cursor_pointer()
                    .child(div().size(px(12.)).rounded_full().bg(theme.background))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(ScheduledEvent::Toggle(id.clone(), !enabled));
                    }))
            };
            let actions = {
                let (run, edit, dup, del) = (id.clone(), id.clone(), (*j).clone(), id.clone());
                div()
                    .flex()
                    .gap_2()
                    .pt_1()
                    .child(
                        small(format!("run-{}", j.id), "Run now").on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.menu = None;
                                cx.emit(ScheduledEvent::RunNow(run.clone()));
                            },
                        )),
                    )
                    .child(
                        small(format!("edit-{}", j.id), "Edit").on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_job(Some(edit.clone()), window, cx)
                            },
                        )),
                    )
                    .child(
                        small(format!("dup-{}", j.id), "Duplicate").on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.menu = None;
                                let mut copy = dup.clone();
                                copy.id = new_id("j");
                                copy.runs.clear();
                                copy.next_at = None;
                                cx.emit(ScheduledEvent::Save(Box::new(copy), None));
                            },
                        )),
                    )
                    .when_some(last_chat, |d, (ws, pane)| {
                        d.child(small(format!("last-{}", j.id), "Open last chat").on_click(
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.menu = None;
                                cx.emit(ScheduledEvent::OpenChat(ws.clone(), pane.clone()));
                            }),
                        ))
                    })
                    .child(
                        small(format!("del-{}", j.id), "Delete")
                            .text_color(theme.danger)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.menu = None;
                                cx.emit(ScheduledEvent::Delete(del.clone()));
                            })),
                    )
            };
            let open = id.clone();
            let toggle_menu = id.clone();
            div()
                .id(SharedString::from(format!("job-{}", j.id)))
                .role(Role::Group)
                .aria_label(first_line(&j.prompt))
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|d| d.border_color(theme.ring))
                .when(!enabled, |d| d.opacity(0.6))
                .flex()
                .flex_col()
                .gap_1()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_job(Some(open.clone()), window, cx)
                }))
                .on_mouse_down(MouseButton::Right, {
                    let id = id.clone();
                    cx.listener(move |this, _, _, cx| {
                        this.menu = Some(id.clone());
                        cx.notify();
                    })
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(muted(format!(
                            "{} · {}",
                            ws.map_or("project missing", |w| w.name.as_str()),
                            account.map_or("account missing", |a| a.name.as_str())
                        )))
                        .child(div().flex_1())
                        .child(switch),
                )
                .child(div().text_sm().truncate().child(first_line(&j.prompt)))
                .child(muted(format!(
                    "{}{} · {}",
                    target_text(j, ws),
                    j.chat_mode.map_or(String::new(), |m| format!(
                        " · {}",
                        mode_label(m).to_lowercase()
                    )),
                    describe_when(&tz, &j.when, account.map(|a| a.name.as_str()))
                )))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .pt_1()
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .text_xs()
                                .child(div().text_color(theme.muted_foreground).child("Next"))
                                .child(if enabled { next_text(j) } else { "off".into() }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .gap_1()
                                .text_xs()
                                .child(div().text_color(theme.muted_foreground).child("Last"))
                                .child(match last {
                                    Some(r) => div()
                                        .flex()
                                        .gap_1()
                                        .min_w(px(0.))
                                        .child(
                                            div()
                                                .text_color(tone(&r.result))
                                                .child(r.result.clone()),
                                        )
                                        .child("·")
                                        .child(div().truncate().child(
                                            if !r.note.is_empty() && r.result != "done" {
                                                r.note.clone()
                                            } else {
                                                ago(r.at, now)
                                            },
                                        )),
                                    None => div().child("–"),
                                }),
                        )
                        .child(
                            small(format!("more-{}", j.id), "⋯")
                                .aria_label("Job actions")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.menu = if this.menu.as_ref() == Some(&toggle_menu) {
                                        None
                                    } else {
                                        Some(toggle_menu.clone())
                                    };
                                    cx.notify();
                                })),
                        ),
                )
                .when(menu_open, |d| d.child(actions))
        });

        div()
            .id("scheduled")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .bg(theme.background)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" && this.dialog.is_none() {
                    cx.emit(ScheduledEvent::Close);
                }
            }))
            .child(
                div()
                    .id("scheduled-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .max_w(px(820.))
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Scheduled"),
                                    )
                                    .child(muted(format!(
                                        "Runs while Wraithgrid or its tray icon is open · times in {}",
                                        zone_name()
                                    ))),
                            )
                            .child(
                                div()
                                    .id("job-new")
                                    .a11y_button("New job")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(theme.primary)
                                    .text_color(theme.primary_foreground)
                                    .cursor_pointer()
                                    .child("+ New job")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_job(None, window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .id("scheduled-close")
                                    .a11y_button("Back to panes")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(theme.muted_foreground)
                                    .hover(|d| d.bg(theme.secondary))
                                    .child("← Back to panes")
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(ScheduledEvent::Close)),
                                    ),
                            ),
                    )
                    .when(!queued.is_empty(), |d| {
                        d.child(
                            div()
                                .max_w(px(820.))
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(format!("Queued messages  {}", queued.len())),
                                )
                                .children(queued_cards),
                        )
                    })
                    .child(if jobs.is_empty() {
                        div()
                            .max_w(px(820.))
                            .py_6()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("No scheduled chats"),
                            )
                            .child(muted("Start chats on a schedule, or when a limit resets.".into()))
                    } else {
                        div()
                            .max_w(px(820.))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .when(!queued.is_empty(), |d| {
                                d.child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(format!("Jobs  {}", jobs.len())),
                                )
                            })
                            .children(cards)
                    }),
            )
            .when_some(self.dialog.clone(), |d, dialog| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(gpui_kit::black().opacity(0.4))
                        .flex()
                        .justify_center()
                        .items_center()
                        .child(dialog),
                )
            })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Once,
    Daily,
    Weekly,
    Hours,
    Reset,
}

pub enum JobEvent {
    Save(Box<ScheduledJob>, Option<String>),
    Cancel,
}

/// New job, or edit one: project, account, prompt, when, where, model/effort/mode.
pub struct JobDialog {
    config: Config,
    job: ScheduledJob,
    editing: bool,
    now: i64,
    kind: Kind,
    prompt: Entity<TextareaState>,
    folder: Entity<InputState>,
    at: Entity<InputState>,
    time: Entity<InputState>,
    every: Entity<InputState>,
    from: Entity<InputState>,
    model: Entity<InputState>,
}

impl JobDialog {
    fn new(
        config: &Config,
        existing: Option<ScheduledJob>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let now = wraithgrid_usage::now_ms();
        let editing = existing.is_some();
        let job = existing.unwrap_or_else(|| {
            let active = &config.active_workspace;
            ScheduledJob {
                id: new_id("j"),
                ws_id: active.clone(),
                account_id: config
                    .workspaces
                    .iter()
                    .find(|w| &w.id == active)
                    .and_then(|w| w.account_id.clone())
                    .or_else(|| config.accounts.first().map(|a| a.id.clone()))
                    .unwrap_or_default(),
                prompt: String::new(),
                model: None,
                effort: None,
                chat_mode: None,
                when: ScheduleWhen::Weekly {
                    days: vec![1, 2, 3, 4, 5],
                    time: "09:00".into(),
                },
                target: ScheduleTarget::New,
                at_limit: "wait".into(),
                enabled: true,
                one_shot: false,
                next_at: None,
                runs: vec![],
            }
        });
        let kind = match &job.when {
            ScheduleWhen::Once { .. } => Kind::Once,
            ScheduleWhen::Weekly { days, .. } if days.len() == 7 => Kind::Daily,
            ScheduleWhen::Weekly { .. } => Kind::Weekly,
            ScheduleWhen::Hours { .. } => Kind::Hours,
            ScheduleWhen::Reset { .. } => Kind::Reset,
        };
        let once = default_once_at(&chrono::Local, now);
        let (at, time, every, from) = match &job.when {
            ScheduleWhen::Once { at } => (at.clone(), "09:00".into(), "6".into(), "09:00".into()),
            ScheduleWhen::Weekly { time, .. } => (once, time.clone(), "6".into(), "09:00".into()),
            ScheduleWhen::Hours { every, from } => {
                (once, "09:00".into(), every.to_string(), from.clone())
            }
            ScheduleWhen::Reset { .. } => (once, "09:00".into(), "6".into(), "09:00".into()),
        };
        let input = |value: String,
                     placeholder: &'static str,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let prompt_text = job.prompt.clone();
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Review the open PRs in this repo and list which ones need me.")
                .auto_grow(3, 8)
                .default_value(prompt_text)
        });
        let folder = input(String::new(), "~/code/my-app", window, cx);
        let at = input(at, "2026-11-13T09:00", window, cx);
        let time = input(time, "09:00", window, cx);
        let every = input(every, "6", window, cx);
        let from = input(from, "09:00", window, cx);
        let model = input(job.model.clone().unwrap_or_default(), "default", window, cx);
        // Typing changes the preview and what Save allows: draw again.
        for i in [&folder, &at, &time, &every, &from, &model] {
            cx.observe(i, |_, _, cx| cx.notify()).detach();
        }
        cx.observe(&prompt, |_, _, cx| cx.notify()).detach();
        JobDialog {
            config: config.clone(),
            job,
            editing,
            now,
            kind,
            prompt,
            folder,
            at,
            time,
            every,
            from,
            model,
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt.update(cx, |p, cx| p.focus(window, cx));
    }

    fn value(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().trim().to_string()
    }

    fn days(&self) -> Vec<i64> {
        match &self.job.when {
            ScheduleWhen::Weekly { days, .. } if days.len() < 7 => days.clone(),
            _ => vec![1, 2, 3, 4, 5],
        }
    }

    /// What the date, time and hours fields can't take, by field, for the form to show.
    fn problems(&self, cx: &App) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let time_ok = |t: &str| t.is_empty() || is_time(t);
        match self.kind {
            Kind::Once => {
                let at = Self::value(&self.at, cx);
                match parse_local(&chrono::Local, &at) {
                    None => out.push(("at", "A date and time like 2026-11-13T09:00.".to_string())),
                    Some(t) if t <= self.now => {
                        out.push(("at", "That time has passed.".to_string()))
                    }
                    Some(_) => {}
                }
            }
            Kind::Daily | Kind::Weekly => {
                if !time_ok(&Self::value(&self.time, cx)) {
                    out.push(("time", "A time like 09:00 (00:00–23:59).".to_string()));
                }
            }
            Kind::Hours => {
                let every = Self::value(&self.every, cx);
                if !every.parse::<i64>().is_ok_and(|n| (1..=24).contains(&n)) {
                    out.push(("every", "A whole number of hours, 1 to 24.".to_string()));
                }
                if !time_ok(&Self::value(&self.from, cx)) {
                    out.push(("from", "A time like 09:00 (00:00–23:59).".to_string()));
                }
            }
            Kind::Reset => {}
        }
        out
    }

    /// Picks the project's folder with the system's folder dialog.
    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let home = wraithgrid_core::paths::home_dir()
                .to_string_lossy()
                .into_owned();
            let dir = wraithgrid_core::paths::contract_home(&dir.to_string_lossy(), &home);
            let _ = this.update_in(cx, |this, window, cx| {
                this.folder.update(cx, |i, cx| i.set_value(dir, window, cx));
            });
        })
        .detach();
    }

    /// The time as the form says it now.
    fn when(&self, cx: &App) -> ScheduleWhen {
        let time = || {
            let t = Self::value(&self.time, cx);
            if t.is_empty() {
                "09:00".into()
            } else {
                t
            }
        };
        match self.kind {
            Kind::Once => ScheduleWhen::Once {
                at: Self::value(&self.at, cx),
            },
            Kind::Daily => ScheduleWhen::Weekly {
                days: (0..7).collect(),
                time: time(),
            },
            Kind::Weekly => ScheduleWhen::Weekly {
                days: self.days(),
                time: time(),
            },
            Kind::Hours => {
                let from = Self::value(&self.from, cx);
                ScheduleWhen::Hours {
                    every: Self::value(&self.every, cx)
                        .parse::<i64>()
                        .unwrap_or(1)
                        .clamp(1, 24),
                    from: if from.is_empty() {
                        "09:00".into()
                    } else {
                        from
                    },
                }
            }
            Kind::Reset => ScheduleWhen::Reset {
                window: match &self.job.when {
                    ScheduleWhen::Reset { window } => window.clone(),
                    _ => "fiveHour".into(),
                },
            },
        }
    }

    fn workspace(&self) -> Option<&Workspace> {
        self.config
            .workspaces
            .iter()
            .find(|w| w.id == self.job.ws_id)
    }

    fn needs_folder(&self) -> bool {
        self.workspace()
            .is_some_and(|w| w.project_folder().is_none())
    }

    /// The job as it would be saved, if the form is complete.
    fn ready(&self, cx: &App) -> Option<ScheduledJob> {
        let prompt = self.prompt.read(cx).value().trim().to_string();
        let model = Self::value(&self.model, cx);
        let when = self.when(cx);
        let when_ok = match &when {
            ScheduleWhen::Once { at } => {
                parse_local(&chrono::Local, at).is_some_and(|t| t > self.now)
            }
            ScheduleWhen::Weekly { days, .. } => !days.is_empty(),
            _ => true,
        };
        let ok = self.workspace().is_some()
            && self.problems(cx).is_empty()
            && !self.job.account_id.is_empty()
            && !prompt.is_empty()
            && (model.is_empty() || is_chat_model(&model))
            && when_ok
            && (!self.needs_folder() || !Self::value(&self.folder, cx).is_empty());
        ok.then(|| ScheduledJob {
            prompt: prompt.chars().take(8000).collect(),
            model: (!model.is_empty()).then_some(model),
            when,
            ..self.job.clone()
        })
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = self.ready(cx) {
            let folder = self.needs_folder().then(|| Self::value(&self.folder, cx));
            cx.emit(JobEvent::Save(Box::new(job), folder));
        }
    }

    fn set_kind(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = kind;
        match kind {
            Kind::Once => {
                let at = default_once_at(&chrono::Local, self.now);
                self.at.update(cx, |i, cx| i.set_value(at, window, cx));
            }
            Kind::Weekly => {
                self.job.when = ScheduleWhen::Weekly {
                    days: vec![1, 2, 3, 4, 5],
                    time: "09:00".into(),
                }
            }
            Kind::Hours => {
                self.every.update(cx, |i, cx| i.set_value("6", window, cx));
                self.from
                    .update(cx, |i, cx| i.set_value("09:00", window, cx));
            }
            Kind::Reset => {
                self.job.when = ScheduleWhen::Reset {
                    window: "fiveHour".into(),
                }
            }
            Kind::Daily => {}
        }
        cx.notify();
    }
}

impl EventEmitter<JobEvent> for JobDialog {}

impl Render for JobDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tz = chrono::Local;
        let chip = |id: String, label: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .a11y_toggle(label.clone(), on)
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
        let label = |t: &str| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t.to_string())
        };
        let row = || div().flex().flex_wrap().gap_2();

        let projects = self.config.workspaces.iter().map(|w| {
            let id = w.id.clone();
            chip(
                format!("job-ws-{}", w.id),
                w.name.clone(),
                self.job.ws_id == w.id,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.job.ws_id = id.clone();
                this.job.target = ScheduleTarget::New;
                cx.notify();
            }))
        });
        let accounts = self.config.accounts.iter().map(|a| {
            let id = a.id.clone();
            chip(
                format!("job-acct-{}", a.id),
                format!("{} · {}", a.name, a.cli.spec().label),
                self.job.account_id == a.id,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.job.account_id = id.clone();
                cx.notify();
            }))
        });
        let kinds = [
            (Kind::Once, "Once"),
            (Kind::Daily, "Daily"),
            (Kind::Weekly, "Weekly"),
            (Kind::Hours, "Every N hours"),
            (Kind::Reset, "Limit resets"),
        ]
        .map(|(k, l)| {
            chip(format!("job-kind-{l}"), l.into(), self.kind == k)
                .on_click(cx.listener(move |this, _, window, cx| this.set_kind(k, window, cx)))
        });
        let days = self.days();
        let day_chips = DAYS.iter().enumerate().map(|(i, d)| {
            let i = i as i64;
            chip(format!("job-day-{d}"), d.to_string(), days.contains(&i)).on_click(cx.listener(
                move |this, _, _, cx| {
                    let mut days = this.days();
                    if let Some(at) = days.iter().position(|x| *x == i) {
                        days.remove(at);
                    } else {
                        days.push(i);
                        days.sort();
                    }
                    this.job.when = ScheduleWhen::Weekly {
                        days,
                        time: "09:00".into(),
                    };
                    cx.notify();
                },
            ))
        });
        let reset_window = match &self.job.when {
            ScheduleWhen::Reset { window } => window.clone(),
            _ => "fiveHour".into(),
        };
        let windows = [("fiveHour", "5-hour"), ("sevenDay", "Weekly")].map(|(w, l)| {
            chip(format!("job-win-{w}"), l.into(), reset_window == w).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.job.when = ScheduleWhen::Reset { window: w.into() };
                    cx.notify();
                },
            ))
        });
        let chats: Vec<(String, String)> = self
            .workspace()
            .map(|w| {
                w.panes
                    .iter()
                    .chain(w.history.iter().map(|h| &h.pane))
                    .filter(|p| !p.shell)
                    .map(|p| (p.id.clone(), p.title.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let target = match &self.job.target {
            ScheduleTarget::New => None,
            ScheduleTarget::Chat { pane_id } => Some(pane_id.clone()),
        };
        let wheres = std::iter::once((None, "New chat in the project".to_string()))
            .chain(
                chats
                    .into_iter()
                    .map(|(id, t)| (Some(id), format!("Continue “{t}”"))),
            )
            .enumerate()
            .map(|(n, (id, l))| {
                let on = target == id;
                chip(format!("job-where-{n}"), l, on).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.job.target = match &id {
                            None => ScheduleTarget::New,
                            Some(p) => ScheduleTarget::Chat { pane_id: p.clone() },
                        };
                        cx.notify();
                    },
                ))
            });
        let efforts = std::iter::once(None)
            .chain(ChatEffort::ALL.iter().copied().map(Some))
            .map(|e| {
                let name = e.map_or("default", |e| e.as_str());
                chip(
                    format!("job-effort-{name}"),
                    name.into(),
                    self.job.effort == e,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.job.effort = e;
                    cx.notify();
                }))
            });
        let modes = ChatMode::ALL.iter().copied().map(|m| {
            let on = self.job.chat_mode.unwrap_or(ChatMode::Default) == m;
            chip(format!("job-mode-{}", m.as_str()), mode_label(m).into(), on).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.job.chat_mode = (m != ChatMode::Default).then_some(m);
                    cx.notify();
                }),
            )
        });
        let limit = |value: &'static str, text: &'static str, note: Option<&'static str>| {
            let on = self.job.at_limit == value;
            div()
                .id(SharedString::from(format!("job-limit-{value}")))
                .role(Role::RadioButton)
                .aria_label(text)
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .flex()
                .gap_2()
                .cursor_pointer()
                .child(
                    div()
                        .mt(px(3.))
                        .size(px(12.))
                        .flex_shrink_0()
                        .rounded_full()
                        .border_1()
                        .border_color(if on { theme.primary } else { theme.border })
                        .when(on, |d| d.bg(theme.primary)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().text_sm().child(text))
                        .when_some(note, |d, n| {
                            d.child(div().text_xs().text_color(theme.muted_foreground).child(n))
                        }),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.job.at_limit = value.into();
                    cx.notify();
                }))
        };
        let next: Vec<String> = upcoming(&tz, &self.when(cx), self.now, 3)
            .into_iter()
            .map(|t| fmt_when(&tz, t, self.now))
            .collect();
        let ready = self.ready(cx).is_some();
        let needs_folder = self.needs_folder();
        let problems = self.problems(cx);
        let problem = |field: &str| {
            problems
                .iter()
                .find(|(f, _)| *f == field)
                .map(|(_, e)| div().text_xs().text_color(theme.danger).child(e.clone()))
        };

        div()
            .id("job-dialog")
            .a11y_dialog(if self.editing { "Edit job" } else { "New job" })
            .w(px(640.))
            .max_h(relative(0.94))
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(JobEvent::Cancel);
                }
            }))
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if self.editing {
                        "Edit scheduled job"
                    } else {
                        "New scheduled job"
                    }),
            )
            .child(label("Project"))
            .child(row().children(projects))
            .child(label("Account"))
            .child(row().children(accounts))
            .when(needs_folder, |d| {
                d.child(label("Folder"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.folder)))
                            .child(
                                div()
                                    .id("job-folder-choose")
                                    .a11y_button("Choose a folder")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme.border)
                                    .text_sm()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(theme.secondary))
                                    .child("Choose…")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.choose_folder(window, cx)
                                    })),
                            ),
                    )
                    .child(div().text_xs().text_color(theme.warning).child(
                        "This project has no folder. Pick one: the job's chats start there, and it's saved on the project.",
                    ))
            })
            .child(label("Prompt"))
            .child(Textarea::new(&self.prompt))
            .child(label("When"))
            .child(row().children(kinds))
            .when(self.kind == Kind::Once, |d| {
                d.child(label("Date and time"))
                    .child(div().w(px(220.)).child(Input::new(&self.at)))
                    .children(problem("at"))
            })
            .when(self.kind == Kind::Weekly, |d| {
                d.child(label("Days")).child(row().children(day_chips))
            })
            .when(matches!(self.kind, Kind::Weekly | Kind::Daily), |d| {
                d.child(label("Time"))
                    .child(div().w(px(120.)).child(Input::new(&self.time)))
                    .children(problem("time"))
            })
            .when(self.kind == Kind::Hours, |d| {
                d.child(
                    div()
                        .flex()
                        .gap_4()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(label("Every (hours, 1–24)"))
                                .child(div().w(px(80.)).child(Input::new(&self.every)))
                                .children(problem("every")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(label("Starting at"))
                                .child(div().w(px(120.)).child(Input::new(&self.from)))
                                .children(problem("from")),
                        ),
                )
            })
            .when(self.kind == Kind::Reset, |d| {
                d.child(label("Which limit")).child(row().children(windows))
            })
            .child(label("Where"))
            .child(row().children(wheres))
            .child(label("Model"))
            .child(div().w(px(240.)).child(Input::new(&self.model)))
            .child(label("Effort"))
            .child(row().children(efforts))
            .child(label("Mode"))
            .child(row().children(modes))
            .when(
                self.job.chat_mode == Some(ChatMode::BypassPermissions),
                |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme.warning)
                            .child("Runs every tool without asking, unattended."),
                    )
                },
            )
            .child(label("If the account is at its limit"))
            .child(limit("wait", "Wait for the reset", None))
            .child(limit(
                "switch",
                "Let “Switch account at limit” carry it to another account",
                Some("Only when that setting is on; never another account otherwise."),
            ))
            .when(!next.is_empty(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("Next runs: {}", next.join(" · "))),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(
                        div()
                            .id("job-cancel")
                            .a11y_button("Cancel")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|d| d.bg(theme.secondary))
                            .child("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(JobEvent::Cancel))),
                    )
                    .child(
                        div()
                            .id("job-save")
                            .a11y_button("Save job")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .when(!ready, |d| d.opacity(0.5))
                            .when(ready, |d| {
                                d.cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.save(cx)))
                            })
                            .child("Save job"),
                    ),
            )
    }
}
