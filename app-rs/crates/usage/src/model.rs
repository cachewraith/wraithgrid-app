//! Usage: what chat turns used (from the CLI's own reports), kept per day, account, project
//! and chat in userData/usage.json; and each account's plan limits as the CLI last reported
//! them (claude's `rate_limit_event`, codex's `account/rateLimits`). claude reports a cost;
//! codex and agy only tokens (#53). Terminal panes are not measured: their output is a
//! screen, not data. Pure functions, so the store records and the UI summarizes the same way.
//! Port of `src/shared/usage.ts`.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Days, Local, NaiveDate};
use serde::Serialize;
use serde_json::{Map, Value};

/// One limit window: how much of it is used (0–1) and when it starts over (ms).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub utilization: f64,
    pub resets_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLimits {
    pub five_hour: Option<UsageWindow>,
    pub seven_day: Option<UsageWindow>,
    /// When the CLI said so (ms).
    pub at: i64,
}

/// What one turn used, from the CLI's `result`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TurnUsage {
    pub model: Option<String>,
    /// None: the CLI reports no cost (codex, agy); never guessed.
    pub cost_usd: Option<f64>,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    /// Local day, `YYYY-MM-DD`.
    pub day: String,
    pub account_id: Option<String>,
    pub workspace_id: Option<String>,
    pub pane_id: String,
    /// The chat's title when it last ran.
    pub title: String,
    pub cost_usd: f64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub turns: u64,
    /// Turns whose CLI reported no cost: their cost is unknown, not $0.
    pub unpriced: u64,
}

/// usage.json. Written with the same keys as the Electron app, so either can read the other's.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct UsageData {
    pub buckets: Vec<UsageBucket>,
    pub limits: BTreeMap<String, AccountLimits>,
}

pub const USAGE_DAYS: u64 = 90;

// ---- Reading usage.json: the zod schema's rules, by hand ----
//
// As in TS: a bad field with a fallback (`title`, the counters, a limit window) takes the
// fallback; any other bad bucket empties `buckets`, any bad limits entry empties `limits`; a
// file that is not an object at all is rejected (the store then starts empty).

/// JS string length (UTF-16 units), which zod's `.max()` counts.
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `z.number().finite().min(0).catch(0)`, as a count.
fn count(v: Option<&Value>) -> u64 {
    match v.and_then(Value::as_f64) {
        Some(n) if n.is_finite() && n >= 0.0 => n as u64,
        _ => 0,
    }
}

/// `z.number().finite().min(0).catch(0)`, as money.
fn money(v: Option<&Value>) -> f64 {
    match v.and_then(Value::as_f64) {
        Some(n) if n.is_finite() && n >= 0.0 => n,
        _ => 0.0,
    }
}

/// `z.number().finite()`: any JSON number.
fn finite(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|n| n.is_finite())
}

/// `z.string().max(max).nullable()`: Some(None) for null, None when invalid (or missing).
fn nullable_id(v: Option<&Value>, max: usize) -> Option<Option<String>> {
    match v? {
        Value::Null => Some(None),
        Value::String(s) if js_len(s) <= max => Some(Some(s.clone())),
        _ => None,
    }
}

