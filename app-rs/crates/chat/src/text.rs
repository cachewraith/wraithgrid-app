//! String helpers the menus share (`slash.rs`, `files.rs`). The TS sorted with JS
//! `localeCompare`, so a list reads the way people expect (`package.json` before `README.md`);
//! [`locale_cmp`] approximates that ordering (ICU's root collation) without an ICU dependency.

use std::cmp::Ordering;

/// Punctuation and symbols in ICU root order: they sort before digits, digits before letters.
const PUNCT: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

/// A character's primary weight: its class, then its place within it (letters case-blind).
fn primary(c: char) -> (u8, u32) {
    if c.is_whitespace() {
        (0, c as u32)
    } else if let Some(i) = PUNCT.find(c) {
        (1, i as u32)
    } else if c.is_ascii_digit() {
        (2, c as u32)
    } else if c.is_alphabetic() {
        (3, c.to_lowercase().next().unwrap_or(c) as u32)
    } else {
        (1, 100 + c as u32)
    }
}

/// JS `a.localeCompare(b)`, near enough: case-blind first, then lower case before upper.
pub fn locale_cmp(a: &str, b: &str) -> Ordering {
    let primary_order = a.chars().map(primary).cmp(b.chars().map(primary));
    if primary_order != Ordering::Equal {
        return primary_order;
    }
    for (x, y) in a.chars().zip(b.chars()) {
        if x != y {
            // ICU's tertiary level puts the lower-case form first.
            return match (x.is_lowercase(), y.is_lowercase()) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => x.cmp(&y),
            };
        }
    }
    a.len().cmp(&b.len())
}

/// The letters of `q` appear in `s` in order (each char matched once).
pub(crate) fn subsequence(q: &str, s: &str) -> bool {
    let q: Vec<char> = q.chars().collect();
    let mut i = 0;
    for ch in s.chars() {
        if i < q.len() && ch == q[i] {
            i += 1;
        }
    }
    i == q.len()
}

/// The largest char boundary at or below `i` (a caret from the UI must never split a char).
pub(crate) fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_like_locale_compare() {
        let mut v = vec![
            "README.md",
            "package.json",
            "src/",
            "Tests",
            "tests",
            "a_b",
            "a-b",
        ];
        v.sort_by(|a, b| locale_cmp(a, b));
        assert_eq!(
            v,
            [
                "a_b",
                "a-b",
                "package.json",
                "README.md",
                "src/",
                "tests",
                "Tests"
            ]
        );
        assert!(subsequence("apmk", "api-mocking"));
        assert!(!subsequence("zz", "api"));
        assert_eq!(floor_boundary("aé", 2), 1);
    }
}
