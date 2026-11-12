//! A project's page (`ProjectView.tsx`), as Claude's Projects page shows one: its open chats,
//! and the ones closed here (History), which reopen and carry on their conversation; a search
//! over both, and deleting a chat after asking (`DeleteChatDialog.tsx`).

use std::collections::HashMap;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::config::{Config, Pane};
use wraithgrid_core::paths::{contract_home, expand_home, home_dir};

use crate::team_section::TeamSection;

pub enum ProjectPageEvent {
    /// Show an open chat.
    Reveal(String),
    /// Reopen a History chat: (project, pane).
    Reopen(String, String),
    /// Delete a chat, open or in History: (project, pane).
    Delete(String, String),
    NewChat,
    Close,
}

/// Every word of the query appears in the title, account or folder (`matchesQuery`).
fn matches(q: &str, fields: &[&str]) -> bool {
    let hay = fields.join(" ").to_lowercase();
    q.to_lowercase().split_whitespace().all(|w| hay.contains(w))
}

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

/// `PANE_STATUS_LABEL`.
fn status_label(s: &str) -> &'static str {
    match s {
        "starting" => "starting",
        "running" => "running",
        "background" => "in background",
        "done" => "done",
        "approval" => "needs approval",
        "exited" => "exited",
        "login" => "login needed",
        _ => "idle",
    }
}

/// Asked before a chat is deleted: (pane, title, open, busy).
struct Confirm {
    pane: String,
    title: String,
    open: bool,
    busy: bool,
}

pub struct ProjectView {
    focus: FocusHandle,
    config: Config,
    ws_id: String,
    /// Each pane's status, as `PaneStatus` names it.
    statuses: HashMap<String, &'static str>,
    query: Entity<InputState>,
    confirm: Option<Confirm>,
    /// The project's team (#79), between the header and the search.
    team: Entity<TeamSection>,
}

impl ProjectView {
    pub fn new(
        config: Config,
        ws_id: String,
        statuses: HashMap<String, &'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search chats…"));
        cx.observe(&query, |_, _, cx| cx.notify()).detach();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(30))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        })
        .detach();
        let team = cx.new(|_| TeamSection::new(config.clone(), ws_id.clone(), statuses.clone()));
        ProjectView {
            focus: cx.focus_handle(),
            config,
            ws_id,
            statuses,
            query,
            confirm: None,
            team,
        }
    }

    pub fn team(&self) -> Entity<TeamSection> {
        self.team.clone()
    }

    pub fn set_data(
        &mut self,
        config: Config,
        statuses: HashMap<String, &'static str>,
        cx: &mut Context<Self>,
    ) {
        self.team
            .update(cx, |t, cx| t.set_data(config.clone(), statuses.clone(), cx));
        self.config = config;
        self.statuses = statuses;
        cx.notify();
    }

    fn ask_delete(&mut self, pane: &Pane, open: bool, cx: &mut Context<Self>) {
        let busy = matches!(
            self.statuses.get(&pane.id).copied(),
            Some("running" | "starting" | "approval" | "background")
        );
        self.confirm = Some(Confirm {
            pane: pane.id.clone(),
            title: pane.title.clone(),
            open,
            busy,
        });
        cx.notify();
    }
}

impl EventEmitter<ProjectPageEvent> for ProjectView {}

