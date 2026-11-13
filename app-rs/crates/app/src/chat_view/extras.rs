//! The message box's extras (`Composer.tsx`, `ReplyButton.tsx`, `QuestionCard.tsx`,
//! `ImageViewer.tsx`): Reply quotes from a selection in the chat (#164), earlier prompts on
//! ↑/↓ (#132), Claude's questions one at a time (#127), the pasted-image viewer (#163) and
//! "Send later" with this chat's queued messages (#77). The pure parts are the chat crate's
//! `compose` module.

use gpui_kit::base::TextSelection;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_chat::compose::{
    answer_of, at_edge, history_step, next_question, prompt_history, quote_of, split_recommended,
    HistoryNav, QuestionPick, Quote, QUOTES_MAX,
};
use wraithgrid_chat::{ApprovalDecision, ChatAnswers, ChatItem, ChatQuestion};
use wraithgrid_schedule::SendLater;

use super::{ChatView, ChatViewEvent};
use crate::a11y::A11y as _;

/// A queued "send later" message of this chat, as the window describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Queued {
    pub id: String,
    pub prompt: String,
    /// "at 14:00 · in 1h", "when home limit resets · in 2h".
    pub when: String,
}

#[derive(Default)]
pub(super) struct Extras {
    pub quotes: Vec<Quote>,
    /// The quote card shown in full.
    quote_open: Option<usize>,
    pub history: HistoryNav,
    /// Where the Reply button shows (where the selection in the chat was let go), and the
    /// text it quotes: taken then, as pressing the button ends the selection.
    reply_at: Option<(Point<Pixels>, String)>,
    /// The pasted image the viewer shows.
    viewer: Option<usize>,
    later_menu: bool,
    later_at: Option<Entity<InputState>>,
    queued: Vec<Queued>,
    /// ("When home's limit resets", "in 2h"), when the account's reset is known.
    reset: Option<(String, String)>,
    question: Option<Questions>,
    /// Attached files (paperclip or drop) other than images: sent as paths after the message.
    pub files: Vec<String>,
    /// Thinking blocks and handoff texts opened, by item id.
    opened: std::collections::HashSet<String>,
}

/// Claude's questions being answered (`QuestionCard`): which one, and what was picked.
struct Questions {
    req_id: String,
    questions: Vec<ChatQuestion>,
    at: usize,
    picks: Vec<QuestionPick>,
    other: Vec<Entity<InputState>>,
}

/// `hh:mm` of a time, local.
fn hm(ms: i64) -> String {
    let d = wraithgrid_schedule::local_of(&chrono::Local, ms);
    format!(
        "{:02}:{:02}",
        chrono::Timelike::hour(&d),
        chrono::Timelike::minute(&d)
    )
}

impl ChatView {
    /// This chat's queued messages and its account's reset, from the window.
    pub fn set_later(
        &mut self,
        queued: Vec<Queued>,
        reset: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        if self.x.queued != queued || self.x.reset != reset {
            self.x.queued = queued;
            self.x.reset = reset;
            cx.notify();
        }
    }

    /// Ctrl/⌘+Shift+R replies with the selection in the chat.
    pub(super) fn extras_key(
        &mut self,
        e: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let k = &e.keystroke;
        let m = k.modifiers;
        if (m.control || m.platform) && m.shift && k.key.eq_ignore_ascii_case("r") {
            self.reply_from_selection(window, cx);
            return true;
        }
        false
    }

    /// ↑ from the first line or ↓ from the last walks the prompts sent before (#132); false
    /// leaves the key to the box.
    pub(super) fn history_key(
        &mut self,
        up: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = self.composer.read(cx);
        let text = state.value().to_string();
        let caret = state.cursor().min(text.len());
        if !at_edge(&text, caret, caret, up) {
            return false;
        }
        let list = prompt_history(self.thread.items.iter().filter_map(|i| match i {
            ChatItem::User { text, .. } => Some(text.as_str()),
            _ => None,
        }));
        let Some((nav, next)) = history_step(&list, &self.x.history, up, &text) else {
            return false;
        };
        self.x.history = nav;
        self.composer
            .update(cx, |c, cx| c.set_value(next, window, cx));
        cx.notify();
        true
    }