/// `^\d{4}-\d{2}-\d{2}$`.
fn is_day(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

fn parse_bucket(v: &Value) -> Option<UsageBucket> {
    let o = v.as_object()?;
    let day = o.get("day")?.as_str().filter(|d| is_day(d))?;
    let pane_id = o.get("paneId")?.as_str()?;
    if pane_id.is_empty() || js_len(pane_id) > 64 {
        return None;
    }
    let title = match o.get("title") {
        Some(Value::String(t)) if js_len(t) <= 200 => t.clone(),
        _ => String::new(),
    };
    Some(UsageBucket {
        day: day.to_owned(),
        account_id: nullable_id(o.get("accountId"), 64)?,
        workspace_id: nullable_id(o.get("workspaceId"), 64)?,
        pane_id: pane_id.to_owned(),
        title,
        cost_usd: money(o.get("costUsd")),
        input: count(o.get("input")),
        output: count(o.get("output")),
        cache_read: count(o.get("cacheRead")),
        cache_write: count(o.get("cacheWrite")),
        turns: count(o.get("turns")),
        unpriced: count(o.get("unpriced")),
    })
}

fn parse_buckets(v: Option<&Value>) -> Vec<UsageBucket> {
    let Some(Value::Array(items)) = v else {
        return Vec::new();
    };
    if items.len() > 50_000 {
        return Vec::new();
    }
    items
        .iter()
        .map(parse_bucket)
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

/// A limit window, or None for anything else (`.nullable().catch(null)`).
fn parse_window(v: Option<&Value>) -> Option<UsageWindow> {
    let o = v?.as_object()?;
    Some(UsageWindow {
        utilization: finite(o.get("utilization"))?,
        resets_at: finite(o.get("resetsAt"))? as i64,
    })
}

fn parse_limits(v: Option<&Value>) -> BTreeMap<String, AccountLimits> {
    let Some(Value::Object(m)) = v else {
        return BTreeMap::new();
    };
    let entry = |(k, v): (&String, &Value)| -> Option<(String, AccountLimits)> {
        let o: &Map<String, Value> = v.as_object()?;
        if js_len(k) > 64 {
            return None;
        }
        Some((
            k.clone(),
            AccountLimits {
                five_hour: parse_window(o.get("fiveHour")),
                seven_day: parse_window(o.get("sevenDay")),
                at: finite(o.get("at"))? as i64,
            },
        ))
    };
    m.iter()
        .map(entry)
        .collect::<Option<BTreeMap<_, _>>>()
        .unwrap_or_default()
}

/// usage.json's contents as data (`usageDataSchema.safeParse`): None when it is not an object.
pub fn parse_usage_data(raw: &Value) -> Option<UsageData> {
    let o = raw.as_object()?;
    Some(UsageData {
        buckets: parse_buckets(o.get("buckets")),
        limits: parse_limits(o.get("limits")),
    })
}

// ---- Days ----

fn local_date(ms: i64) -> NaiveDate {
    match DateTime::from_timestamp_millis(ms) {
        Some(t) => t.with_timezone(&Local).date_naive(),
        None if ms < 0 => NaiveDate::MIN,
        None => NaiveDate::MAX,
    }
}

/// Local calendar day of `ms`, `YYYY-MM-DD`.
pub fn day_key(ms: i64) -> String {
    local_date(ms).format("%Y-%m-%d").to_string()
}

/// The local day `n` calendar days before `now`'s. By the calendar, not n × 24 h: across a
/// clock change that skips or repeats a day (#140).
pub fn day_key_before(now: i64, n: u64) -> String {
    let d = local_date(now);
    d.checked_sub_days(Days::new(n))
        .unwrap_or(NaiveDate::MIN)
        .format("%Y-%m-%d")
        .to_string()
}

// ---- Recording ----

/// One finished turn and whose it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub at: i64,
    pub account_id: Option<String>,
    pub workspace_id: Option<String>,
    pub pane_id: String,
    pub title: String,
    pub usage: TurnUsage,
}

/// Adds one turn to its day/account/project/chat bucket.
pub fn add_turn(data: &mut UsageData, t: &Turn) {
    let day = day_key(t.at);
    let u = &t.usage;
    let unpriced = u64::from(u.cost_usd.is_none());
    let found = data.buckets.iter_mut().find(|b| {
        b.day == day
            && b.pane_id == t.pane_id
            && b.account_id == t.account_id
            && b.workspace_id == t.workspace_id
    });
    match found {
        Some(b) => {
            if !t.title.is_empty() {
                b.title.clone_from(&t.title);
            }
            b.cost_usd += u.cost_usd.unwrap_or(0.0);
            b.input += u.input;
            b.output += u.output;
            b.cache_read += u.cache_read;
            b.cache_write += u.cache_write;
            b.turns += 1;
            b.unpriced += unpriced;
        }
        None => data.buckets.push(UsageBucket {
            day,
            account_id: t.account_id.clone(),
            workspace_id: t.workspace_id.clone(),
            pane_id: t.pane_id.clone(),
            title: t.title.clone(),
            cost_usd: u.cost_usd.unwrap_or(0.0),
            input: u.input,
            output: u.output,
            cache_read: u.cache_read,
            cache_write: u.cache_write,
            turns: 1,
            unpriced,
        }),
    }
}

/// Drops days older than USAGE_DAYS.
pub fn prune_usage(data: &mut UsageData, now: i64) {
    let oldest = day_key_before(now, USAGE_DAYS - 1);
    data.buckets.retain(|b| b.day >= oldest);
}

// ---- Summaries ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageRange {
    Today,
    Days7,
    Days30,
}

impl UsageRange {
    /// `RANGE_DAYS`.
    pub fn days(self) -> u64 {
        match self {
            Self::Today => 1,
            Self::Days7 => 7,
            Self::Days30 => 30,
        }
    }
}

