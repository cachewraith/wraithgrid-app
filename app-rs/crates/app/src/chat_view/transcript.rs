//! The conversation itself (`Item`, `ToolCard`, `ApprovalBar` and the first screen of
//! ChatView.tsx): messages with their chips and images, answers with Copy, tool calls as a
//! line that opens, notices and turn footers, the working line, and the approval pinned over
//! the box. How each row is worded is the chat crate's `present` module.

use std::sync::Arc;

use gpui_kit::base::SelectableText;
use gpui_kit::component::text::TextView;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_chat::present::{
    plain_first_line, relative_to, tool_glyph, tool_state, tool_verb, turn_footer, ToolState,
};
use wraithgrid_chat::slash::{leading_command, split_mentions};
use wraithgrid_chat::{
    now_ms, ApprovalDecision, ChatItem, ItemLevel, ToolItem, ToolKind, ToolPreview,
};

use super::ChatView;
use crate::a11y::A11y as _;

/// Starters on a new chat's first screen: what is sent, and a line on what it does.
const SUGGESTIONS: [(&str, &str); 4] = [
    (
        "Explain how this project is put together",
        "A map of its folders and how they fit",
    ),
    ("Review my uncommitted changes", "Before you commit them"),
    (
        "Find and fix a bug",
        "Say what goes wrong; it reproduces it first",
    ),
    (
        "Write tests for the latest change",
        "Reads the last commit and covers it",
    ),
];

/// How long streamed text must stop growing before the working line shows under it again
/// (#129): the turn goes on unseen (e.g. Claude writing a big tool call).
const TEXT_QUIET_MS: i64 = 1200;

/// A saved thumbnail (`data:image/png;base64,…`) as an image to draw.
fn decode_thumb(url: &str) -> Option<Arc<Image>> {
    use base64::Engine as _;
    let (head, data) = url.split_once(";base64,")?;
    let format = match head.strip_prefix("data:image/")? {
        "png" => ImageFormat::Png,
        "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        _ => return None,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

/// A small "Copy" in a code block's corner.
fn code_copy(code: SharedString, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .id("copy")
        .a11y_button("Copy code")
        .px_2()
        .py(px(1.))
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .text_color(theme.muted_foreground)
        .hover(|d| d.bg(theme.secondary))
        .child("Copy")
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()))
        })
        .into_any_element()
}

impl ChatView {
    /// Claude asked questions and waits on them (the box dims; the card is above it).
    pub(super) fn asking(&self) -> bool {
        self.thread.busy
            && self
                .thread
                .pending_approval()
                .is_some_and(|t| t.approval.as_ref().is_some_and(|a| a.questions.is_some()))
    }

    /// The tool call waiting on Allow / Deny: only while its turn runs.
    fn pending_tool(&self) -> Option<ToolItem> {
        self.thread
            .busy
            .then(|| self.thread.pending_approval())
            .flatten()
            .filter(|t| t.approval.as_ref().is_some_and(|a| a.questions.is_none()))
            .cloned()
    }

