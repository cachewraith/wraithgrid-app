//! How the chat view words a transcript's rows, ported from `ToolCard.tsx`, `ChatView.tsx` and
//! `src/renderer/lib/markdown.ts` (`plainFirstLine`): a tool's verb and state, its path relative
//! to the pane's folder, a turn's footer. Pure, so it is tested without a window.

use crate::thread::ToolItem;
use crate::types::{ApprovalDecision, ToolKind};

/// A tool's name as a verb phrase (`KIND_LABEL`); None: its own name is shown.
pub fn tool_verb(kind: ToolKind) -> Option<&'static str> {
    Some(match kind {
        ToolKind::Shell => "Ran",
        ToolKind::Edit => "Edited",
        ToolKind::Read => "Read",
        ToolKind::Search => "Searched",
        ToolKind::Web => "Fetched",
        ToolKind::Agent => "Agent",
        ToolKind::Todo => "Tasks",
        ToolKind::Other => return None,
    })
}

/// The tool's mark before its name (`KIND_ICON`, as a glyph the UI font has).
pub fn tool_glyph(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Shell => "$",
        ToolKind::Edit => "✎",
        ToolKind::Read => "▤",
        ToolKind::Search => "⌕",
        ToolKind::Web => "◎",
        ToolKind::Agent => "✦",
        ToolKind::Todo => "☰",
        ToolKind::Other => "⚙",
    }
}

/// Where a tool call is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    /// Its approval waits for the user (only while the turn runs).
    Waiting,
    Denied,
    Running,
    /// No output and the turn is over: stopped, or the CLI exited.
    Stopped,
    Error,
    Done,
}

/// The state of a tool card; `live`: the turn still runs.
pub fn tool_state(t: &ToolItem, live: bool) -> ToolState {
    let decided = t.approval.as_ref().map(|a| a.decided);
    match decided {
        Some(None) if live => ToolState::Waiting,
        Some(Some(ApprovalDecision::Deny)) => ToolState::Denied,
        _ if t.output.is_none() && live => ToolState::Running,
        _ if t.output.is_none() => ToolState::Stopped,
        _ if t.is_error => ToolState::Error,
        _ => ToolState::Done,
    }
}

/// A path inside the pane's folder, shown relative to it.
pub fn relative_to<'a>(subtitle: &'a str, cwd: &str) -> &'a str {
    let cwd = cwd.trim_end_matches(['/', '\\']);
    if cwd.is_empty() {
        return subtitle;
    }
    subtitle
        .strip_prefix(cwd)
        .and_then(|rest| rest.strip_prefix(['/', '\\']))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(subtitle)
}

/// The first line with words of a markdown text, without its markup (`plainFirstLine`).
pub fn plain_first_line(md: &str) -> String {
    let line = md
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("```"))
        .unwrap_or("");
    let line = line.trim_start_matches(['#', '>', ' ']);
    let line = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line);
    line.chars()
        .filter(|c| !matches!(c, '*' | '`' | '_'))
        .collect::<String>()
        .trim()
        .to_string()
}

/// `1m 5s`-style elapsed time (`formatDuration`).
pub fn format_duration(ms: f64) -> String {
    let s = (ms.max(0.0) / 1000.0).round() as i64;
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m {}s", s / 60, s % 60)
    }
}

/// A turn's cost: 4 decimals under a cent; None when there is none (or it is zero).
pub fn format_cost(usd: Option<f64>) -> Option<String> {
    let c = usd.filter(|c| *c > 0.0)?;
    Some(if c < 0.01 {
        format!("${c:.4}")
    } else {
        format!("${c:.2}")
    })
}

/// The line under a finished turn ("1m 5s · $0.0042"); None when it has nothing to say.
pub fn turn_footer(cost_usd: Option<f64>, duration_ms: Option<f64>) -> Option<String> {
    let parts: Vec<String> = duration_ms
        .map(format_duration)
        .into_iter()
        .chain(format_cost(cost_usd))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::ToolApproval;

    fn tool(
        output: Option<&str>,
        is_error: bool,
        decided: Option<Option<ApprovalDecision>>,
    ) -> ToolItem {
        ToolItem {
            id: "t".into(),
            name: "Bash".into(),
            tool_kind: ToolKind::Shell,
            subtitle: String::new(),
            preview: None,
            output: output.map(String::from),
            is_error,
            approval: decided.map(|d| ToolApproval {
                req_id: "r".into(),
                can_always: false,
                decided: d,
                questions: None,
                answers: None,
            }),
            result: None,
        }
    }

    #[test]
    fn says_where_a_tool_is() {
        assert_eq!(
            tool_state(&tool(None, false, None), true),
            ToolState::Running
        );
        assert_eq!(
            tool_state(&tool(None, false, None), false),
            ToolState::Stopped
        );
        assert_eq!(
            tool_state(&tool(Some("x"), true, None), false),
            ToolState::Error
        );
        assert_eq!(
            tool_state(&tool(Some("x"), false, None), true),
            ToolState::Done
        );
        assert_eq!(
            tool_state(&tool(None, false, Some(None)), true),
            ToolState::Waiting
        );
        // A question nobody can answer any more is not waiting.
        assert_eq!(
            tool_state(&tool(None, false, Some(None)), false),
            ToolState::Stopped
        );
        assert_eq!(
            tool_state(&tool(None, false, Some(Some(ApprovalDecision::Deny))), true),
            ToolState::Denied
        );
    }

    #[test]
    fn paths_inside_the_folder_read_relative() {
        assert_eq!(relative_to("/home/a/p/src/x.rs", "/home/a/p"), "src/x.rs");
        assert_eq!(relative_to("/home/a/p/src/x.rs", "/home/a/p/"), "src/x.rs");
        assert_eq!(relative_to("/home/a/px/y", "/home/a/p"), "/home/a/px/y");
        assert_eq!(relative_to("ls -la", "/home/a/p"), "ls -la");
        assert_eq!(relative_to("/x", ""), "/x");
    }

    #[test]
    fn first_line_without_markup() {
        assert_eq!(
            plain_first_line("\n## **Done**: all `ok`\nmore"),
            "Done: all ok"
        );
        assert_eq!(plain_first_line("- item"), "item");
        assert_eq!(plain_first_line(""), "");
    }

    #[test]
    fn footer_like_electron() {
        assert_eq!(format_duration(1_400.0), "1s");
        assert_eq!(format_duration(65_000.0), "1m 5s");
        assert_eq!(format_cost(Some(0.0042)), Some("$0.0042".into()));
        assert_eq!(format_cost(Some(0.126)), Some("$0.13".into()));
        assert_eq!(format_cost(Some(0.0)), None);
        assert_eq!(turn_footer(Some(0.0), None), None);
        assert_eq!(
            turn_footer(Some(0.5), Some(65_000.0)),
            Some("1m 5s · $0.50".into())
        );
    }
}