/// Some of it has a known cost: else the cost reads "–", not $0.
pub fn has_cost(turns: u64, unpriced: u64) -> bool {
    unpriced < turns
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UsageTotals {
    pub cost_usd: f64,
    /// Everything sent and received, cache included.
    pub tokens: u64,
    pub turns: u64,
    /// Of `turns`, those with no cost reported (codex, agy).
    pub unpriced: u64,
}

impl UsageTotals {
    pub fn has_cost(&self) -> bool {
        has_cost(self.turns, self.unpriced)
    }

    fn add(&mut self, b: &UsageBucket) {
        self.cost_usd += b.cost_usd;
        self.tokens += tokens_of(b);
        self.turns += b.turns;
        self.unpriced += b.unpriced;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DailyCost {
    pub day: String,
    pub cost_usd: f64,
}

/// One account: totals in the range, and cost per day (oldest first).
#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsage {
    pub totals: UsageTotals,
    pub daily: Vec<DailyCost>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectUsage {
    pub workspace_id: Option<String>,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatUsage {
    pub pane_id: String,
    pub title: String,
    pub workspace_id: Option<String>,
    pub cost_usd: f64,
    pub turns: u64,
    pub unpriced: u64,
}

impl ChatUsage {
    pub fn has_cost(&self) -> bool {
        has_cost(self.turns, self.unpriced)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageSummary {
    pub total: UsageTotals,
    /// Keyed by account id; "" for turns with no account.
    pub accounts: BTreeMap<String, AccountUsage>,
    /// Most expensive first.
    pub projects: Vec<ProjectUsage>,
    /// The five most expensive chats.
    pub chats: Vec<ChatUsage>,
}

fn tokens_of(b: &UsageBucket) -> u64 {
    b.input + b.output + b.cache_read + b.cache_write
}

/// Totals for the range ending today. The per-day series always covers at least 7 days, so
/// "Today" still shows a week of trend.
pub fn summarize_usage(data: &UsageData, range: UsageRange, now: i64) -> UsageSummary {
    let days = range.days();
    let series_days = days.max(7);
    let series: Vec<String> = (0..series_days)
        .map(|i| day_key_before(now, series_days - 1 - i))
        .collect();
    let from = &series[(series_days - days) as usize];
    let day_index: HashMap<&str, usize> = series
        .iter()
        .enumerate()
        .map(|(i, d)| (d.as_str(), i))
        .collect();
    let mut total = UsageTotals::default();
    let mut accounts: BTreeMap<String, AccountUsage> = BTreeMap::new();
    // In first-seen order, as JS Maps keep it (the sorts below are stable).
    let mut projects: Vec<ProjectUsage> = Vec::new();
    let mut chats: Vec<ChatUsage> = Vec::new();
    for b in &data.buckets {
        if b.day < series[0] {
            continue;
        }
        let a = accounts
            .entry(b.account_id.clone().unwrap_or_default())
            .or_insert_with(|| AccountUsage {
                totals: UsageTotals::default(),
                daily: series
                    .iter()
                    .map(|day| DailyCost {
                        day: day.clone(),
                        cost_usd: 0.0,
                    })
                    .collect(),
            });
        if let Some(&i) = day_index.get(b.day.as_str()) {
            a.daily[i].cost_usd += b.cost_usd;
        }
        if b.day < *from {
            continue;
        }
        a.totals.add(b);
        total.add(b);
        match projects
            .iter_mut()
            .find(|p| p.workspace_id == b.workspace_id)
        {
            Some(p) => p.totals.add(b),
            None => {
                let mut totals = UsageTotals::default();
                totals.add(b);
                projects.push(ProjectUsage {
                    workspace_id: b.workspace_id.clone(),
                    totals,
                });
            }
        }
        match chats.iter_mut().find(|c| c.pane_id == b.pane_id) {
            Some(c) => {
                if !b.title.is_empty() {
                    c.title.clone_from(&b.title);
                }
                c.cost_usd += b.cost_usd;
                c.turns += b.turns;
                c.unpriced += b.unpriced;
            }
            None => chats.push(ChatUsage {
                pane_id: b.pane_id.clone(),
                title: b.title.clone(),
                workspace_id: b.workspace_id.clone(),
                cost_usd: b.cost_usd,
                turns: b.turns,
                unpriced: b.unpriced,
            }),
        }
    }
    projects.sort_by(|x, y| y.totals.cost_usd.total_cmp(&x.totals.cost_usd));
    chats.sort_by(|x, y| y.cost_usd.total_cmp(&x.cost_usd));
    chats.truncate(5);
    UsageSummary {
        total,
        accounts,
        projects,
        chats,
    }
}

/// A window that has passed its reset time reads as empty: the CLI has not said more yet.
pub fn live_window(w: Option<UsageWindow>, now: i64) -> Option<UsageWindow> {
    let w = w?;
    Some(if w.resets_at <= now {
        UsageWindow {
            utilization: 0.0,
            resets_at: w.resets_at,
        }
    } else {
        w
    })
}
