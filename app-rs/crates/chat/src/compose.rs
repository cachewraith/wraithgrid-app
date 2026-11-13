//! The pure parts of the message box, ported from `src/renderer/lib/{quotes,prompt-history,
//! questions}.ts`: reply quotes (#164), prompt history (#132) and Claude's questions asked one
//! at a time (#127). Lengths count characters (the TS counted UTF-16 units); carets are byte
//! offsets.

use crate::thread::ChatItem;
use crate::types::{take_chars, ChatQuestion, CHAT_TEXT_MAX};

// --- Reply quotes (#164) ---

/// Quotes one message can carry.
pub const QUOTES_MAX: usize = 5;

/// A part of the chat the message replies to, sent first as a blockquote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub text: String,
    /// Cut at CHAT_TEXT_MAX.
    pub trimmed: bool,
}

/// A selection as a quote: blank lines around it dropped, cut at `max`; None if blank.
pub fn make_quote(selected: &str, max: usize) -> Option<Quote> {
    let text = selected.replace("\r\n", "\n").replace('\r', "\n");
    // Leading blank lines go; the first line keeps its indent.
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        if !line.ends_with('\n') || !line.trim().is_empty() {
            break;
        }
        start += line.len();
    }
    let text = text[start..].trim_end();
    if text.trim().is_empty() {
        return None;
    }
    Some(if text.chars().count() > max {
        Quote {
            text: take_chars(text, max).trim_end().to_string(),
            trimmed: true,
        }
    } else {
        Quote {
            text: text.to_string(),
            trimmed: false,
        }
    })
}

/// [`make_quote`] at the message limit.
pub fn quote_of(selected: &str) -> Option<Quote> {
    make_quote(selected, CHAT_TEXT_MAX)
}

/// Code as a fenced block, with a fence longer than any run of backticks inside it.
pub fn fence_code(code: &str, lang: &str) -> String {
    let mut run = 0;
    let mut longest = 0;
    for ch in code.chars() {
        run = if ch == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}{lang}\n{}\n{fence}", code.trim_end_matches('\n'))
}

