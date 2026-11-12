//! Switching account at the limit (#61), ported from tests/unit/auto-switch.test.ts.

use std::collections::BTreeMap;

use wraithgrid_usage::*;

const NOW: i64 = 1_000_000_000_000;

fn acct(id: &str, cli: &str, signed_in: bool) -> UsageAccount {
    UsageAccount {
        id: id.into(),
        name: id.into(),
        cli: cli.into(),
        signed_in,
        config_dir: format!("/home/u/.wraithgrid/accounts/{id}").into(),
    }
}

fn claude(id: &str) -> UsageAccount {
    acct(id, "claude", true)
}

fn lim_at(five: Option<f64>, week: Option<f64>, resets_at: i64) -> AccountLimits {
    let w = |u: Option<f64>| {
        u.map(|utilization| UsageWindow {
            utilization,
            resets_at,
        })
    };
    AccountLimits {
        five_hour: w(five),
        seven_day: w(week),
        at: NOW,
    }
}

fn lim(five: f64) -> AccountLimits {
    lim_at(Some(five), None, NOW + 3_600_000)
}

fn limits<const N: usize>(entries: [(&str, AccountLimits); N]) -> BTreeMap<String, AccountLimits> {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
}

#[test]
fn reads_the_fullest_live_window_a_window_past_its_reset_counts_0() {
    let pk = |pct, window| Some(PeakUse { pct, window });
    let later = NOW + 3_600_000;
    assert_eq!(
        peak_use(Some(&lim_at(Some(0.4), Some(0.96), later)), NOW),
        pk(96, LimitWindow::Weekly)
    );
    assert_eq!(
        peak_use(Some(&lim_at(Some(0.97), Some(0.5), later)), NOW),
        pk(97, LimitWindow::FiveHour)
    );
    assert_eq!(
        peak_use(Some(&lim_at(Some(0.99), None, NOW - 1)), NOW),
        pk(0, LimitWindow::FiveHour)
    );
    assert_eq!(peak_use(None, NOW), None);
}

#[test]
fn knows_a_usage_limit_error_from_claude_and_codex_not_other_failures() {
    assert!(is_limit_error(Some(
        "Claude AI usage limit reached|1792771705"
    )));
    assert!(is_limit_error(Some("You've hit your limit · resets 3pm")));
    assert!(is_limit_error(Some(
        "You've hit your session limit · resets 4:40pm (Asia/Phnom_Penh)"
    )));
    assert!(is_limit_error(Some(
        "You've hit your usage limit. Upgrade to Pro"
    )));
    assert!(is_limit_error(Some("usageLimitExceeded")));
    assert!(!is_limit_error(Some(
        "The turn failed (error_during_execution)."
    )));
    assert!(!is_limit_error(None));
}

#[test]
fn picks_the_next_account_of_the_same_cli_wrapping_signed_in_and_with_room() {
    let accounts = [
        claude("a"),
        acct("cx", "codex", true),
        acct("b", "claude", false),
        claude("c"),
        claude("d"),
    ];
    let l = limits([("c", lim(0.96)), ("d", lim(0.2))]);
    let id = |a: Option<&UsageAccount>| a.map(|a| a.id.clone());
    assert_eq!(
        id(next_account(&accounts, &accounts[0], &l, NOW, 95)),
        Some("d".into())
    );
    assert_eq!(
        id(next_account(&accounts, &accounts[4], &l, NOW, 95)),
        Some("a".into())
    );
    let full = limits([("c", lim(0.96)), ("d", lim(0.95))]);
    assert_eq!(next_account(&accounts, &accounts[0], &full, NOW, 95), None);
    let gem = [acct("g", "gemini", true), acct("h", "gemini", true)];
    assert_eq!(next_account(&gem, &gem[0], &BTreeMap::new(), NOW, 95), None);
}

#[test]
fn decides_stay_under_the_threshold_switch_at_it_or_on_a_limit_error_else_none_left() {
    let accounts = [claude("work"), claude("home")];
    let from = &accounts[0];
    assert_eq!(
        decide_switch(
            &accounts,
            from,
            &limits([("work", lim(0.5))]),
            NOW,
            95,
            None
        ),
        SwitchDecision::Stay
    );
    assert_eq!(
        decide_switch(
            &accounts,
            from,
            &limits([("work", lim(0.97))]),
            NOW,
            95,
            None
        ),
        SwitchDecision::Switch {
            to: &accounts[1],
            reason: "work reached 97% of its 5-hour limit".into(),
            resend: false
        }
    );
    let on_error = decide_switch(
        &accounts,
        from,
        &BTreeMap::new(),
        NOW,
        95,
        Some("Claude AI usage limit reached|1"),
    );
    assert!(matches!(
        on_error,
        SwitchDecision::Switch { reason, resend: true, .. } if reason == "work hit its usage limit"
    ));
    assert_eq!(
        decide_switch(
            &accounts,
            from,
            &limits([("work", lim(0.97)), ("home", lim(0.99))]),
            NOW,
            95,
            None
        ),
        SwitchDecision::NoneLeft {
            reason: "work reached 97% of its 5-hour limit".into()
        }
    );
    // A threshold of 100 waits for the window to be full.
    assert_eq!(
        decide_switch(
            &accounts,
            from,
            &limits([("work", lim(0.97))]),
            NOW,
            100,
            None
        ),
        SwitchDecision::Stay
    );
}
