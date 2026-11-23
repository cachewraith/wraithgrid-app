//! `wraithgrid://` links: the website's "Open in Wraithgrid" (`wraithgrid://clone?repo=…`).
//! A link arrives in argv (first launch on Linux/Windows), from a second launch over the
//! single-instance socket (window.rs), or as macOS's open-URL event (main.rs). It is kept
//! until the renderer takes it (`app:takeLink`, after `app:link` or at start), and parsed
//! there: a link only ever opens a dialog the user confirms, never runs anything itself.

use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

pub const SCHEME: &str = "wraithgrid://";
const LINK_MAX: usize = 8192;

#[derive(Default)]
pub struct Pending(Mutex<Option<String>>);

/// A link worth keeping: our scheme, bounded, one line (the socket protocol is line-based).
pub fn valid(link: &str) -> bool {
    link.len() <= LINK_MAX
        && link
            .get(..SCHEME.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(SCHEME))
        && !link.chars().any(char::is_control)
}

/// The link this process was started with, if any.
pub fn from_args(args: impl IntoIterator<Item = String>) -> Option<String> {
    args.into_iter().skip(1).find(|a| valid(a))
}

pub fn setup(app: &AppHandle) {
    app.manage(Pending::default());
    if let Some(link) = from_args(std::env::args()) {
        keep(app, link);
    }
}

fn keep(app: &AppHandle, link: String) {
    *app.state::<Pending>().0.lock().unwrap() = Some(link);
}

/// A link for the running app: kept, the renderer told, the window shown.
pub fn deliver(app: &AppHandle, link: &str) {
    if !valid(link) {
        return;
    }
    keep(app, link.to_string());
    super::emit(app, "app:link", Value::Null);
    super::window::reopen_window(app);
}

pub async fn handle(app: &AppHandle, channel: &str) -> Option<Result<Value, String>> {
    match channel {
        "app:takeLink" => {
            let link = app.state::<Pending>().0.lock().unwrap().take();
            Some(Ok(json!(link)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_bounded_one_line_links_of_our_scheme() {
        assert!(valid(
            "wraithgrid://clone?repo=https%3A%2F%2Fgithub.com%2Fo%2Fr"
        ));
        assert!(valid("WraithGrid://clone"));
        assert!(!valid("https://example.com"));
        assert!(!valid("wraithgrid:/x"));
        assert!(!valid("wraithgrid://clone\nopen"));
        assert!(!valid(&format!("wraithgrid://{}", "a".repeat(LINK_MAX))));
    }

    #[test]
    fn finds_the_link_among_the_arguments_but_never_the_program() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            from_args(args(&["wraithgrid", "--x", "wraithgrid://clone?repo=a"])),
            Some("wraithgrid://clone?repo=a".into())
        );
        assert_eq!(from_args(args(&["wraithgrid://clone"])), None);
        assert_eq!(from_args(args(&["wraithgrid", "/home/me"])), None);
    }
}