    /// The selection in the chat becomes a quote above the box (`ReplyButton`).
    fn reply_from_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = match self.x.reply_at.take() {
            Some((_, text)) => text,
            None => TextSelection::selected_text(window, cx),
        };
        let Some(q) = quote_of(&selected) else {
            return cx.notify();
        };
        if self.x.quotes.len() < QUOTES_MAX {
            self.x.quotes.push(q);
        }
        TextSelection::clear(window, cx);
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }

    /// A selection made in the chat shows Reply where the mouse let go.
    pub(super) fn thread_mouse_up(
        &mut self,
        e: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = if TextSelection::has_selection(window, cx) {
            TextSelection::selected_text(window, cx)
        } else {
            String::new()
        };
        self.x.reply_at = (!text.trim().is_empty()).then_some((e.position, text));
        cx.notify();
    }

    pub(super) fn reply_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let at = self.x.reply_at.as_ref()?.0;
        let theme = cx.theme().clone();
        Some(
            deferred(
                anchored()
                    .position(at - point(px(0.), px(34.)))
                    .snap_to_window_with_margin(px(4.))
                    .child(
                        div()
                            .id(SharedString::from(format!("reply-{}", self.pane_id)))
                            .a11y_button("Reply")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.popover)
                            .shadow_md()
                            .text_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(theme.secondary))
                            .child("↩ Reply")
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reply_from_selection(window, cx)
                            })),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// The quotes waiting above the box: 3 lines, a click shows all, × removes.
    pub(super) fn quote_cards(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.x.quotes.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let cards = self.x.quotes.iter().enumerate().map(|(i, q)| {
            let open = self.x.quote_open == Some(i);
            let shown = if open {
                q.text.clone()
            } else {
                q.text.lines().take(3).collect::<Vec<_>>().join("\n")
            };
            div()
                .id(SharedString::from(format!("quote-{}-{i}", self.pane_id)))
                .role(Role::Group)
                .aria_label(format!("Quote {}", i + 1))
                .flex()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme.secondary)
                .border_l_2()
                .border_color(theme.ring)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.x.quote_open = if this.x.quote_open == Some(i) {
                        None
                    } else {
                        Some(i)
                    };
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(shown)
                        .when(q.trimmed, |d| d.child(div().italic().child("trimmed"))),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("unquote-{}-{i}", self.pane_id)))
                        .a11y_button(format!("Remove quote {}", i + 1))
                        .text_xs()
                        .cursor_pointer()
                        .child("×")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if i < this.x.quotes.len() {
                                this.x.quotes.remove(i);
                            }
                            this.x.quote_open = None;
                            cx.notify();
                        })),
                )
        });
        Some(
            div()
                .px_2()
                .pt_2()
                .flex()
                .flex_col()
                .gap_1()
                .children(cards)
                .into_any_element(),
        )
    }

    pub(super) fn open_viewer(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.x.viewer = Some(i);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// ← → step through the pasted images, Esc closes the viewer.
    pub(super) fn viewer_key(
        &mut self,
        e: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(at) = self.x.viewer else { return };
        let n = self.images.len().max(1);
        match e.keystroke.key.as_str() {
            "left" => self.x.viewer = Some((at + n - 1) % n),
            "right" => self.x.viewer = Some((at + 1) % n),
            "escape" => {
                self.x.viewer = None;
                self.composer.update(cx, |c, cx| c.focus(window, cx));
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The full-window viewer over the pasted images (`ImageViewer`).
    pub(super) fn image_viewer(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let at = self.x.viewer.filter(|i| *i < self.images.len())?;
        let image = self.images[at].1.clone();
        let size = window.viewport_size();
        let n = self.images.len();
        let theme = cx.theme().clone();
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id(SharedString::from(format!("viewer-{}", self.pane_id)))
                        .a11y_dialog("Image viewer")
                        .occlude()
                        .w(size.width)
                        .h(size.height)
                        .bg(hsla(0., 0., 0., 0.85))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.x.viewer = None;
                            this.composer.update(cx, |c, cx| c.focus(window, cx));
                            cx.notify();
                        }))
                        .child(
                            img(image)
                                .max_w(size.width * 0.9)
                                .max_h(size.height * 0.85)
                                .object_fit(ObjectFit::Contain),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!("Image {} of {n} · ← → · Esc closes", at + 1)),
                        ),
                ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    /// The pending question set, when Claude asked one (an approval with questions).
    fn asked(&self) -> Option<(String, Vec<ChatQuestion>)> {
        self.thread.items.iter().rev().find_map(|i| match i {
            ChatItem::Tool(t) => t.approval.as_ref().and_then(|a| {
                (a.decided.is_none())
                    .then(|| a.questions.clone().map(|q| (a.req_id.clone(), q)))
                    .flatten()
            }),
            _ => None,
        })
    }

    fn answered(q: &Questions, cx: &App) -> Vec<bool> {
        q.questions
            .iter()
            .zip(&q.picks)
            .zip(&q.other)
            .map(|((x, p), o)| {
                let p = QuestionPick {
                    other: o.read(cx).value().to_string(),
                    ..p.clone()
                };
                !answer_of(x, &p).is_empty()
            })
            .collect()
    }

    fn answer_questions(&mut self, cx: &mut Context<Self>) {
        let Some(q) = self.x.question.take() else {
            return;
        };
        let answers: ChatAnswers = q
            .questions
            .iter()
            .zip(&q.picks)
            .zip(&q.other)
            .map(|((x, p), o)| {
                let p = QuestionPick {
                    other: o.read(cx).value().to_string(),
                    ..p.clone()
                };
                (x.question.clone(), answer_of(x, &p))
            })
            .collect();
        if let Some(session) = &self.session {
            session.approve(&q.req_id, ApprovalDecision::Allow, Some(answers));
        }
        cx.notify();
    }

    fn skip_questions(&mut self, cx: &mut Context<Self>) {
        if let Some(q) = self.x.question.take() {
            if let Some(session) = &self.session {
                session.approve(&q.req_id, ApprovalDecision::Deny, None);
            }
        }
        cx.notify();
    }

    fn pick(&mut self, label: String, cx: &mut Context<Self>) {
        let Some(q) = &mut self.x.question else {
            return;
        };
        let at = q.at;
        let multi = q.questions[at].multi_select;
        q.picks[at].toggle(&label, multi);
        let last = at + 1 == q.questions.len();
        if !multi && !last {
            let mut answered = Self::answered(q, cx);
            answered[at] = true;
            // A one-choice pick moves on after a moment, so the tick is seen first.
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(350))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(q) = &mut this.x.question {
                        if q.at == at {
                            q.at = next_question(at, &answered);
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// Claude's questions, one at a time (`QuestionCard`): numbered options (keys 1…n), an
    /// answer of your own, Back / Next, then Answer (all at once) or Skip.
    pub(super) fn question_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some((req_id, questions)) = self.asked() else {
            self.x.question = None;
            return None;
        };
        if self.x.question.as_ref().is_none_or(|q| q.req_id != req_id) {
            let other = questions
                .iter()
                .map(|_| {
                    let input = cx.new(|cx| {
                        InputState::new(window, cx).placeholder("Other: type your own answer")
                    });
                    cx.subscribe(&input, |this, input, e: &InputEvent, cx| {
                        let Some(q) = &mut this.x.question else {
                            return;
                        };
                        let Some(i) = q.other.iter().position(|o| *o == input) else {
                            return;
                        };
                        match e {
                            InputEvent::Focus => {
                                q.picks[i].other_on = true;
                                if !q.questions[i].multi_select {
                                    q.picks[i].labels.clear();
                                }
                            }
                            InputEvent::Change => q.picks[i].other_on = true,
                            _ => {}
                        }
                        cx.notify();
                    })
                    .detach();
                    input
                })
                .collect();
            self.x.question = Some(Questions {
                req_id,
                picks: vec![QuestionPick::default(); questions.len()],
                questions,
                at: 0,
                other,
            });
        }
        let theme = cx.theme().clone();
        let q = self.x.question.as_ref()?;
        let answered = Self::answered(q, cx);
        let ready = answered.iter().all(|a| *a);
        let at = q.at;
        let cur = &q.questions[at];
        let pick = &q.picks[at];
        let last = at + 1 == q.questions.len();
        let n = q.questions.len();
        let button = |id: String, label: &str, primary: bool, enabled: bool| {
            div()
                .id(SharedString::from(id))
                .a11y_button(label.to_string())
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .when(primary, |d| {
                    d.bg(theme.primary).text_color(theme.primary_foreground)
                })
                .when(!primary, |d| d.border_1().border_color(theme.border))
                .when(enabled, |d| d.cursor_pointer())
                .when(!enabled, |d| d.opacity(0.5))
                .child(label.to_string())
        };
        let tabs = q.questions.iter().enumerate().map(|(i, x)| {
            let label = if x.header.is_empty() {
                format!("Question {}", i + 1)
            } else {
                x.header.clone()
            };
            div()
                .id(SharedString::from(format!("cq-tab-{i}")))
                .role(Role::Tab)
                .aria_label(label.clone())
                .aria_selected(i == at)
                .px_2()
                .py(px(1.))
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .when(i == at, |d| d.bg(theme.secondary))
                .child(if answered[i] && i != at {
                    format!("✓ {label}")
                } else {
                    label
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(q) = &mut this.x.question {
                        q.at = i;
                    }
                    cx.notify();
                }))
        });
        let options = cur.options.iter().enumerate().map(|(k, o)| {
            let on = pick.labels.contains(&o.label);
            let (text, recommended) = split_recommended(&o.label);
            let label = o.label.clone();
            div()
                .id(SharedString::from(format!("cq-opt-{at}-{k}")))
                .a11y_toggle(label.clone(), on)
                .flex()
                .items_start()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .border_1()
                .border_color(if on { theme.ring } else { theme.border })
                .when(on, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
                .child(
                    div()
                        .text_xs()
                        .px_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(theme.border)
                        .child((k + 1).to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(div().flex().gap_2().text_sm().child(text.to_string()).when(
                            recommended,
                            |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.primary)
                                        .child("recommended"),
                                )
                            },
                        ))
                        .when(!o.description.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(o.description.clone()),
                            )
                        }),
                )
                .child(div().w(px(14.)).child(if on { "✓" } else { "" }))
                .on_click(cx.listener(move |this, _, _, cx| this.pick(label.clone(), cx)))
        });
        let other = q.other[at].clone();
        Some(
            div()
                .id(SharedString::from(format!("cq-{}", self.pane_id)))
                .mx_2()
                .mt_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.warning)
                .bg(theme.popover)
                .flex()
                .flex_col()
                .gap_2()
                .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                    let Some(q) = &this.x.question else { return };
                    let key = e.keystroke.key.as_str();
                    let n_opts = q.questions[q.at].options.len();
                    let typing = q.other[q.at].focus_handle(cx).is_focused(window);
                    match key {
                        "escape" => this.skip_questions(cx),
                        "enter" => {
                            let answered = Self::answered(q, cx);
                            if q.at + 1 == q.questions.len() {
                                if answered.iter().all(|a| *a) {
                                    this.answer_questions(cx);
                                }
                            } else if answered[q.at] {
                                if let Some(q) = &mut this.x.question {
                                    q.at += 1;
                                }
                            }
                        }
                        _ if typing => return,
                        "left" => {
                            if let Some(q) = &mut this.x.question {
                                q.at = q.at.saturating_sub(1);
                            }
                        }
                        k if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() => {
                            let d = (k.as_bytes()[0] - b'0') as usize;
                            if (1..=n_opts).contains(&d) {
                                let label = q.questions[q.at].options[d - 1].label.clone();
                                this.pick(label, cx);
                            } else if d == n_opts + 1 {
                                let input = q.other[q.at].clone();
                                input.update(cx, |i, cx| i.focus(window, cx));
                            } else {
                                return;
                            }
                        }
                        _ => return,
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child("Claude asks"),
                        )
                        .when(n > 1, |d| d.child(div().flex().gap_1().children(tabs))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{}{}",
                            if n > 1 {
                                format!("{} of {n}", at + 1)
                            } else {
                                cur.header.clone()
                            },
                            if cur.multi_select { " · pick any" } else { "" }
                        )),
                )
                .child(div().text_sm().child(cur.question.clone()))
                .child(div().flex().flex_col().gap_1().children(options))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .px_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(theme.border)
                                .child((cur.options.len() + 1).to_string()),
                        )
                        .child(div().flex_1().child(Input::new(&other))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            button("cq-skip".into(), "Skip", false, true)
                                .on_click(cx.listener(|this, _, _, cx| this.skip_questions(cx))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "1–{} pick · Enter {}{}",
                                    cur.options.len() + 1,
                                    if last { "answer" } else { "next" },
                                    if at > 0 { " · ← back" } else { "" }
                                )),
                        )
                        .when(at > 0, |d| {
                            d.child(button("cq-back".into(), "Back", false, true).on_click(
                                cx.listener(|this, _, _, cx| {
                                    if let Some(q) = &mut this.x.question {
                                        q.at = q.at.saturating_sub(1);
                                    }
                                    cx.notify();
                                }),
                            ))
                        })
                        .child(if last {
                            button("cq-answer".into(), "Answer", true, ready).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if ready {
                                        this.answer_questions(cx)
                                    }
                                },
                            ))
                        } else {
                            let can = answered[at];
                            button("cq-next".into(), "Next", true, can).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let (true, Some(q)) = (can, &mut this.x.question) {
                                        q.at += 1;
                                    }
                                    cx.notify();
                                },
                            ))
                        }),
                )
                .into_any_element(),
        )
    }

    /// The text to queue: the box, with its quotes first; the box and quotes clear.
    fn take_for_later(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        let typed = self.composer.read(cx).value().trim().to_string();
        let text = wraithgrid_chat::compose::with_quotes(&self.x.quotes, &typed)
            .trim()
            .to_string();
        if text.is_empty() {
            return None;
        }
        self.x.quotes.clear();
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        Some(text)
    }

    fn queue(&mut self, when: SendLater, window: &mut Window, cx: &mut Context<Self>) {
        self.x.later_menu = false;
        self.x.later_at = None;
        if let Some(text) = self.take_for_later(window, cx) {
            cx.emit(ChatViewEvent::Queue { text, when });
        }
        cx.notify();
    }

    /// The clock beside Send: "Send later".
    pub(super) fn later_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let on = self.x.later_menu || self.x.later_at.is_some();
        div()
            .id(SharedString::from(format!("later-{}", self.pane_id)))
            .a11y_button("Send later")
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .when(on, |d| d.bg(theme.secondary))
            .cursor_pointer()
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new("Send later").build(window, cx)
            })
            .child("◷")
            .on_click(cx.listener(|this, _, _, cx| {
                this.x.later_menu = !this.x.later_menu;
                cx.notify();
            }))
            .into_any_element()
    }

    /// The menu of times, above the box, and the "At a time…" row.
    pub(super) fn later_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        if let Some(input) = &self.x.later_at {
            let input = input.clone();
            let ok = !self.composer.read(cx).value().trim().is_empty();
            return Some(
                div()
                    .px_2()
                    .pt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child("Send at")
                    .child(div().w(px(200.)).child(Input::new(&input)))
                    .child(
                        div()
                            .id("later-cancel")
                            .a11y_button("Cancel")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|d| d.bg(theme.secondary))
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.x.later_at = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("later-queue")
                            .a11y_button("Queue")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .when(ok, |d| d.cursor_pointer())
                            .when(!ok, |d| d.opacity(0.5))
                            .child("Queue")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let at = input.read(cx).value().trim().to_string();
                                if let Some(t) =
                                    wraithgrid_schedule::parse_local(&chrono::Local, &at)
                                {
                                    this.queue(SendLater::At(t), window, cx);
                                }
                            })),
                    )
                    .into_any_element(),
            );
        }
        if !self.x.later_menu {
            return None;
        }
        let now = wraithgrid_usage::now_ms();
        let tomorrow = {
            use chrono::{Duration, Local, NaiveTime};
            let day = Local::now().date_naive() + Duration::days(1);
            wraithgrid_schedule::local_ms(&Local, day.and_time(NaiveTime::from_hms_opt(9, 0, 0)?))
        };
        let row = |id: &'static str, label: String, hint: String| {
            div()
                .id(id)
                .role(Role::MenuItem)
                .aria_label(format!("{label} {hint}"))
                .flex()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(div().flex_1().child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(hint),
                )
        };
        Some(
            div()
                .mx_2()
                .mt_2()
                .w(px(280.))
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .text_sm()
                .flex()
                .flex_col()
                .when_some(self.x.reset.clone(), |d, (label, hint)| {
                    d.child(row("later-reset", label, hint).on_click(
                        cx.listener(|this, _, window, cx| this.queue(SendLater::Reset, window, cx)),
                    ))
                })
                .child(
                    row("later-hour", "In 1 hour".into(), hm(now + 3_600_000)).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.queue(SendLater::At(now + 3_600_000), window, cx)
                        }),
                    ),
                )
                .child(
                    row("later-morning", "Tomorrow morning".into(), "09:00".into()).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.queue(SendLater::At(tomorrow), window, cx)
                        }),
                    ),
                )
                .child(
                    row("later-at", "At a time…".into(), String::new()).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.x.later_menu = false;
                            let value =
                                wraithgrid_schedule::to_local(&chrono::Local, now + 3_600_000);
                            this.x.later_at = Some(cx.new(|cx| {
                                InputState::new(window, cx)
                                    .placeholder("2026-11-13T09:00")
                                    .default_value(value)
                            }));
                            cx.notify();
                        },
                    )),
                )
                .into_any_element(),
        )
    }

    /// This chat's queued messages above the box: Cancel or Send now.
    pub(super) fn queued_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        self.x
            .queued
            .iter()
            .map(|j| {
                let (cancel, send) = (j.id.clone(), j.id.clone());
                let small = |id: String, label: &'static str| {
                    div()
                        .id(SharedString::from(id))
                        .a11y_button(label)
                        .px_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .text_xs()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.secondary))
                        .child(label)
                };
                div()
                    .mx_2()
                    .mt_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme.secondary)
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child("◷")
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .child(j.prompt.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(j.when.clone()),
                    )
                    .child(
                        small(format!("qc-{}", j.id), "Cancel").on_click(cx.listener(
                            move |_, _, _, cx| cx.emit(ChatViewEvent::CancelQueued(cancel.clone())),
                        )),
                    )
                    .child(
                        small(format!("qs-{}", j.id), "Send now").on_click(cx.listener(
                            move |_, _, _, cx| cx.emit(ChatViewEvent::SendQueuedNow(send.clone())),
                        )),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

impl ChatView {
    /// The paperclip: the system file chooser, then [`Self::attach_paths`].
    pub(super) fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.attach_paths(&paths, cx));
        })
        .detach();
    }

    /// Picked or dropped files: images (small enough) go as images, like a paste; the rest as
    /// paths listed after the message (`withAttachments`).
    pub(super) fn attach_paths(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        use base64::Engine as _;
        let picked = wraithgrid_chat::attach::read_picked(paths);
        for img in picked.images {
            if self.images.len() >= wraithgrid_chat::CHAT_IMAGES_MAX {
                break;
            }
            let format = match img.media_type {
                wraithgrid_chat::ChatImageType::Png => ImageFormat::Png,
                wraithgrid_chat::ChatImageType::Jpeg => ImageFormat::Jpeg,
                wraithgrid_chat::ChatImageType::Gif => ImageFormat::Gif,
                wraithgrid_chat::ChatImageType::Webp => ImageFormat::Webp,
            };
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&img.data) else {
                continue;
            };
            self.images
                .push((img, std::sync::Arc::new(Image::from_bytes(format, bytes))));
        }
        for f in picked.files {
            if !self.x.files.contains(&f) && self.x.files.len() < wraithgrid_chat::CHAT_FILES_MAX {
                self.x.files.push(f);
            }
        }
        cx.notify();
    }

    pub(super) fn attach_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        div()
            .id(SharedString::from(format!("attach-{}", self.pane_id)))
            .a11y_button("Attach files")
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .text_color(theme.muted_foreground)
            .cursor_pointer()
            .hover(|d| d.bg(theme.secondary))
            .child("📎")
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new("Attach files (or drop them here)")
                    .build(window, cx)
            })
            .on_click(cx.listener(|this, _, window, cx| this.pick_files(window, cx)))
            .into_any_element()
    }

    /// The attached files as chips, each with ×.
    pub(super) fn file_chips(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.x.files.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let chips = self.x.files.iter().enumerate().map(|(i, f)| {
            let name = wraithgrid_core::paths::base_name(f);
            let path = f.clone();
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py(px(2.))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .text_xs()
                .child(div().text_color(theme.muted_foreground).child("▤"))
                .child(name.clone())
                .child(
                    div()
                        .id(SharedString::from(format!("unfile-{i}")))
                        .a11y_button(format!("Remove {name}"))
                        .px_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .text_color(theme.muted_foreground)
                        .hover(|d| d.bg(theme.secondary))
                        .child("×")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.x.files.retain(|x| *x != path);
                            cx.notify();
                        })),
                )
        });
        Some(
            div()
                .id(SharedString::from(format!("files-{}", self.pane_id)))
                .role(Role::Group)
                .aria_label("Attached files")
                .px_2()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(chips)
                .into_any_element(),
        )
    }
}

