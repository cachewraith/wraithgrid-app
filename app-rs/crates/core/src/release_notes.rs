//! Release notes for About (`src/shared/whats-new.ts`, `src/main/release-notes.ts`):
//! docs/WHATS-NEW.md split per version (bundled, so the installed version's notes read
//! offline), the notes of every release a check found kept in userData/release-notes.json
//! (capped) for after the update, and the version that ran last ("updated from").

use std::path::Path;

use regex::Regex;
use serde_json::{json, Value};

use crate::update::{compare_versions, parse_version, ReleaseInfo};

/// Versions kept besides the installed one, and characters per entry.
pub const MAX_EARLIER: usize = 10;
pub const MAX_NOTE_CHARS: usize = 8000;

#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseNote {
    /// Without the leading `v`, e.g. "1.12.0".
    pub version: String,
    /// The heading after "What's new in 1.12:" / "Previously, in 1.11:".
    pub title: String,
    /// `YYYY-MM-DD`, or an ISO time for notes saved from GitHub.
    pub date: Option<String>,
    /// Markdown.
    pub notes: String,
}

fn heading() -> Regex {
    Regex::new(r"^#{1,2}\s+(.+?)\s*$").unwrap()
}

fn stamp() -> Regex {
    Regex::new(r"^\*v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)\s*·\s*(\d{4}-\d{2}-\d{2})\*\s*$").unwrap()
}

fn strip_prefix(title: &str) -> String {
    let re = Regex::new(r"(?i)^(?:what['’]s new in|previously, in)\s+[\d.]+\s*:\s*").unwrap();
    re.replace(title, "").into_owned()
}

/// A version's section starts at a heading followed by a `*vX.Y.Z · YYYY-MM-DD*` line and
/// runs until the next such heading. `---` rules between sections are dropped.
pub fn parse_whats_new(md: &str) -> Vec<ReleaseNote> {
    let (heading, stamp) = (heading(), stamp());
    let text = md.replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    // (heading line, title, version, date, first body line)
    let mut starts: Vec<(usize, String, String, String, usize)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(h) = heading.captures(line) else {
            continue;
        };
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim().is_empty() {
            j += 1;
        }
        if let Some(s) = lines.get(j).and_then(|l| stamp.captures(l)) {
            starts.push((i, strip_prefix(&h[1]), s[1].into(), s[2].into(), j + 1));
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, (_, title, version, date, body))| {
            let end = starts.get(k + 1).map_or(lines.len(), |s| s.0);
            let notes = lines[*body..end]
                .iter()
                .filter(|l| l.trim() != "---")
                .copied()
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            ReleaseNote {
                version: version.clone(),
                title: title.clone(),
                date: Some(date.clone()),
                notes,
            }
        })
        .collect()
}

/// One row of a `| New | … |` or `| Fix | … |` table: the change's name and its sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteItem {
    pub name: String,
    /// Markdown (bold, code, links).
    pub text: String,
}

/// A version's notes split for About (#124): intro text, then New and Fixed as items.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct NoteParts {
    /// From a leading `## …` heading (a GitHub release body has one).
    pub title: Option<String>,
    /// Everything that isn't a New/Fix table, as markdown (other tables stay in it).
    pub intro: String,
    pub added: Vec<NoteItem>,
    pub fixed: Vec<NoteItem>,
}

fn is_table_row(l: &str) -> bool {
    let t = l.trim();
    t.len() >= 2 && t.starts_with('|') && t.ends_with('|')
}

fn is_table_rule(l: &str) -> bool {
    Regex::new(r"^\s*\|(?:\s*:?-{2,}:?\s*\|)+\s*$")
        .unwrap()
        .is_match(l)
}