    /// Each item of the conversation as a row; items that draw nothing have no row (and no
    /// gap).
    pub(super) fn transcript_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let items = self.thread.items.clone();
        let busy = self.thread.busy;
        let streaming = items.last().filter(|_| busy).and_then(|i| match i {
            ChatItem::Assistant { id, .. } => Some(id.clone()),
            _ => None,
        });
        let mut rows = Vec::with_capacity(items.len());
        for item in &items {
            let row = match item {
                ChatItem::User {
                    id,
                    text,
                    images,
                    thumbs,
                } => Some(self.user_row(id, text, *images, thumbs.as_deref(), cx)),
                ChatItem::Assistant { id, text } => {
                    Some(self.answer_row(id, text, streaming.as_ref() == Some(id), cx))
                }
                ChatItem::Thinking { id, text } => {
                    (!text.trim().is_empty()).then(|| self.thinking(id, text, cx))
                }
                ChatItem::Tool(t) => Some(self.tool_card(t, busy, cx)),
                ChatItem::Notice { level, text, .. } => Some(self.notice(*level, text, cx)),
                ChatItem::Turn {
                    cost_usd,
                    duration_ms,
                    error,
                    ..
                } => match error {
                    Some(e) => Some(self.notice(ItemLevel::Error, e, cx)),
                    None => turn_footer(*cost_usd, *duration_ms).map(|line| {
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(line)
                            .into_any_element()
                    }),
                },
                ChatItem::Handoff {
                    id,
                    text,
                    messages,
                    from_cli,
                    to_cli,
                    divider,
                } => Some(self.handoff(id, text, *messages, from_cli, to_cli, divider, cx)),
            };
            rows.extend(row);
        }
        rows
    }

    /// A sent message: its quotes, images, then the text with `/command` and `@path` as
    /// chips (`UserText`); the text can be selected.
    fn user_row(
        &mut self,
        id: &str,
        text: &str,
        images: u64,
        thumbs: Option<&[String]>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (quotes, body) = wraithgrid_chat::compose::split_quotes(text);
        let shown = self.x.sent.get(id).cloned().or_else(|| {
            let thumbs = thumbs?;
            Some(
                self.x
                    .thumbs
                    .entry(id.to_string())
                    .or_insert_with(|| thumbs.iter().filter_map(|t| decode_thumb(t)).collect())
                    .clone(),
            )
        });
        let shown = shown.filter(|s| !s.is_empty());
        let tiles = shown.as_ref().map(|list| {
            let n = list.len();
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(list.iter().enumerate().map(|(i, image)| {
                    let all = list.clone();
                    div()
                        .id(SharedString::from(format!("sent-{id}-{i}")))
                        .a11y_button(format!("Open image {} of {n}", i + 1))
                        .size(px(if n == 1 { 160. } else { 72. }))
                        .rounded_md()
                        .overflow_hidden()
                        .border_1()
                        .border_color(theme.border)
                        .cursor_pointer()
                        .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_viewer(all.clone(), i, window, cx)
                        }))
                }))
        });
        // Saved before thumbnails, or still being made: the count.
        let count = (shown.is_none() && images > 0).then(|| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "{images} image{}",
                    if images == 1 { "" } else { "s" }
                ))
        });
        let cmd = leading_command(&body);
        let rest = cmd.as_ref().map_or(body.as_str(), |c| c.rest.as_str());
        let chip = |label: String, mention: bool| {
            div()
                .flex_none()
                .px_1()
                .rounded_sm()
                .bg(if mention {
                    theme.info.opacity(0.15)
                } else {
                    theme.primary.opacity(0.15)
                })
                .font_family(crate::terminal::mono(cx))
                .text_sm()
                .child(label)
        };
        let mut parts: Vec<AnyElement> = Vec::new();
        if let Some(c) = &cmd {
            parts.push(chip(format!("/{}", c.name), false).into_any_element());
        }
        for (k, part) in split_mentions(rest).into_iter().enumerate() {
            parts.push(if part.mention {
                chip(part.text, true).into_any_element()
            } else if !part.text.trim().is_empty() {
                SelectableText::new(
                    SharedString::from(format!("utext-{id}-{k}")),
                    part.text.trim().to_string(),
                )
                .into_any_element()
            } else {
                continue;
            });
        }
        div()
            .flex()
            .justify_end()
            .child(
                div()
                    .max_w(relative(0.8))
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .bg(theme.secondary)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(quotes.into_iter().map(|q| {
                        div()
                            .pl_2()
                            .border_l_2()
                            .border_color(theme.border)
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(q)
                    }))
                    .children(tiles)
                    .children(count)
                    .when(!parts.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_baseline()
                                .gap_x_1()
                                .children(parts),
                        )
                    }),
            )
            .into_any_element()
    }

    /// An answer: markdown with Copy on each code block, and "Copy answer" once it is done.
    fn answer_row(
        &mut self,
        id: &str,
        text: &str,
        streaming: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.markdown(id, text, cx);
        let theme = cx.theme().clone();
        let copied = self.x.copied.as_deref() == Some(id);
        let (key, whole) = (id.to_string(), text.to_string());
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                TextView::new(&state)
                    .selectable(true)
                    .code_block_actions(|block, _, cx| code_copy(block.code(), cx)),
            )
            .when(!streaming && !text.trim().is_empty(), |d| {
                d.child(
                    div().flex().child(
                        div()
                            .id(SharedString::from(format!("copy-{id}")))
                            .a11y_button("Copy answer")
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.secondary))
                            .child(if copied { "✓ Copied" } else { "Copy" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(whole.clone()));
                                this.flash_copied(key.clone(), cx);
                            })),
                    ),
                )
            })
            .into_any_element()
    }

    /// The Copy button shows a tick for a moment.
    fn flash_copied(&mut self, id: String, cx: &mut Context<Self>) {
        self.x.copied = Some(id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1200))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.x.copied.as_deref() == Some(id.as_str()) {
                    this.x.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A notice: a line across the chat, or a boxed note (`cnote`).
    fn notice(&self, level: ItemLevel, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match level {
            ItemLevel::Divider => div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().flex_1().h(px(1.)).bg(theme.border))
                .child(text.to_string())
                .child(div().flex_1().h(px(1.)).bg(theme.border))
                .into_any_element(),
            ItemLevel::Error => div()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.danger.opacity(0.4))
                .bg(theme.danger.opacity(0.08))
                .text_sm()
                .text_color(theme.danger)
                .child(text.to_string())
                .into_any_element(),
            ItemLevel::Info => div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(theme.secondary)
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text.to_string())
                .into_any_element(),
        }
    }

    /// A tool's input, as the card (and the approval bar) shows it, in a box that scrolls
    /// past a height.
    fn preview(&self, key: &str, p: &ToolPreview, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mono = crate::terminal::mono(cx);
        let boxed = |id: String| {
            div()
                .id(SharedString::from(id))
                .max_h(px(320.))
                .overflow_y_scroll()
                .rounded_md()
                .font_family(mono.clone())
                .text_xs()
        };
        match p {
            ToolPreview::Code { text } => boxed(format!("code-{key}"))
                .p_2()
                .bg(theme.secondary)
                .child(text.clone())
                .into_any_element(),
            ToolPreview::Diff {
                path,
                before,
                after,
            } => {
                // Lines wrap rather than run out of the card.
                let lines = before
                    .lines()
                    .map(|l| (false, l))
                    .chain(after.lines().map(|l| (true, l)))
                    .map(|(add, l)| {
                        div()
                            .px_2()
                            .flex()
                            .gap_1()
                            .bg(if add {
                                theme.success.opacity(0.12)
                            } else {
                                theme.danger.opacity(0.12)
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.muted_foreground)
                                    .child(if add { "+" } else { "-" }),
                            )
                            .child(div().flex_1().min_w_0().child(l.to_string()))
                    });
                boxed(format!("diff-{key}"))
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_color(theme.muted_foreground)
                            .child(path.clone()),
                    )
                    .children(lines)
                    .into_any_element()
            }
            ToolPreview::Todos { items } => div()
                .flex()
                .flex_col()
                .text_sm()
                .children(items.iter().map(|i| {
                    let mark = match i.status {
                        wraithgrid_chat::TodoStatus::Done => "☑",
                        wraithgrid_chat::TodoStatus::Active => "▸",
                        wraithgrid_chat::TodoStatus::Pending => "☐",
                    };
                    div()
                        .when(i.status == wraithgrid_chat::TodoStatus::Done, |d| {
                            d.text_color(theme.muted_foreground)
                        })
                        .child(format!("{mark} {}", i.text))
                }))
                .into_any_element(),
        }
    }

    /// One tool call (`ToolCard`): a line (mark, verb, path, state, chevron) that opens to its
    /// input and output. Closed by default, except the task list.
    fn tool_card(&mut self, t: &ToolItem, live: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let state = tool_state(t, live);
        // Claude's questions (#127): one quiet line; the questions are in the card below.
        if let Some(asked) = t
            .approval
            .as_ref()
            .and_then(|a| a.questions.as_ref().map(|q| q.len()))
            .filter(|n| *n > 0)
        {
            let waiting = state == ToolState::Waiting;
            let word = match t.approval.as_ref().and_then(|a| a.decided) {
                _ if waiting => "waiting for you",
                Some(ApprovalDecision::Deny) => "skipped",
                Some(_) => "answered",
                None => "",
            };
            return div()
                .px_2()
                .py_1()
                .flex()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("?")
                .child(div().flex_1().child(format!(
                    "Claude {} {asked} {}",
                    if waiting { "asks" } else { "asked" },
                    if asked == 1 { "question" } else { "questions" }
                )))
                .child(div().text_xs().child(word))
                .into_any_element();
        }
        let key = format!("tool:{}", t.id);
        let has_body = t.preview.is_some()
            || t.output.as_ref().is_some_and(|o| !o.trim().is_empty())
            || t.result.is_some();
        // A task list starts open; "opened" then means closed.
        let open = has_body && (self.x.opened.contains(&key) != (t.tool_kind == ToolKind::Todo));
        let cwd = self.setup.cwd.to_string_lossy().into_owned();
        let subtitle = relative_to(&t.subtitle, &cwd).to_string();
        let name = tool_verb(t.tool_kind).unwrap_or(&t.name).to_string();
        let state_el = {
            let (text, color) = match state {
                ToolState::Running => ("running…", theme.muted_foreground),
                ToolState::Waiting => ("needs approval", theme.warning),
                ToolState::Stopped => ("stopped", theme.muted_foreground),
                ToolState::Denied => ("✕ denied", theme.danger),
                ToolState::Error => ("✕", theme.danger),
                ToolState::Done => ("✓", theme.success),
            };
            div().flex_none().text_xs().text_color(color).child(text)
        };
        let decided = match t.approval.as_ref().and_then(|a| a.decided) {
            Some(ApprovalDecision::Allow) => Some("allowed"),
            Some(ApprovalDecision::Always) => Some("always allowed"),
            _ => None,
        };
        let head_key = key.clone();
        let header = div()
            .id(SharedString::from(format!("toolh-{}", t.id)))
            .role(Role::Button)
            .aria_label(format!("{name} {subtitle}"))
            .when(has_body, |d| d.aria_expanded(open))
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .when(has_body, |d| {
                d.cursor_pointer()
                    .hover(|d| d.bg(theme.secondary))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_opened(&head_key, cx)))
            })
            .child(
                div()
                    .flex_none()
                    .w(px(14.))
                    .text_color(theme.muted_foreground)
                    .child(tool_glyph(t.tool_kind)),
            )
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::MEDIUM)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(subtitle),
            )
            .when_some(decided, |d, s| {
                d.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(s),
                )
            })
            .child(state_el)
            .when(has_body, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(if open { "▾" } else { "▸" }),
                )
            });
        // A task's result, closed: its first line, which opens the card.
        let peek = t.result.as_ref().filter(|_| !open).map(|r| {
            let open_key = key.clone();
            div()
                .id(SharedString::from(format!("toolr-{}", t.id)))
                .px_2()
                .pb_1()
                .truncate()
                .text_xs()
                .cursor_pointer()
                .text_color(if r.ok {
                    theme.muted_foreground
                } else {
                    theme.danger
                })
                .child(plain_first_line(&r.text))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_opened(&open_key, cx)))
        });
        let body = open.then(|| {
            let preview = t.preview.as_ref().map(|p| self.preview(&t.id, p, cx));
            let output = t.output.as_ref().filter(|o| !o.trim().is_empty()).map(|o| {
                div()
                    .id(SharedString::from(format!("out-{}", t.id)))
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .p_2()
                    .rounded_md()
                    .bg(theme.secondary)
                    .font_family(crate::terminal::mono(cx))
                    .text_xs()
                    .when(t.is_error, |d| d.text_color(theme.danger))
                    .child(o.clone())
            });
            let result = t.result.as_ref().map(|r| {
                let state = self.markdown(&format!("res:{}", t.id), &r.text, cx);
                div()
                    .text_sm()
                    .when(!r.ok, |d| d.text_color(theme.danger))
                    .child(
                        TextView::new(&state)
                            .selectable(true)
                            .code_block_actions(|block, _, cx| code_copy(block.code(), cx)),
                    )
            });
            div()
                .px_2()
                .pb_2()
                .flex()
                .flex_col()
                .gap_2()
                .children(preview)
                .children(output)
                .children(result)
        });
        div()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(if state == ToolState::Waiting {
                theme.warning
            } else {
                theme.border
            })
            .child(header)
            .children(peek)
            .children(body)
            .into_any_element()
    }

    /// The question over the box while the agent waits to run a tool (`ApprovalBar`).
    pub(super) fn approval_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.pending_tool()?;
        let a = t.approval.clone()?;
        let theme = cx.theme().clone();
        let preview = t
            .preview
            .as_ref()
            .filter(|p| !matches!(p, ToolPreview::Todos { .. }))
            .map(|p| self.preview(&format!("ask-{}", t.id), p, cx));
        let button = |id: String, label: &'static str, primary: bool| {
            div()
                .id(SharedString::from(id))
                .a11y_button(label)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(primary, |d| {
                    d.bg(theme.primary).text_color(theme.primary_foreground)
                })
                .when(!primary, |d| {
                    d.border_1()
                        .border_color(theme.border)
                        .hover(|d| d.bg(theme.secondary))
                })
                .child(label)
        };
        let (allow, always, deny) = (a.req_id.clone(), a.req_id.clone(), a.req_id.clone());
        Some(
            div()
                .id(SharedString::from(format!("ask-{}", self.pane_id)))
                .role(Role::AlertDialog)
                .aria_label(format!("Allow {}?", t.name))
                .mx_2()
                .mt_2()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.warning)
                .bg(theme.popover)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.muted_foreground)
                                .child(tool_glyph(t.tool_kind)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .child("Allow ")
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("{}?", t.name)),
                                )
                                .flex(),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(t.subtitle.clone()),
                        ),
                )
                .children(preview)
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            button(format!("allow-{}", a.req_id), "Allow", true).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.approve(allow.clone(), ApprovalDecision::Allow, cx)
                                }),
                            ),
                        )
                        .when(a.can_always, |d| {
                            d.child(
                                button(format!("always-{}", a.req_id), "Always allow", false)
                                    .tooltip(|window, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(
                                            "Allow, and remember it the way the agent suggests",
                                        )
                                        .build(window, cx)
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.approve(always.clone(), ApprovalDecision::Always, cx)
                                    })),
                            )
                        })
                        .child(
                            button(format!("deny-{}", a.req_id), "Deny", false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.approve(deny.clone(), ApprovalDecision::Deny, cx)
                                }),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// "Thinking" / "Working" under the last row while a turn runs, with its time
    /// (`chat-working`): hidden while an answer streams in, or an approval waits.
    pub(super) fn working_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.thread.busy || self.thread.pending_approval().is_some() {
            return None;
        }
        let last = self.thread.items.last();
        let now = now_ms();
        if matches!(last, Some(ChatItem::Assistant { .. })) && now - self.x.text_at < TEXT_QUIET_MS
        {
            return None;
        }
        let label = match last {
            Some(ChatItem::Tool(_) | ChatItem::Assistant { .. }) => "Working",
            _ => "Thinking",
        };
        let elapsed = self.thread.turn_started_at.map_or(0, |t| now - t);
        let theme = cx.theme();
        let name = self
            .current_choice()
            .map_or_else(|| self.setup.cli.clone(), |a| a.name.clone());
        Some(
            div()
                .id(SharedString::from(format!("working-{}", self.pane_id)))
                .role(Role::Status)
                .aria_label(label)
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(crate::shell::provider_mark(&self.setup.cli, &name, 15., cx))
                .child(format!("{label}…"))
                .when(elapsed >= 2000, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .child(wraithgrid_chat::present::format_duration(elapsed as f64)),
                    )
                })
                .into_any_element(),
        )
    }

    /// A new chat's first screen (`chat-hello`): what to build here, the CLI, account and
    /// folder, and a few starters.
    pub(super) fn hello(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let home = wraithgrid_core::paths::home_dir();
        let cwd = wraithgrid_core::paths::contract_home(
            &self.setup.cwd.to_string_lossy(),
            &home.to_string_lossy(),
        );
        let base = wraithgrid_core::paths::base_name(&cwd);
        let ready = self.ready();
        let mut line = self.cli_label().to_string();
        if let Some(a) = self.current_choice() {
            line.push_str(&format!(" · {}", a.name));
        }
        div()
            .pt_8()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_center()
                    .child(format!(
                        "What should we build in {}?",
                        if base.is_empty() {
                            "this folder"
                        } else {
                            base.as_str()
                        }
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!("{line} ·"))
                    .child(div().font_family(crate::terminal::mono(cx)).child(cwd)),
            )
            .child(
                div()
                    .w_full()
                    .max_w(px(560.))
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_2()
                    .children(SUGGESTIONS.iter().enumerate().map(|(i, (text, hint))| {
                        let send = text.to_string();
                        div()
                            .id(SharedString::from(format!("sug-{i}")))
                            .a11y_button(text.to_string())
                            .w(px(260.))
                            .px_3()
                            .py_2()
                            .flex()
                            .flex_col()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.border)
                            .text_sm()
                            .when(ready, |d| {
                                d.cursor_pointer()
                                    .hover(|d| d.bg(theme.secondary))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.set_box(send.clone(), None, window, cx);
                                        this.send(window, cx);
                                    }))
                            })
                            .when(!ready, |d| d.opacity(0.5))
                            .child(text.to_string())
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(hint.to_string()),
                            )
                    })),
            )
            .into_any_element()
    }
}
