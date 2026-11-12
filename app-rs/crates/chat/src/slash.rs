//! The `/` menu in the chat box, ported from `src/renderer/lib/slash.ts`: Wraithgrid's own
//! commands, then the CLI's skills and commands (from its own list); the ones that only work in
//! the CLI's screen are left out, as chats have no terminal view (#110). codex skills are named
//! as `$name` instead, and also open from a `$` word (#96). An `@` word lists the project's
//! files (#159). Pure.
//!
//! Carets are byte offsets into the text (what a Rust text field holds); the TS used UTF-16
//! indices. A caret inside a character is moved back to its start.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::text::{floor_boundary, locale_cmp, subsequence};
use crate::types::ChatCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlashGroup {
    App,
    Skill,
    Builtin,
    File,
}

const GROUP_ORDER: [SlashGroup; 4] = [
    SlashGroup::App,
    SlashGroup::Skill,
    SlashGroup::Builtin,
    SlashGroup::File,
];

impl SlashGroup {
    /// The group's heading in the menu.
    pub fn label(self) -> &'static str {
        match self {
            SlashGroup::App => "Wraithgrid",
            SlashGroup::Skill => "Skills and commands",
            SlashGroup::Builtin => "Built in",
            SlashGroup::File => "Files",
        }
    }

    fn order(self) -> usize {
        GROUP_ORDER.iter().position(|g| *g == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashItem {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub aliases: Vec<String>,
    pub group: SlashGroup,
    /// The plugin a skill comes from (`postman` of `postman:api-testing`).
    pub source: Option<String>,
    /// What goes before the name in the text: `$` for a codex skill, `@` for a file.
    pub sigil: char,
}

fn app(name: &str, description: &str, aliases: &[&str]) -> SlashItem {
    SlashItem {
        name: name.into(),
        description: description.into(),
        argument_hint: String::new(),
        aliases: aliases.iter().map(|a| a.to_string()).collect(),
        group: SlashGroup::App,
        source: None,
        sigil: '/',
    }
}

/// Handled by Wraithgrid itself; the CLI's commands of the same name are hidden.
pub fn app_commands() -> Vec<SlashItem> {
    vec![
        app("new", "Start a new conversation in this chat", &["clear"]),
        app("model", "Switch the model", &[]),
        app("effort", "How long it thinks: low to max", &[]),
        app(
            "mode",
            "How much it may do without asking: plan, accept edits, auto…",
            &["permissions"],
        ),
        app(
            "usage",
            "Open the usage panel: limits, cost and tokens",
            &[],
        ),
    ]
}

/// claude starts a plugin's descriptions with "(plugin) "; the badge says that.
fn strip_plugin_prefix(description: &str) -> &str {
    if let Some(rest) = description.strip_prefix('(') {
        if let Some(end) = rest.find(')') {
            return rest[end + 1..].trim_start();
        }
    }
    description
}

/// The menu's items for a chat: ours, then the CLI's, grouped.
pub fn build_slash_items(commands: &[ChatCommand], terminal_only: &[String]) -> Vec<SlashItem> {
    let ours = app_commands();
    let taken: HashSet<&str> = ours
        .iter()
        .flat_map(|c| std::iter::once(c.name.as_str()).chain(c.aliases.iter().map(String::as_str)))
        .collect();
    let term: HashSet<&str> = terminal_only.iter().map(String::as_str).collect();
    let theirs: Vec<SlashItem> = commands
        .iter()
        .filter(|c| !taken.contains(c.name.as_str()) && !term.contains(c.name.as_str()))
        .map(|c| {
            let source = c.name.find(':').map(|i| c.name[..i].to_string());
            SlashItem {
                name: c.name.clone(),
                description: if source.is_some() {
                    strip_plugin_prefix(&c.description).to_string()
                } else {
                    c.description.clone()
                },
                argument_hint: c.argument_hint.clone(),
                aliases: c.aliases.clone(),
                group: if c.builtin {
                    SlashGroup::Builtin
                } else {
                    SlashGroup::Skill
                },
                source,
                sigil: if c.mention { '$' } else { '/' },
            }
        })
        .collect();
    let mut all: Vec<SlashItem> = ours.into_iter().chain(theirs).collect();
    all.sort_by_key(|i| i.group.order());
    all
}

/// The text after `/` while the caret is still in the first word of a message that starts
/// with `/`; None when the menu should not show.
pub fn slash_query(text: &str, caret: usize) -> Option<&str> {
    if !text.starts_with('/') {
        return None;
    }
    let head = &text[..floor_boundary(text, caret)];
    if caret < 1 || head.chars().any(char::is_whitespace) {
        return None;
    }
    Some(&head[1..])
}

/// Where a menu word starts (at its sigil) and the text after the sigil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordQuery {
    pub start: usize,
    pub query: String,
}

static MENTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)\$([A-Za-z0-9_.:-]{0,100})$").expect("valid regex"));
static FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)@([^\s@]{0,300})$").expect("valid regex"));