/// Table cells, split on `|` that is neither escaped nor inside backticks.
fn cells(row: &str) -> Vec<String> {
    let s = row.trim();
    let s = s.strip_prefix('|').unwrap_or(s);
    let s = s.strip_suffix('|').unwrap_or(s);
    let (mut out, mut cur, mut code, mut prev) = (Vec::new(), String::new(), false, None);
    for c in s.chars() {
        if c == '`' {
            code = !code;
        }
        if c == '|' && !code && prev != Some('\\') {
            out.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
        prev = Some(c);
    }
    out.push(cur.trim().to_string());
    out
}

/// Splits a version's notes into intro + New + Fixed. Tables headed "New"/"Feature" or
/// "Fix"/"Fixed" become items (first cell the name, the rest the sentence).
pub fn note_parts(md: &str) -> NoteParts {
    let text = md.replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let (heading, stamp) = (heading(), stamp());
    let mut first = 0;
    while first < lines.len() && lines[first].trim().is_empty() {
        first += 1;
    }
    let mut parts = NoteParts::default();
    if let Some(h) = lines.get(first).and_then(|l| heading.captures(l)) {
        parts.title = Some(strip_prefix(&h[1]));
        first += 1;
    }
    let mut intro: Vec<&str> = Vec::new();
    let mut i = first;
    while i < lines.len() {
        let line = lines[i];
        if !is_table_row(line) || !lines.get(i + 1).is_some_and(|l| is_table_rule(l)) {
            if line.trim() != "---" && !stamp.is_match(line) {
                intro.push(line);
            }
            i += 1;
            continue;
        }
        let mut end = i + 2;
        while end < lines.len() && is_table_row(lines[end]) {
            end += 1;
        }
        let kind = cells(line)
            .first()
            .map(|c| c.to_lowercase())
            .unwrap_or_default();
        let into = match kind.as_str() {
            "new" | "feature" => Some(&mut parts.added),
            "fix" | "fixed" => Some(&mut parts.fixed),
            _ => None,
        };
        match into {
            Some(into) => {
                for row in &lines[i + 2..end] {
                    let mut c = cells(row).into_iter();
                    let name = c.next().unwrap_or_default();
                    let rest: Vec<String> = c.collect();
                    if !name.is_empty() || !rest.is_empty() {
                        into.push(NoteItem {
                            name,
                            text: rest.join(" | "),
                        });
                    }
                }
            }
            None => intro.extend_from_slice(&lines[i..end]),
        }
        i = end;
    }
    parts.intro = Regex::new(r"\n{3,}")
        .unwrap()
        .replace_all(&intro.join("\n"), "\n\n")
        .trim()
        .to_string();
    parts
}

/// Version order, newest last; versions that don't parse are equal.
pub fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => compare_versions(&x, &y),
        _ => std::cmp::Ordering::Equal,
    }
}

/// A version's release page on GitHub.
pub fn release_page_url(version: &str) -> String {
    crate::update::release_page_url(&format!("v{}", version.trim_start_matches('v')))
}

/// Newest first, one per version (the first one seen wins), capped.
fn merge(lists: &[&[ReleaseNote]], max: usize) -> Vec<ReleaseNote> {
    let mut out: Vec<ReleaseNote> = Vec::new();
    for n in lists.iter().flat_map(|l| l.iter()) {
        if !out.iter().any(|o| o.version == n.version) {
            out.push(n.clone());
        }
    }
    out.sort_by(|a, b| cmp_versions(&b.version, &a.version));
    out.truncate(max);
    out
}

/// What About shows: the installed version's notes and earlier ones.
#[derive(Clone, Debug, PartialEq)]
pub struct NotesView {
    pub installed: Option<ReleaseNote>,
    pub earlier: Vec<ReleaseNote>,
}

#[derive(Default)]
struct Saved {
    last_run: Option<String>,
    saved: Vec<ReleaseNote>,
}

fn read(file: &Path) -> Saved {
    let Some(raw) = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return Saved::default();
    };
    let note = |v: &Value| {
        let s = |k: &str| v.get(k)?.as_str().map(String::from);
        let version = s("version").filter(|v| parse_version(v).is_some())?;
        let date = match v.get("date")? {
            Value::Null => None,
            Value::String(d) => Some(d.clone()),
            _ => return None,
        };
        Some(ReleaseNote {
            version,
            title: s("title")?,
            date,
            notes: s("notes")?,
        })
    };
    Saved {
        last_run: raw.get("lastRun").and_then(Value::as_str).map(String::from),
        saved: raw
            .get("saved")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(note).collect())
            .unwrap_or_default(),
    }
}

fn write(file: &Path, data: &Saved) {
    let saved: Vec<Value> = data
        .saved
        .iter()
        .map(
            |n| json!({ "version": n.version, "title": n.title, "date": n.date, "notes": n.notes }),
        )
        .collect();
    let text = json!({ "lastRun": data.last_run, "saved": saved }).to_string() + "\n";
    // Worst case the notes come from the bundle or the next check.
    let _ = std::fs::write(file, text);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
    }
}

/// Records this run; returns the older version that ran last, when the app was updated.
pub fn start(file: &Path, current: &str) -> Option<String> {
    let mut data = read(file);
    let last = data.last_run.clone();
    let updated_from = last.clone().filter(|l| cmp_versions(l, current).is_lt());
    if last.as_deref() != Some(current) {
        data.last_run = Some(current.into());
        write(file, &data);
    }
    updated_from
}

/// Keeps a release's notes (from an update check) for after the update.
pub fn remember(file: &Path, r: &ReleaseInfo) {
    if parse_version(&r.version).is_none() || r.notes.trim().is_empty() {
        return;
    }
    let note = ReleaseNote {
        version: r.version.clone(),
        title: String::new(),
        date: r.published_at.clone(),
        notes: r.notes.chars().take(MAX_NOTE_CHARS).collect(),
    };
    let mut data = read(file);
    if data
        .saved
        .iter()
        .any(|n| n.version == note.version && n.notes == note.notes && n.date == note.date)
    {
        return;
    }
    // Keep the newest few, whatever is installed: the update may be several versions ahead.
    data.saved = merge(&[&[note], &data.saved], MAX_EARLIER + 2);
    write(file, &data);
}