impl ChatView {
    /// The thread is scrolled to (near) its end.
    pub(super) fn at_bottom(&self) -> bool {
        let max = self.scroll.max_offset().y;
        max <= px(0.) || -self.scroll.offset().y >= max - px(24.)
    }

    /// "Scroll to the latest message", over the thread's bottom edge when it isn't there.
    pub(super) fn jump_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.at_bottom() {
            return None;
        }
        let theme = cx.theme();
        Some(
            div()
                .absolute()
                .right(px(16.))
                .bottom(px(8.))
                .child(
                    div()
                        .id(SharedString::from(format!("jump-{}", self.pane_id)))
                        .a11y_button("Scroll to the latest message")
                        .size(px(28.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .shadow_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.secondary))
                        .child("↓")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.scroll.scroll_to_bottom();
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }
}

impl ChatView {
    /// "Thought process": closed by default, a click shows it all (`cthink`).
    pub(super) fn thinking(&self, id: &str, text: &str, cx: &mut Context<Self>) -> AnyElement {
        if text.trim().is_empty() {
            return div().into_any_element();
        }
        let theme = cx.theme();
        let open = self.x.opened.contains(id);
        let key = id.to_string();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id(SharedString::from(format!("think-{id}")))
                    .role(Role::Button)
                    .aria_label("Thought process")
                    .aria_expanded(open)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .child(format!("{} Thought process", if open { "▾" } else { "▸" }))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_opened(&key, cx))),
            )
            .when(open, |d| {
                d.child(
                    div()
                        .pl_3()
                        .border_l_2()
                        .border_color(theme.border)
                        .text_sm()
                        .italic()
                        .text_color(theme.muted_foreground)
                        .child(text.to_string()),
                )
            })
            .into_any_element()
    }
}