/// One quote as a blockquote: every line behind `> ` (an empty one behind a bare `>`).
pub fn quote_block(q: &Quote) -> String {
    let body = if q.trimmed {
        format!("{}\n[…trimmed]", q.text)
    } else {
        q.text.clone()
    };
    body.split('\n')
        .map(|l| {
            if l.is_empty() {
                ">".to_string()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The message as sent: the quotes first, a blank line between blocks, then the typed text.
pub fn with_quotes(quotes: &[Quote], body: &str) -> String {
    quotes
        .iter()
        .map(quote_block)
        .chain(std::iter::once(body.to_string()))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A sent message's leading blockquotes, unquoted, and the text after them, to draw them apart.
pub fn split_quotes(text: &str) -> (Vec<String>, String) {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut quotes = Vec::new();
    let mut i = 0;
    while i < lines.len() && lines[i].starts_with('>') {
        let mut block = Vec::new();
        while i < lines.len() && lines[i].starts_with('>') {
            let l = &lines[i][1..];
            block.push(l.strip_prefix(' ').unwrap_or(l));
            i += 1;
        }
        quotes.push(block.join("\n"));
        // Blank lines between two quotes, or before the text.
        while i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
    }
    (quotes, lines[i.min(lines.len())..].join("\n"))
}

// --- Prompt history (#132) ---

/// Strips the file list `with_attachments` adds after a message: not something you typed.
fn without_attached(text: &str) -> &str {
    let (head, list) = if let Some(rest) = text.strip_prefix("Attached files:\n") {
        ("", rest)
    } else if let Some(i) = text.rfind("\n\nAttached files:\n") {
        (&text[..i], &text[i + "\n\nAttached files:\n".len()..])
    } else {
        return text;
    };
    let list = list.strip_suffix('\n').unwrap_or(list);
    if !list.is_empty() && list.split('\n').all(|l| l.starts_with("- ")) {
        head
    } else {
        text
    }
}

/// Sent prompts, oldest first; blank ones dropped and a repeat of the one before stored once.
pub fn prompt_history<'a>(user_texts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in user_texts {
        let text = without_attached(t).trim();
        if !text.is_empty() && out.last().map(String::as_str) != Some(text) {
            out.push(text.to_string());
        }
    }
    out
}

/// Where browsing is: an entry of the list, or None (the draft); `draft` is kept meanwhile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryNav {
    pub index: Option<usize>,
    pub draft: String,
}

/// One step: `up` to an older prompt, else to a newer one or back to the draft. None when there
/// is nowhere to go (the oldest already, or not browsing on ↓), so the key does nothing.
pub fn history_step(
    list: &[String],
    nav: &HistoryNav,
    up: bool,
    current: &str,
) -> Option<(HistoryNav, String)> {
    if list.is_empty() {
        return None;
    }
    let Some(at) = nav.index else {
        if !up {
            return None;
        }
        let index = list.len() - 1;
        return Some((
            HistoryNav {
                index: Some(index),
                draft: current.to_string(),
            },
            list[index].clone(),
        ));
    };
    if up {
        let index = at.checked_sub(1)?;
        return Some((
            HistoryNav {
                index: Some(index),
                draft: nav.draft.clone(),
            },
            list[index].clone(),
        ));
    }
    if at + 1 >= list.len() {
        return Some((HistoryNav::default(), nav.draft.clone()));
    }
    Some((
        HistoryNav {
            index: Some(at + 1),
            draft: nav.draft.clone(),
        },
        list[at + 1].clone(),
    ))
}

/// ↑ only from the first line, ↓ only from the last; elsewhere the caret moves between lines.
pub fn at_edge(text: &str, start: usize, end: usize, up: bool) -> bool {
    if start != end {
        return false;
    }
    let at = crate::text::floor_boundary(text, start);
    if up {
        !text[..at].contains('\n')
    } else {
        !text[at..].contains('\n')
    }
}

// --- Claude's questions (#127) ---

/// claude marks its pick by ending a label with "(Recommended)": shown as a tag instead.
pub fn split_recommended(label: &str) -> (&str, bool) {
    let t = label.trim_end();
    const TAG: &str = "(recommended)";
    if t.len() >= TAG.len() && t.is_char_boundary(t.len() - TAG.len()) {
        let (head, tag) = t.split_at(t.len() - TAG.len());
        let head = head.trim_end();
        if tag.eq_ignore_ascii_case(TAG) && !head.is_empty() {
            return (head, true);
        }
    }
    (label, false)
}

/// Where a pick on question `at` goes: the next unanswered one after it, else the next one.
pub fn next_question(at: usize, answered: &[bool]) -> usize {
    (at + 1..answered.len())
        .find(|&i| !answered[i])
        .unwrap_or_else(|| (at + 1).min(answered.len().saturating_sub(1)))
}

/// What was picked for one question.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionPick {
    pub labels: Vec<String>,
    pub other: String,
    /// The "Other…" row is picked: its text is the answer.
    pub other_on: bool,
}

impl QuestionPick {
    /// Picks `label`: toggled on a multi-choice question, the only pick on a one-choice one.
    pub fn toggle(&mut self, label: &str, multi: bool) {
        if multi {
            if let Some(i) = self.labels.iter().position(|l| l == label) {
                self.labels.remove(i);
            } else {
                self.labels.push(label.to_string());
            }
        } else {
            self.labels = vec![label.to_string()];
            self.other_on = false;
        }
    }
}

/// The answer claude reads for one question: the labels (", " between), or the own text.
pub fn answer_of(q: &ChatQuestion, p: &QuestionPick) -> String {
    let other = p.other.trim();
    let mut parts: Vec<&str> = p.labels.iter().map(String::as_str).collect();
    if p.other_on && !other.is_empty() {
        parts.push(other);
    }
    if q.multi_select {
        parts.join(", ")
    } else {
        parts.first().map(|s| s.to_string()).unwrap_or_default()
    }
}

// --- Cross-CLI handoffs (#78) ---

/// The first message of a chat continued in another CLI (`buildHandoff`): the visible text
/// only, newest messages kept under the size cap. The text, and how many messages it carries.
pub fn build_handoff(
    items: &[ChatItem],
    provider: &str,
    account: &str,
    max_chars: usize,
    max_messages: usize,
) -> (String, u64) {
    let header = format!("Continuing a conversation from {provider} ({account}):\n\n");
    let mut remaining = max_chars.saturating_sub(header.chars().count());
    let mut parts: Vec<String> = Vec::new();
    for item in items.iter().rev() {
        if parts.len() >= max_messages {
            break;
        }
        let (kind, text) = match item {
            ChatItem::User { text, .. } => ("user", text),
            ChatItem::Assistant { text, .. } => ("assistant", text),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        let prefix = format!("{kind}: ");
        let gap = if parts.is_empty() { 0 } else { 2 };
        if remaining <= prefix.len() + gap {
            break;
        }
        let available = remaining - prefix.len() - gap;
        let len = text.chars().count();
        // Too long: its end is kept, and nothing older fits after it.
        let kept: String = if len > available {
            text.chars().skip(len - available).collect()
        } else {
            text.clone()
        };
        let cut = kept.chars().count() != len;
        remaining -= prefix.len() + kept.chars().count() + gap;
        parts.insert(0, format!("{prefix}{kept}"));
        if cut {
            break;
        }
    }
    let text: String = format!("{header}{}", parts.join("\n\n"))
        .chars()
        .take(max_chars)
        .collect();
    (text, parts.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(user: bool, text: &str) -> ChatItem {
        if user {
            ChatItem::User {
                id: "u".into(),
                text: text.into(),
                images: 0,
                thumbs: None,
            }
        } else {
            ChatItem::Assistant {
                id: "a".into(),
                text: text.into(),
            }
        }
    }

    #[test]
    fn handoffs_carry_the_newest_messages() {
        let items = vec![said(true, "fix it"), said(false, "done"), said(true, "  ")];
        let (text, n) = build_handoff(&items, "Claude", "home", 20_000, 60);
        assert_eq!(
            text,
            "Continuing a conversation from Claude (home):\n\nuser: fix it\n\nassistant: done"
        );
        assert_eq!(n, 2);
        // Over the cap: the newest message's end survives, older ones go.
        let long = "x".repeat(100);
        let items = vec![said(true, "old"), said(false, &long)];
        let (text, n) = build_handoff(&items, "Claude", "home", 80, 60);
        assert_eq!(n, 1);
        assert!(text.ends_with("xxx") && !text.contains("old"));
        assert!(text.chars().count() <= 80);
        let (_, n) = build_handoff(&[said(true, "a"), said(true, "b")], "Codex", "w", 20_000, 1);
        assert_eq!(n, 1);
    }

    fn q(text: &str) -> Quote {
        Quote {
            text: text.into(),
            trimmed: false,
        }
    }

    #[test]
    fn makes_quotes() {
        assert_eq!(
            make_quote("\n\n  keep the indent\nsecond\n\n", 100),
            Some(q("  keep the indent\nsecond"))
        );
        assert_eq!(make_quote("a\r\nb", 100), Some(q("a\nb")));
        assert_eq!(make_quote("  \n\t ", 100), None);
        assert_eq!(make_quote("", 100), None);
        let cut = make_quote("abcdefgh", 5).unwrap();
        assert_eq!(
            cut,
            Quote {
                text: "abcde".into(),
                trimmed: true
            }
        );
        assert_eq!(quote_block(&cut), "> abcde\n> […trimmed]");
    }

    #[test]
    fn quotes_go_first_and_code_stays_fenced() {
        assert_eq!(
            quote_block(&q("Remember to turn it off\n\n(or keep the filter)")),
            "> Remember to turn it off\n>\n> (or keep the filter)"
        );
        let (a, b) = (q("first"), q("second\nline"));
        assert_eq!(
            with_quotes(&[a.clone(), b], "why this?"),
            "> first\n\n> second\n> line\n\nwhy this?"
        );
        assert_eq!(with_quotes(std::slice::from_ref(&a), ""), "> first");
        assert_eq!(with_quotes(&[], "just text"), "just text");
        let code = fence_code("const a = 1\n", "ts");
        assert_eq!(code, "```ts\nconst a = 1\n```");
        assert_eq!(quote_block(&q(&code)), "> ```ts\n> const a = 1\n> ```");
        assert_eq!(fence_code("a ``` b", ""), "````\na ``` b\n````");
    }

    #[test]
    fn splits_sent_quotes() {
        let quotes = [q("first"), q("> nested\n\nwith a gap")];
        assert_eq!(
            split_quotes(&with_quotes(&quotes, "why?\n\n> later quote")),
            (
                vec!["first".to_string(), "> nested\n\nwith a gap".to_string()],
                "why?\n\n> later quote".to_string()
            )
        );
        assert_eq!(
            split_quotes("no quote > here"),
            (vec![], "no quote > here".to_string())
        );
        assert_eq!(
            split_quotes("> only"),
            (vec!["only".to_string()], "".into())
        );
    }

    #[test]
    fn history_is_the_sent_prompts() {
        let texts = [
            "fix the build",
            "fix the build",
            "  run the tests ",
            "",
            "fix the build",
            "read these\n\nAttached files:\n- /tmp/a.txt\n- /tmp/b.txt",
            "Attached files:\n- /tmp/c.txt",
        ];
        assert_eq!(
            prompt_history(texts),
            [
                "fix the build",
                "run the tests",
                "fix the build",
                "read these"
            ]
        );
    }

    #[test]
    fn history_steps_and_brings_the_draft_back() {
        let list: Vec<String> = ["one", "two", "three"].map(String::from).to_vec();
        let mut nav = HistoryNav::default();
        let mut step = |up: bool, cur: &str| -> Option<String> {
            let (n, t) = history_step(&list, &nav, up, cur)?;
            nav = n;
            Some(t)
        };
        assert_eq!(step(false, "my draft"), None);
        assert_eq!(step(true, "my draft").as_deref(), Some("three"));
        assert_eq!(step(true, "three").as_deref(), Some("two"));
        assert_eq!(step(true, "two").as_deref(), Some("one"));
        assert_eq!(step(true, "one"), None);
        assert_eq!(step(false, "one").as_deref(), Some("two"));
        assert_eq!(step(false, "two").as_deref(), Some("three"));
        assert_eq!(step(false, "three").as_deref(), Some("my draft"));
        assert_eq!(nav, HistoryNav::default());
        assert_eq!(history_step(&[], &HistoryNav::default(), true, "x"), None);
    }

    #[test]
    fn history_only_from_the_edge_lines() {
        let text = "first line\nsecond line\nthird";
        assert!(at_edge(text, 3, 3, true));
        assert!(!at_edge(text, 14, 14, true));
        assert!(!at_edge(text, 14, 14, false));
        assert!(at_edge(text, text.len() - 1, text.len() - 1, false));
        assert!(!at_edge(text, 0, 5, true));
        assert!(at_edge("", 0, 0, true));
        assert!(at_edge("", 0, 0, false));
    }

    #[test]
    fn questions_step() {
        assert_eq!(
            split_recommended("Callback + sweep (Recommended)"),
            ("Callback + sweep", true)
        );
        assert_eq!(
            split_recommended("Don’t approve, alert (recommended) "),
            ("Don’t approve, alert", true)
        );
        assert_eq!(split_recommended("Callback only"), ("Callback only", false));
        assert_eq!(
            split_recommended("(Recommended) first"),
            ("(Recommended) first", false)
        );
        assert_eq!(split_recommended("(Recommended)"), ("(Recommended)", false));
        assert_eq!(next_question(0, &[true, false, false]), 1);
        assert_eq!(next_question(0, &[true, true, false]), 2);
        assert_eq!(next_question(0, &[true, true, true]), 1);
        assert_eq!(next_question(2, &[true, true, true]), 2);
    }

    #[test]
    fn answers_join_picks() {
        let mut q = ChatQuestion {
            question: "Which?".into(),
            header: "Pick".into(),
            multi_select: false,
            options: vec![],
        };
        let mut p = QuestionPick::default();
        p.toggle("A", false);
        p.toggle("B", false);
        assert_eq!(answer_of(&q, &p), "B");
        q.multi_select = true;
        let mut p = QuestionPick::default();
        p.toggle("A", true);
        p.toggle("B", true);
        p.other = " mine ".into();
        p.other_on = true;
        assert_eq!(answer_of(&q, &p), "A, B, mine");
        p.toggle("A", true);
        assert_eq!(answer_of(&q, &p), "B, mine");
    }
}
