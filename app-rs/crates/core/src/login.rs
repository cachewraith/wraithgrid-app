//! Sign-in from a dialog (`src/main/login.ts`): the CLI's own login runs over pipes in the
//! account's config dir (claude `auth login`, codex `login --device-auth`); the CLI does the
//! OAuth and keeps the tokens. This reads its output for the sign-in link (and codex's
//! one-time code), and its status command's answer at the end.

use crate::agent_cli::AgentCli;

/// Who is signed in, as the status command says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub email: Option<String>,
    pub plan: Option<String>,
}

/// How one CLI signs in over pipes (`LoginFlow`).
pub struct LoginFlow {
    pub login: &'static [&'static str],
    pub status: &'static [&'static str],
    /// Sign-in links are taken only on these hosts (and their subdomains).
    pub hosts: &'static [&'static str],
    /// Device sign-in: it prints a one-time code to type on the page.
    pub device: bool,
    /// The status command answers on stderr (codex).
    pub status_stderr: bool,
}

pub const CLAUDE_HOSTS: [&str; 3] = ["claude.com", "claude.ai", "anthropic.com"];

/// `LOGIN_FLOWS`: the CLIs whose Login opens the dialog.
pub fn login_flow(cli: AgentCli) -> Option<LoginFlow> {
    match cli {
        AgentCli::Claude => Some(LoginFlow {
            login: &["auth", "login"],
            status: &["auth", "status", "--json"],
            hosts: &CLAUDE_HOSTS,
            device: false,
            status_stderr: false,
        }),
        AgentCli::Codex => Some(LoginFlow {
            login: &["login", "--device-auth"],
            status: &["login", "status"],
            hosts: &["openai.com", "chatgpt.com"],
            device: true,
            status_stderr: true,
        }),
        _ => None,
    }
}

impl LoginFlow {
    pub fn device_code(&self, text: &str) -> Option<String> {
        if self.device {
            codex_device_code(text)
        } else {
            None
        }
    }

    pub fn parse_status(&self, text: &str) -> AuthStatus {
        if self.device {
            parse_codex_status(text)
        } else {
            parse_auth_status(text)
        }
    }
}

/// Without terminal colour codes (`ESC [ … letter`).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The sign-in link in the output, only if it is an https page on one of `hosts`.
pub fn login_url(text: &str, hosts: &[&str]) -> Option<String> {
    let text = strip_ansi(text);
    let mut rest = text.as_str();
    while let Some(at) = rest.find("https://") {
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>'))
            .unwrap_or(tail.len());
        let url = &tail[..end];
        let host = url["https://".len()..]
            .split(['/', '?', '#', ':'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if !host.is_empty()
            && hosts
                .iter()
                .any(|h| host == *h || host.ends_with(&format!(".{h}")))
        {
            return Some(url.to_string());
        }
        rest = &tail[end..];
    }
    None
}

/// `claude auth status --json`: signed in, and as whom.
pub fn parse_auth_status(text: &str) -> AuthStatus {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return AuthStatus::default();
    };
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .filter(|x| !x.is_empty())
            .map(String::from)
    };
    AuthStatus {
        logged_in: v.get("loggedIn").and_then(|x| x.as_bool()) == Some(true),
        email: s("email"),
        plan: s("subscriptionType"),
    }
}

/// `codex login status` (on stderr): "Logged in using ChatGPT" / "Not logged in".
pub fn parse_codex_status(text: &str) -> AuthStatus {
    let clean = strip_ansi(text);
    if clean.to_lowercase().contains("not logged in") {
        return AuthStatus::default();
    }
    let Some(at) = clean.find("Logged in using ") else {
        return AuthStatus::default();
    };
    let how = clean[at + "Logged in using ".len()..]
        .lines()
        .next()
        .unwrap_or("")
        .trim();
    AuthStatus {
        logged_in: true,
        email: None,
        plan: Some(if how == "an API key" {
            "API key".into()
        } else {
            how.to_string()
        }),
    }
}

/// The one-time code `codex login --device-auth` prints on the line after "one-time code".
pub fn codex_device_code(text: &str) -> Option<String> {
    let clean = strip_ansi(text);
    let at = clean.find("one-time code")?;
    let next = clean[at..].split_once('\n')?.1;
    let word = next.split_whitespace().next()?;
    let ok = |part: &str| {
        (4..=8).contains(&part.len())
            && part
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    match word.split_once('-') {
        Some((a, b)) if ok(a) && ok(b) => Some(word.to_string()),
        _ => None,
    }
}

/// claude's last output line, without its code prompt: what a failed sign-in shows.
pub fn last_line(out: &str) -> String {
    let last = out.trim().lines().last().unwrap_or("");
    match last.find("prompted >") {
        Some(at) => last[at + "prompted >".len()..].trim().to_string(),
        None => last.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sign_in_links_on_the_hosts_only() {
        let out = "Visit \x1b[1mhttps://claude.ai/oauth/authorize?x=1\x1b[0m to sign in";
        assert_eq!(
            login_url(out, &CLAUDE_HOSTS).as_deref(),
            Some("https://claude.ai/oauth/authorize?x=1")
        );
        assert_eq!(
            login_url("go to https://console.anthropic.com/x now", &CLAUDE_HOSTS).as_deref(),
            Some("https://console.anthropic.com/x")
        );
        assert_eq!(login_url("https://evil.com/claude.ai", &CLAUDE_HOSTS), None);
        assert_eq!(login_url("http://claude.ai/x", &CLAUDE_HOSTS), None);
        assert_eq!(login_url("https://notclaude.ai/x", &CLAUDE_HOSTS), None);
    }

    #[test]
    fn reads_codex_device_codes_and_status() {
        let out = "2. Enter this one-time code \x1b[90m(expires in 15 minutes)\x1b[0m\n   \x1b[94mFAKE-C0DE1\x1b[0m\n";
        assert_eq!(codex_device_code(out).as_deref(), Some("FAKE-C0DE1"));
        assert_eq!(codex_device_code("no code here"), None);
        assert_eq!(
            parse_codex_status("Logged in using ChatGPT\n"),
            AuthStatus {
                logged_in: true,
                email: None,
                plan: Some("ChatGPT".into())
            }
        );
        assert_eq!(
            parse_codex_status("Logged in using an API key")
                .plan
                .as_deref(),
            Some("API key")
        );
        assert!(!parse_codex_status("Not logged in").logged_in);
    }

    #[test]
    fn reads_claude_status() {
        let s = parse_auth_status(r#"{"loggedIn":true,"email":"a@b.c","subscriptionType":"max"}"#);
        assert!(s.logged_in);
        assert_eq!(s.email.as_deref(), Some("a@b.c"));
        assert_eq!(s.plan.as_deref(), Some("max"));
        assert!(!parse_auth_status("not json").logged_in);
        assert_eq!(
            last_line("one\nPaste code here if prompted > Bad code"),
            "Bad code"
        );
    }
}
