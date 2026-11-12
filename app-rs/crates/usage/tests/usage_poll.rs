//! The live claude limits poll, ported from tests/unit/usage-poll.test.ts. The fetch and the
//! files are fakes: nothing here touches the network.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::DateTime;
use serde_json::{json, Value};
use wraithgrid_usage::*;

fn now() -> i64 {
    ms("2026-11-02T03:00:00Z")
}

fn ms(iso: &str) -> i64 {
    DateTime::parse_from_rfc3339(iso)
        .unwrap()
        .timestamp_millis()
}

/// The answer's shape as Anthropic sent it on 2026-11-02 (trimmed): utilization in percent.
fn answer() -> Value {
    json!({
        "five_hour": { "utilization": 73.0, "resets_at": "2026-11-02T06:19:59.643548+00:00" },
        "seven_day": { "utilization": 9.0, "resets_at": "2026-11-08T03:59:59.643565+00:00" },
        "seven_day_opus": null,
        "extra_usage": { "is_enabled": false }
    })
}

fn creds(token: &str, expires_at: i64) -> String {
    json!({ "claudeAiOauth": { "accessToken": token, "refreshToken": "r", "expiresAt": expires_at } })
        .to_string()
}

fn account(id: &str) -> UsageAccount {
    UsageAccount {
        id: id.into(),
        name: id.into(),
        cli: "claude".into(),
        signed_in: true,
        config_dir: Path::new("/home").join(id),
    }
}

fn file(id: &str) -> PathBuf {
    Path::new("/home").join(id).join(".credentials.json")
}

type Answer = Box<dyn Fn(&str) -> (u16, String) + Send + Sync>;

struct Fake {
    deps: UsagePollDeps,
    calls: Arc<Mutex<Vec<(String, String)>>>,
    clock: Arc<Mutex<i64>>,
}

fn fake(files: HashMap<PathBuf, String>, answer: Answer) -> Fake {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(Mutex::new(now()));
    let (c, k) = (calls.clone(), clock.clone());
    let deps = UsagePollDeps {
        read_file: Box::new(move |p| {
            files
                .get(p)
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }),
        fetch: Box::new(move |req| {
            let auth = req
                .headers
                .iter()
                .find(|(k, _)| *k == "Authorization")
                .unwrap()
                .1
                .clone();
            c.lock().unwrap().push((req.url.to_owned(), auth.clone()));
            let (status, body) = answer(auth.strip_prefix("Bearer ").unwrap());
            Ok(FetchResponse { status, body })
        }),
        now: Box::new(move || *k.lock().unwrap()),
    };
    Fake { deps, calls, clock }
}

#[test]
fn turns_the_answer_into_limits() {
    assert_eq!(
        parse_oauth_usage(&answer(), now()),
        Some(AccountLimits {
            five_hour: Some(UsageWindow {
                utilization: 0.73,
                resets_at: ms("2026-11-02T06:19:59.643Z")
            }),
            seven_day: Some(UsageWindow {
                utilization: 0.09,
                resets_at: ms("2026-11-08T03:59:59.643Z")
            }),
            at: now(),
        })
    );
    assert_eq!(
        parse_oauth_usage(&json!({ "five_hour": null, "seven_day": null }), now()),
        None
    );
    assert_eq!(parse_oauth_usage(&json!("nope"), now()), None);
}

#[test]
fn reads_the_token_none_when_missing_broken_or_expired() {
    let read = |s: String| move |_: &Path| Ok::<_, io::Error>(s.clone());
    let dir = Path::new("/d");
    let tok = read_claude_token(dir, &read(creds("tok", now() + 3_600_000)), now());
    assert_eq!(tok.as_ref().map(OAuthToken::expose), Some("tok"));
    assert_eq!(format!("{tok:?}"), "Some(OAuthToken(<redacted>))");
    assert_eq!(
        read_claude_token(dir, &read(creds("tok", now() - 1)), now()),
        None
    );
    assert_eq!(read_claude_token(dir, &read("{".into()), now()), None);
    let fail = |_: &Path| Err::<String, _>(io::Error::other("x"));
    assert_eq!(read_claude_token(dir, &fail, now()), None);
}