fn word_query(re: &Regex, text: &str, caret: usize) -> Option<WordQuery> {
    let caret = floor_boundary(text, caret);
    let m = re.captures(&text[..caret])?.get(1)?;
    Some(WordQuery {
        start: m.start() - 1,
        query: m.as_str().to_string(),
    })
}

/// The `$word` the caret ends (start of the text or after a space), for the skills menu;
/// None when the caret is not in one.
pub fn mention_query(text: &str, caret: usize) -> Option<WordQuery> {
    word_query(&MENTION_RE, text, caret)
}

/// The `@path` the caret ends (start of the text or after a space), for the file menu; None
/// when the caret is not in one (an email's `@` isn't).
pub fn file_query(text: &str, caret: usize) -> Option<WordQuery> {
    word_query(&FILE_RE, text, caret)
}

/// A path from the file list as a menu item.
pub fn file_item(path: &str) -> SlashItem {
    SlashItem {
        name: path.to_string(),
        description: String::new(),
        argument_hint: String::new(),
        aliases: Vec::new(),
        group: SlashGroup::File,
        source: None,
        sigil: '@',
    }
}

/// One run of a sent message: plain text or an `@path` mention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPart {
    pub text: String,
    pub mention: bool,
}

static SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|\s)(@[^\s@]*[./][^\s@]*)").expect("valid regex"));

/// A sent message's `@path` mentions (with a `/` or `.` in them, so `@someone` isn't one).
pub fn split_mentions(text: &str) -> Vec<TextPart> {
    let mut out = Vec::new();
    let mut at = 0;
    let part = |text: &str, mention| TextPart {
        text: text.to_string(),
        mention,
    };
    for c in SPLIT_RE.captures_iter(text) {
        let Some(m) = c.get(2) else { continue };
        if m.start() > at {
            out.push(part(&text[at..m.start()], false));
        }
        out.push(part(m.as_str(), true));
        at = m.end();
    }
    if at < text.len() {
        out.push(part(&text[at..], false));
    }
    out
}

fn score(item: &SlashItem, q: &str) -> u32 {
    let name = item.name.to_lowercase();
    let short = match name.find(':') {
        Some(i) => &name[i + 1..],
        None => name.as_str(),
    };
    let aliases: Vec<String> = item.aliases.iter().map(|a| a.to_lowercase()).collect();
    if name == q || short == q || aliases.iter().any(|a| a == q) {
        return 100;
    }
    if name.starts_with(q) {
        return 80;
    }
    if short.starts_with(q) {
        return 75;
    }
    if aliases.iter().any(|a| a.starts_with(q)) {
        return 70;
    }
    if name.contains(q) {
        return 50;
    }
    if subsequence(q, short) {
        return 30;
    }
    if q.chars().count() >= 3 && item.description.to_lowercase().contains(q) {
        return 15;
    }
    0
}

/// The menu's default length.
pub const SLASH_LIMIT: usize = 80;

/// Items matching what was typed, best first; everything (grouped) for a bare `/`.
pub fn rank_slash(items: &[SlashItem], query: &str, limit: usize) -> Vec<SlashItem> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return items.iter().take(limit).cloned().collect();
    }
    let mut scored: Vec<(u32, &SlashItem)> = items
        .iter()
        .map(|item| (score(item, &q), item))
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| {
        sb.cmp(sa)
            .then(a.group.order().cmp(&b.group.order()))
            .then_with(|| locale_cmp(&a.name, &b.name))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, i)| i.clone())
        .collect()
}

/// A sent message's leading `/command` and what follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadingCommand {
    pub name: String,
    pub rest: String,
}

static LEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^/([A-Za-z0-9_.:-]{1,100})(\s.*)?$").expect("valid regex"));

/// A sent message's leading `/command`, to draw it as a chip; None for plain text.
pub fn leading_command(text: &str) -> Option<LeadingCommand> {
    let c = LEADING_RE.captures(text)?;
    Some(LeadingCommand {
        name: c.get(1)?.as_str().to_string(),
        rest: c.get(2).map_or("", |m| m.as_str()).trim_start().to_string(),
    })
}
