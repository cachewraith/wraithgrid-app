//! The JavaScript string and Node path semantics the TS sources rely on (UTF-16 lengths, `\s`,
//! `encodeURIComponent`, `path.join`/`path.resolve`, `Date.toISOString`), so the port caps,
//! links and names exactly as the Electron app did.

use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `s.length` in JavaScript: UTF-16 code units.
pub(crate) fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(0, n)`, never splitting a character.
pub(crate) fn js_slice(s: &str, n: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        used += c.len_utf16();
        if used > n {
            break;
        }
        out.push(c);
    }
    out
}

/// A character JavaScript's `\s` (and `trim`) matches.
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `s.trim()`.
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// `(s ?? '').replace(/\s+/g, ' ').trim().slice(0, max)`: a description on one line.
pub(crate) fn one_line(s: Option<&str>, max: usize) -> String {
    let joined = s
        .unwrap_or("")
        .split(is_js_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    js_slice(&joined, max)
}

/// `encodeURIComponent`.
pub(crate) fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `path.normalize`: `.` and `..` resolved lexically, repeated separators collapsed.
pub(crate) fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `path.join(dir, name)`; an empty `name` is the dir itself.
pub(crate) fn join(dir: &Path, name: &str) -> PathBuf {
    if name.is_empty() {
        normalize(dir)
    } else {
        normalize(&dir.join(name))
    }
}

/// `path.resolve(p)`: absolute against the current dir, then normalized.
pub(crate) fn resolve(p: &Path) -> PathBuf {
    if p.is_absolute() {
        return normalize(p);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    normalize(&cwd.join(p))
}

/// A path without its trailing separators (a lone `/` stays), as `trimTrailingSep`.
fn trim_trailing_sep(p: &str) -> &str {
    if p.len() > 1 {
        p.trim_end_matches(['/', '\\'])
    } else {
        p
    }
}

/// `~` and `~/x` become absolute under `home`. Other paths are returned unchanged.
pub(crate) fn expand_home(p: &str, home: &str) -> String {
    if p == "~" {
        return home.to_string();
    }
    if p.starts_with("~/") || p.starts_with("~\\") {
        return format!("{}{}", trim_trailing_sep(home), &p[1..]);
    }
    p.to_string()
}

/// The inverse of expand_home, for messages.
pub(crate) fn contract_home(p: &Path, home: &Path) -> String {
    let p = p.to_string_lossy();
    let home = home.to_string_lossy();
    let h = trim_trailing_sep(&home);
    if h.is_empty() {
        return p.into_owned();
    }
    if p == h {
        return "~".into();
    }
    match p.strip_prefix(h) {
        Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => format!("~{rest}"),
        _ => p.into_owned(),
    }
}

/// `resolveUserPath`: expands `~` and makes the path absolute.
pub(crate) fn resolve_user_path(p: &str, home: &Path) -> PathBuf {
    resolve(Path::new(&expand_home(p, &home.to_string_lossy())))
}

/// `date.toISOString()`: `2026-11-12T09:30:00.000Z`.
pub(crate) fn iso_timestamp(t: SystemTime) -> String {
    let ms = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let (days, rest) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn js_strings() {
        assert_eq!(js_len("a😀"), 3);
        assert_eq!(js_slice("a😀b", 2), "a");
        assert_eq!(
            one_line(Some(" A clock\n above\u{a0}the prompt "), 400),
            "A clock above the prompt"
        );
        assert_eq!(encode_uri_component("a b/é~"), "a%20b%2F%C3%A9~");
    }

    #[test]
    fn iso_time() {
        let t = UNIX_EPOCH + Duration::from_millis(1_794_477_000_123);
        assert_eq!(iso_timestamp(t), "2026-11-12T09:50:00.123Z");
        assert_eq!(iso_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn paths() {
        assert_eq!(
            normalize(Path::new("/a//b/./c/../d")),
            PathBuf::from("/a/b/d")
        );
        assert_eq!(join(Path::new("/a/"), ""), PathBuf::from("/a"));
        assert_eq!(expand_home("~/x", "/home/me/"), "/home/me/x");
        assert_eq!(
            contract_home(Path::new("/home/me/x"), Path::new("/home/me")),
            "~/x"
        );
    }
}