impl Focusable for ProjectView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let now = wraithgrid_usage::now_ms();
        let Some(ws) = self
            .config
            .workspaces
            .iter()
            .find(|w| w.id == self.ws_id)
            .or_else(|| self.config.workspaces.first())
            .cloned()
        else {
            return div().id("project");
        };
        let name_of = |id: Option<&String>| {
            id.and_then(|id| self.config.account(id))
                .map_or("shell".to_string(), |a| a.name.clone())
        };
        let home = home_dir().to_string_lossy().into_owned();
        let mut folders: Vec<&str> = ws
            .panes
            .iter()
            .chain(ws.history.iter().map(|h| &h.pane))
            .map(|p| p.cwd.as_str())
            .collect();
        folders.dedup();
        folders.sort();
        folders.dedup();
        let folder =
            (folders.len() == 1).then(|| contract_home(&expand_home(folders[0], &home), &home));
        let q = self.query.read(cx).value().to_string();
        let open: Vec<&Pane> = ws
            .panes
            .iter()
            .filter(|p| matches(&q, &[&p.title, &name_of(p.account_id.as_ref()), &p.cwd]))
            .collect();
        let history: Vec<_> = ws
            .history
            .iter()
            .filter(|h| {
                matches(
                    &q,
                    &[
                        &h.pane.title,
                        &name_of(h.pane.account_id.as_ref()),
                        &h.pane.cwd,
                    ],
                )
            })
            .collect();
        let total = ws.panes.len() + ws.history.len();
        let muted = |t: String| div().text_xs().text_color(theme.muted_foreground).child(t);
        let row = |id: String, title: String, account: String, state: AnyElement| {
            div()
                .id(SharedString::from(id))
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(div().flex_1().min_w(px(0.)).truncate().child(title))
                .child(muted(account))
                .child(div().w(px(110.)).flex().justify_end().child(state))
        };
        let trash = |id: String| {
            div()
                .id(SharedString::from(id))
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .text_color(theme.muted_foreground)
                .hover(|d| d.text_color(theme.danger).bg(theme.secondary))
                .child("🗑")
        };
        let open_rows =
            open.iter().map(|p| {
                let status = self.statuses.get(&p.id).copied();
                let busy = matches!(status, Some("running" | "starting" | "background"));
                let loud = matches!(status, Some("approval" | "login" | "exited"));
                let text = match status {
                    Some("background") => status_label("background").to_string(),
                    Some(s) if loud => status_label(s).to_string(),
                    Some(s) if busy => status_label(s).to_string(),
                    _ => "open".to_string(),
                };
                let color = if loud {
                    theme.warning
                } else if busy {
                    theme.success
                } else {
                    theme.muted_foreground
                };
                let (reveal, del) = (p.id.clone(), (*p).clone());
                div()
                    .flex()
                    .items_center()
                    .child(
                        row(
                            format!("pj-open-{}", p.id),
                            p.title.clone(),
                            name_of(p.account_id.as_ref()),
                            div()
                                .text_xs()
                                .text_color(color)
                                .child(text)
                                .into_any_element(),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ProjectPageEvent::Reveal(reveal.clone()))
                        })),
                    )
                    .child(trash(format!("pj-del-{}", p.id)).on_click(
                        cx.listener(move |this, _, _, cx| this.ask_delete(&del, true, cx)),
                    ))
            });
        let history_rows =
            history.iter().map(|h| {
                let (ws_id, reopen, del) = (ws.id.clone(), h.pane.id.clone(), h.pane.clone());
                div()
                    .flex()
                    .items_center()
                    .child(
                        row(
                            format!("pj-hist-{}", h.pane.id),
                            h.pane.title.clone(),
                            name_of(h.pane.account_id.as_ref()),
                            muted(ago(h.closed_at, now)).into_any_element(),
                        )
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(
                                "Reopen and carry on this conversation",
                            )
                            .build(window, cx)
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ProjectPageEvent::Reopen(ws_id.clone(), reopen.clone()))
                        })),
                    )
                    .child(trash(format!("pj-hdel-{}", h.pane.id)).on_click(
                        cx.listener(move |this, _, _, cx| this.ask_delete(&del, false, cx)),
                    ))
            });
        let summary = [
            folder,
            Some(format!("{} open", ws.panes.len())),
            (!ws.history.is_empty()).then(|| format!("{} in history", ws.history.len())),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let section = |title: &'static str| {
            div()
                .pt_2()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(title)
        };

        div()
            .id("project")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .bg(theme.background)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    if this.confirm.take().is_some() {
                        cx.notify();
                    } else {
                        cx.emit(ProjectPageEvent::Close);
                    }
                }
            }))
            .child(
                div()
                    .id("project-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .p_6()
                    .child(
                        div()
                            .max_w(px(820.))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
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
                                                    .child(ws.name.clone()),
                                            )
                                            .child(muted(summary)),
                                    )
                                    .child(
                                        div()
                                            .id("pj-new-chat")
                                            .px_3()
                                            .py_1()
                                            .rounded_md()
                                            .bg(theme.primary)
                                            .text_color(theme.primary_foreground)
                                            .cursor_pointer()
                                            .child("+ New chat")
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(ProjectPageEvent::NewChat)
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id("project-close")
                                            .px_3()
                                            .py_1()
                                            .rounded_md()
                                            .cursor_pointer()
                                            .text_color(theme.muted_foreground)
                                            .hover(|d| d.bg(theme.secondary))
                                            .child("← Back to panes")
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(ProjectPageEvent::Close)
                                            })),
                                    ),
                            )
                            .child(self.team.clone())
                            .when(total > 0, |d| d.child(Input::new(&self.query)))
                            .when(total == 0, |d| {
                                d.child(
                                    div()
                                        .py_6()
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_lg()
                                                .font_weight(FontWeight::MEDIUM)
                                                .child("No chats yet"),
                                        )
                                        .child(muted(
                                            "Start one in this project; it shows up here, and stays in History after you close it."
                                                .into(),
                                        )),
                                )
                            })
                            .when(!open.is_empty(), |d| {
                                d.child(section("Open")).children(open_rows)
                            })
                            .when(!history.is_empty(), |d| {
                                d.child(section("History")).children(history_rows)
                            })
                            .when(total > 0 && open.is_empty() && history.is_empty(), |d| {
                                d.child(muted(format!("No chats match “{q}”.")))
                            }),
                    ),
            )
            .when_some(self.confirm.as_ref(), |d, c| {
                let (ws_id, pane) = (ws.id.clone(), c.pane.clone());
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(gpui_kit::black().opacity(0.4))
                        .flex()
                        .justify_center()
                        .items_center()
                        .child(
                            div()
                                .w(px(420.))
                                .p_4()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.popover)
                                .shadow_lg()
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("Delete “{}”?", c.title)),
                                )
                                .child(muted(if c.open {
                                    format!(
                                        "{}The chat closes and does not go to History.",
                                        if c.busy {
                                            "It is still working and will be stopped. "
                                        } else {
                                            ""
                                        }
                                    )
                                } else {
                                    "It is removed from History and can no longer be reopened here."
                                        .into()
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("pj-keep")
                                                .px_3()
                                                .py_1()
                                                .rounded_md()
                                                .cursor_pointer()
                                                .hover(|d| d.bg(theme.secondary))
                                                .child("Keep it")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.confirm = None;
                                                    cx.notify();
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("pj-delete")
                                                .px_3()
                                                .py_1()
                                                .rounded_md()
                                                .cursor_pointer()
                                                .bg(theme.danger)
                                                .text_color(gpui_kit::white())
                                                .child("Delete chat")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.confirm = None;
                                                    cx.emit(ProjectPageEvent::Delete(
                                                        ws_id.clone(),
                                                        pane.clone(),
                                                    ));
                                                })),
                                        ),
                                ),
                        ),
                )
            })
    }
}