/// The installed version's notes first (bundled beats saved), then earlier versions.
pub fn view(file: &Path, current: &str, bundled: &[ReleaseNote]) -> NotesView {
    let saved = read(file).saved;
    let all: Vec<ReleaseNote> = merge(&[bundled, &saved], usize::MAX)
        .into_iter()
        .filter(|n| cmp_versions(&n.version, current).is_le())
        .collect();
    let installed = all.iter().find(|n| n.version == current).cloned();
    let earlier = all
        .into_iter()
        .filter(|n| n.version != current)
        .take(MAX_EARLIER)
        .collect();
    NotesView { installed, earlier }
}

// ---- New-release notice (`update-notify.ts`) ------------------------------------------------

/// Title and body of the OS notice for a newer release, once per version: `state_file`
/// (userData/update-notified.json) remembers the last one announced.
pub fn notice_once(
    state_file: &Path,
    current: &str,
    latest: &ReleaseInfo,
) -> Option<(String, String)> {
    let prefix = format!(
        "https://github.com/{}/releases/",
        crate::update::RELEASES_REPO
    );
    if !latest.url.starts_with(&prefix) {
        return None;
    }
    let last = std::fs::read_to_string(state_file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("lastNotified")?.as_str().map(String::from));
    if last.as_deref() == Some(latest.version.as_str()) {
        return None;
    }
    // Worst case the same release is announced again next launch.
    let _ = std::fs::write(
        state_file,
        json!({ "lastNotified": latest.version }).to_string() + "\n",
    );
    Some((
        format!("Wraithgrid {} is available", latest.version),
        format!("You have {current}. Click to update from Settings."),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "# What's new in 1.2.0: big one\n\n*v1.2.0 · 2026-11-05*\n\nIntro **bold**.\n\n| New | What changed |\n|---|---|\n| Thing | It `a|b` works |\n\n| Fix | What changed |\n|---|---|\n| Bug | Gone |\n\n---\n\n## Previously, in 1.1.0: small\n\n*v1.1.0 · 2026-10-01*\n\n| Other | x |\n|---|---|\n| a | b |\n";

    #[test]
    fn whats_new_and_parts() {
        let notes = parse_whats_new(MD);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].version, "1.2.0");
        assert_eq!(notes[0].title, "big one");
        assert_eq!(notes[0].date.as_deref(), Some("2026-11-05"));
        assert_eq!(notes[1].title, "small");
        assert!(!notes[0].notes.lines().any(|l| l.trim() == "---"));
        let p = note_parts(&notes[0].notes);
        assert_eq!(p.title, None);
        assert_eq!(p.intro, "Intro **bold**.");
        assert_eq!(
            p.added,
            vec![NoteItem {
                name: "Thing".into(),
                text: "It `a|b` works".into()
            }]
        );
        assert_eq!(p.fixed[0].name, "Bug");
        // Other tables stay in the intro.
        assert!(note_parts(&notes[1].notes).intro.contains("| a | b |"));
        assert_eq!(
            note_parts("## What's new in 2.0: hello\n\ntext")
                .title
                .as_deref(),
            Some("hello")
        );
        // The real file parses.
        let real = parse_whats_new(include_str!("../../../../docs/WHATS-NEW.md"));
        assert!(real.len() >= 5 && real.iter().all(|n| parse_version(&n.version).is_some()));
    }

    #[test]
    fn store_keeps_notes_and_last_run() {
        let dir = std::env::temp_dir().join(format!("wg-notes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("release-notes.json");
        assert_eq!(start(&file, "1.1.0"), None);
        assert_eq!(start(&file, "1.2.0").as_deref(), Some("1.1.0"));
        assert_eq!(start(&file, "1.2.0"), None);
        let release = |v: &str| ReleaseInfo {
            version: v.into(),
            tag: format!("v{v}"),
            url: String::new(),
            notes: format!("notes {v}"),
            published_at: None,
            assets: vec![],
        };
        remember(&file, &release("1.3.0"));
        remember(&file, &release("1.0.5"));
        let bundled = parse_whats_new(MD);
        let v = view(&file, "1.2.0", &bundled);
        assert_eq!(v.installed.unwrap().title, "big one");
        // Newer than installed is left out; earlier newest first.
        let earlier: Vec<_> = v.earlier.iter().map(|n| n.version.as_str()).collect();
        assert_eq!(earlier, ["1.1.0", "1.0.5"]);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(raw["lastRun"], "1.2.0");
        assert_eq!(raw["saved"][0]["version"], "1.3.0");
        assert_eq!(raw["saved"][0]["date"], Value::Null);

        let state = dir.join("update-notified.json");
        let mut r = release("1.3.0");
        r.url = crate::update::release_page_url("v1.3.0");
        assert!(notice_once(&state, "1.2.0", &r).is_some());
        assert!(notice_once(&state, "1.2.0", &r).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
