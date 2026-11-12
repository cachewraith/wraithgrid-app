//! Live plan limits for claude accounts: every few minutes, each signed-in account's 5-hour and
//! weekly use from Anthropic, the same answer claude's own /usage shows. Without this an
//! account's numbers only moved after one of its chat turns. The account's OAuth token is read
//! from its own config dir here, sent only to api.anthropic.com, and never logged or handed to
//! the UI. An expired token is skipped (the CLI refreshes it on its next run); macOS keeps it
//! in the Keychain, so nothing is polled there. Port of `src/main/usage-poll.ts`.
//!
//! No timer or thread of its own: the app calls `poll_once` every `POLL_MS`, off the UI thread
//! (the fetch blocks), and stores what it returns.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone as _};
use serde_json::Value;

use crate::model::{AccountLimits, UsageWindow};
use crate::{now_ms, UsageAccount};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const POLL_MS: i64 = 120_000;
/// After a refusal (401/403) or a rate limit (429), leave that account alone this long.
pub const BACKOFF_MS: i64 = 10 * 60_000;
/// A request still unanswered after this is given up until the next round.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// An account's OAuth access token. Its Debug output never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthToken(String);

impl OAuthToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OAuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthToken(<redacted>)")
    }
}

/// One GET. Not Debug: its headers carry the token.
pub struct FetchRequest<'a> {
    pub url: &'a str,
    pub headers: &'a [(&'a str, String)],
    pub timeout: Duration,
}

pub struct FetchResponse {
    pub status: u16,
    pub body: String,
}

pub type ReadFileFn = Box<dyn Fn(&Path) -> io::Result<String> + Send + Sync>;
pub type FetchFn = Box<dyn Fn(&FetchRequest<'_>) -> anyhow::Result<FetchResponse> + Send + Sync>;
pub type ClockFn = Box<dyn Fn() -> i64 + Send + Sync>;

pub struct UsagePollDeps {
    pub read_file: ReadFileFn,
    pub fetch: FetchFn,
    pub now: ClockFn,
}

impl UsagePollDeps {
    /// The real file system, network (ureq) and clock.
    pub fn live() -> Self {
        Self {
            read_file: Box::new(|p| std::fs::read_to_string(p)),
            fetch: Box::new(ureq_fetch),
            now: Box::new(now_ms),
        }
    }
}

/// A blocking GET with ureq. Any status comes back as a response; only no answer at all
/// (offline, timeout, TLS) is an error.
pub fn ureq_fetch(req: &FetchRequest<'_>) -> anyhow::Result<FetchResponse> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(req.timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let mut r = agent.get(req.url);
    for (k, v) in req.headers {
        r = r.header(*k, v.as_str());
    }
    let mut res = r.call()?;
    let status = res.status().as_u16();
    let body = res.body_mut().read_to_string()?;
    Ok(FetchResponse { status, body })
}

/// The account's access token, or None when it has none or it has expired.
pub fn read_claude_token(
    config_dir: &Path,
    read_file: &dyn Fn(&Path) -> io::Result<String>,
    now: i64,
) -> Option<OAuthToken> {
    let text = read_file(&config_dir.join(".credentials.json")).ok()?;
    let raw: Value = serde_json::from_str(&text).ok()?;
    let oauth = raw.as_object()?.get("claudeAiOauth")?.as_object()?;
    let token = oauth
        .get("accessToken")?
        .as_str()
        .filter(|t| !t.is_empty())?;
    if let Some(expires_at) = oauth.get("expiresAt").and_then(Value::as_f64) {
        if expires_at <= now as f64 {
            return None;
        }
    }
    Some(OAuthToken(token.to_owned()))
}

/// `Date.parse` for the forms an ISO time comes in: with an offset, local without one, and a
/// bare date as UTC midnight.
fn parse_time_ms(s: &str) -> Option<i64> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.timestamp_millis());
    }
    if let Ok(t) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return chrono::Local
            .from_local_datetime(&t)
            .earliest()
            .map(|t| t.timestamp_millis());
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    Some(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis())
}

fn window_of(v: Option<&Value>) -> Option<UsageWindow> {
    let o = v?.as_object()?;
    let utilization = o.get("utilization")?.as_f64()?;
    let resets_at = parse_time_ms(o.get("resets_at")?.as_str()?)?;
    // Percent here; 0–1 everywhere else (claude's `rate_limit_event`).
    Some(UsageWindow {
        utilization: utilization.max(0.0) / 100.0,
        resets_at,
    })
}

/// `/api/oauth/usage`'s answer as limits; None when it has neither window.
pub fn parse_oauth_usage(body: &Value, now: i64) -> Option<AccountLimits> {
    let o = body.as_object()?;
    let five_hour = window_of(o.get("five_hour"));
    let seven_day = window_of(o.get("seven_day"));
    (five_hour.is_some() || seven_day.is_some()).then_some(AccountLimits {
        five_hour,
        seven_day,
        at: now,
    })
}

pub struct UsagePoller {
    deps: UsagePollDeps,
    quiet_until: HashMap<String, i64>,
}

impl UsagePoller {
    pub fn new(deps: UsagePollDeps) -> Self {
        Self {
            deps,
            quiet_until: HashMap::new(),
        }
    }

    /// One round over the signed-in claude accounts, one at a time. Returns each account's
    /// new limits, in list order, for the caller to store.
    pub fn poll_once(&mut self, accounts: &[UsageAccount]) -> Vec<(String, AccountLimits)> {
        let mut got = Vec::new();
        for a in accounts {
            if a.cli != "claude" || !a.signed_in {
                continue;
            }
            if self.quiet_until.get(&a.id).copied().unwrap_or(0) > (self.deps.now)() {
                continue;
            }
            if let Some(limits) = self.poll_one(a) {
                got.push((a.id.clone(), limits));
            }
        }
        got
    }

    fn poll_one(&mut self, a: &UsageAccount) -> Option<AccountLimits> {
        let token = read_claude_token(&a.config_dir, &*self.deps.read_file, (self.deps.now)())?;
        let headers = [
            ("Authorization", format!("Bearer {}", token.expose())),
            ("anthropic-beta", "oauth-2025-04-20".to_owned()),
            ("Content-Type", "application/json".to_owned()),
        ];
        let req = FetchRequest {
            url: USAGE_URL,
            headers: &headers,
            timeout: FETCH_TIMEOUT,
        };
        // Offline or timed out: the next round tries again.
        let res = (self.deps.fetch)(&req).ok()?;
        if matches!(res.status, 401 | 403 | 429) {
            self.quiet_until
                .insert(a.id.clone(), (self.deps.now)() + BACKOFF_MS);
            return None;
        }
        if res.status != 200 {
            return None;
        }
        let body: Value = serde_json::from_str(&res.body).ok()?;
        parse_oauth_usage(&body, (self.deps.now)())
    }
}