impl ChatView {
    fn toggle_opened(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.x.opened.remove(id) {
            self.x.opened.insert(id.to_string());
        }
        cx.notify();
    }

    /// A handoff from another CLI (#78): the divider, then "Handoff from X · N messages" that
    /// opens on what was carried over.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handoff(
        &self,
        id: &str,
        text: &str,
        messages: u64,
        from_cli: &str,
        to_cli: &str,
        divider: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let open = self.x.opened.contains(id);
        let from = wraithgrid_core::agent_cli::AgentCli::parse(from_cli)
            .map_or(from_cli, |c| c.spec().provider_label);
        let key = id.to_string();
        let line = div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().h(px(1.)).bg(theme.border))
            .when(!to_cli.is_empty(), |d| {
                d.child(crate::shell::provider_mark(to_cli, to_cli, 12., cx))
            })
            .child(format!(
                "{} · {messages} message{} carried over",
                if divider.is_empty() {
                    "continued"
                } else {
                    divider
                },
                if messages == 1 { "" } else { "s" }
            ))
            .child(div().flex_1().h(px(1.)).bg(theme.border));
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(line)
            .when(
                !text.trim().is_empty() && !from_cli.is_empty() && from_cli != to_cli,
                |d| {
                    d.child(
                        div()
                            .id(SharedString::from(format!("handoff-{id}")))
                            .role(Role::Button)
                            .aria_label(format!("Handoff from {from}"))
                            .aria_expanded(open)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .child(format!(
                                "{} Handoff from {from} · {messages} messages · {}",
                                if open { "▾" } else { "▸" },
                                if open { "hide" } else { "show" }
                            ))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_opened(&key, cx)),
                            ),
                    )
                    .when(open, |d| {
                        d.child(
                            div()
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(theme.border)
                                .text_xs()
                                .font_family(crate::terminal::mono(cx))
                                .child(text.to_string()),
                        )
                    })
                },
            )
            .into_any_element()
    }
}