#[test]
fn asks_for_each_signed_in_claude_account_with_a_token_and_reports_its_limits() {
    let files = HashMap::from([
        (file("work"), creds("tok-work", now() + 3_600_000)),
        (file("codex"), creds("tok-codex", now() + 3_600_000)),
    ]);
    let f = fake(files, Box::new(|_| (200, answer().to_string())));
    let accounts = [
        account("work"),
        account("nocreds"),
        UsageAccount {
            signed_in: false,
            ..account("out")
        },
        UsageAccount {
            cli: "codex".into(),
            ..account("codex")
        },
    ];
    let got = UsagePoller::new(f.deps).poll_once(&accounts);
    assert_eq!(
        *f.calls.lock().unwrap(),
        [(USAGE_URL.to_owned(), "Bearer tok-work".to_owned())]
    );
    let ids: Vec<_> = got.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["work"]);
}

#[test]
fn leaves_an_account_alone_after_401_or_429_other_errors_retry_next_round() {
    let status = Arc::new(Mutex::new(401));
    let s = status.clone();
    let files = HashMap::from([(file("work"), creds("t", now() + 3_600_000))]);
    let f = fake(
        files,
        Box::new(move |_| (*s.lock().unwrap(), String::new())),
    );
    let (calls, clock) = (f.calls.clone(), f.clock.clone());
    let mut poller = UsagePoller::new(f.deps);
    let accounts = [account("work")];
    poller.poll_once(&accounts);
    poller.poll_once(&accounts);
    assert_eq!(calls.lock().unwrap().len(), 1);
    *clock.lock().unwrap() += BACKOFF_MS;
    *status.lock().unwrap() = 500;
    poller.poll_once(&accounts);
    poller.poll_once(&accounts);
    assert_eq!(calls.lock().unwrap().len(), 3);
}

#[test]
fn survives_a_network_failure() {
    let files = HashMap::from([(file("work"), creds("t", now() + 3_600_000))]);
    let mut f = fake(files, Box::new(|_| (200, String::new())));
    f.deps.fetch = Box::new(|_| Err(anyhow::anyhow!("offline")));
    assert!(UsagePoller::new(f.deps)
        .poll_once(&[account("work")])
        .is_empty());
}

#[test]
fn a_200_that_is_not_json_reports_nothing() {
    let files = HashMap::from([(file("work"), creds("t", now() + 3_600_000))]);
    let f = fake(files, Box::new(|_| (200, "<html>".into())));
    assert!(UsagePoller::new(f.deps)
        .poll_once(&[account("work")])
        .is_empty());
}

#[test]
fn a_window_needs_a_number_and_a_time_and_never_reads_below_zero() {
    let w =
        |v: Value| parse_oauth_usage(&json!({ "five_hour": v }), now()).and_then(|l| l.five_hour);
    // `Z` as well as `+00:00`; a negative percent clamps to 0, as `Math.max(0, …)`.
    assert_eq!(
        w(json!({ "utilization": -5, "resets_at": "2026-11-02T06:00:00Z" })),
        Some(UsageWindow {
            utilization: 0.0,
            resets_at: ms("2026-11-02T06:00:00Z")
        })
    );
    assert_eq!(
        w(json!({ "utilization": "73", "resets_at": "2026-11-02T06:00:00Z" })),
        None
    );
    assert_eq!(w(json!({ "utilization": 73 })), None);
    assert_eq!(w(json!({ "utilization": 73, "resets_at": "soon" })), None);
    // Only the weekly window still makes an answer.
    let week = parse_oauth_usage(
        &json!({ "five_hour": null, "seven_day": { "utilization": 50, "resets_at": "2026-11-08T00:00:00+00:00" } }),
        now(),
    )
    .unwrap();
    assert_eq!(
        (week.five_hour, week.seven_day.map(|w| w.utilization)),
        (None, Some(0.5))
    );
}

#[test]
fn a_token_without_an_expiry_counts_and_an_empty_one_does_not() {
    let read = |s: Value| move |_: &Path| Ok::<_, io::Error>(s.to_string());
    let dir = Path::new("/d");
    let no_expiry = json!({ "claudeAiOauth": { "accessToken": "tok" } });
    assert!(read_claude_token(dir, &read(no_expiry), now()).is_some());
    let empty = json!({ "claudeAiOauth": { "accessToken": "", "expiresAt": now() + 1 } });
    assert_eq!(read_claude_token(dir, &read(empty), now()), None);
    assert_eq!(
        read_claude_token(dir, &read(json!({ "other": 1 })), now()),
        None
    );
}
