//! Switching account at the limit (#61): after a turn, a chat whose account is at (or past) its
//! 5-hour or weekly limit carries on as the next account of the same CLI that still has room.
//! The conversation is copied as with "Continue as…", so the next account has all of it.
//! Pure: the app decides when to ask and does the switch. Port of
//! `src/renderer/lib/auto-switch.ts`.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::model::{live_window, AccountLimits};
use crate::UsageAccount;

/// The CLIs whose conversation can be carried over to another account (`CONTINUE_AS_CLIS`).
pub const CONTINUE_AS_CLIS: [&str; 2] = ["claude", "codex"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitWindow {
    FiveHour,
    Weekly,
}

impl LimitWindow {
    /// As the UI says it: "5-hour" or "weekly".
    pub fn label(self) -> &'static str {
        match self {
            Self::FiveHour => "5-hour",
            Self::Weekly => "weekly",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeakUse {
    pub pct: i64,
    pub window: LimitWindow,
}

/// `Math.round`: halves go up, also below zero.
fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// The fullest of an account's windows right now (a window past its reset counts 0).
pub fn peak_use(limits: Option<&AccountLimits>, now: i64) -> Option<PeakUse> {
    let limits = limits?;
    let five = live_window(limits.five_hour, now);
    let week = live_window(limits.seven_day, now);
    if five.is_none() && week.is_none() {
        return None;
    }
    let f = five.map_or(-1.0, |w| w.utilization);
    let w = week.map_or(-1.0, |w| w.utilization);
    Some(if f >= w {
        PeakUse {
            pct: js_round(f * 100.0) as i64,
            window: LimitWindow::FiveHour,
        }
    } else {
        PeakUse {
            pct: js_round(w * 100.0) as i64,
            window: LimitWindow::Weekly,
        }
    })
}

static LIMIT_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)usage limit|hit your (?:usage |session |weekly )?limit|limit reached|usageLimitExceeded|out of (?:credits|quota)",
    )
    .expect("valid regex")
});

/// A turn that failed because the account is out of quota. claude: "Claude AI usage limit
/// reached|…", "You've hit your limit · resets …", "You've hit your session limit · resets …";
/// codex: "You've hit your usage limit…", `usageLimitExceeded`. Rate limits of the API itself
/// (429 "slow down") are not this.
pub fn is_limit_error(error: Option<&str>) -> bool {
    error.is_some_and(|e| !e.is_empty() && LIMIT_ERROR.is_match(e))
}

fn can_continue_as(cli: &str) -> bool {
    CONTINUE_AS_CLIS.contains(&cli)
}

/// The account to carry on as: the first account after `from` in the list (wrapping) of the
/// same CLI that is signed in and below `threshold` (percent). Accounts with no known limits
/// yet count as having room.
pub fn next_account<'a>(
    accounts: &'a [UsageAccount],
    from: &UsageAccount,
    limits: &BTreeMap<String, AccountLimits>,
    now: i64,
    threshold: i64,
) -> Option<&'a UsageAccount> {
    if !can_continue_as(&from.cli) {
        return None;
    }
    let (after, before) = match accounts.iter().position(|a| a.id == from.id) {
        Some(at) => (&accounts[at + 1..], &accounts[..at]),
        None => (accounts, &accounts[..0]),
    };
    after.iter().chain(before).find(|a| {
        a.id != from.id
            && a.cli == from.cli
            && a.signed_in
            && peak_use(limits.get(&a.id), now).map_or(0, |p| p.pct) < threshold
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum SwitchDecision<'a> {
    Stay,
    /// `resend`: the turn failed on the limit, so its message goes again as `to`.
    Switch {
        to: &'a UsageAccount,
        reason: String,
        resend: bool,
    },
    NoneLeft {
        reason: String,
    },
}

/// What to do after a turn of `from` ended (with `error`, or None).
pub fn decide_switch<'a>(
    accounts: &'a [UsageAccount],
    from: &UsageAccount,
    limits: &BTreeMap<String, AccountLimits>,
    now: i64,
    threshold: i64,
    error: Option<&str>,
) -> SwitchDecision<'a> {
    if !can_continue_as(&from.cli) {
        return SwitchDecision::Stay;
    }
    let hit_error = is_limit_error(error);
    let peak = peak_use(limits.get(&from.id), now);
    let reason = match (hit_error, peak) {
        (true, _) => format!("{} hit its usage limit", from.name),
        (false, Some(p)) if p.pct >= threshold => format!(
            "{} reached {}% of its {} limit",
            from.name,
            p.pct,
            p.window.label()
        ),
        _ => return SwitchDecision::Stay,
    };
    match next_account(accounts, from, limits, now, threshold) {
        Some(to) => SwitchDecision::Switch {
            to,
            reason,
            resend: hit_error,
        },
        None => SwitchDecision::NoneLeft { reason },
    }
}
