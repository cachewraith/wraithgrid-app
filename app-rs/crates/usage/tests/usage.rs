//! The usage model and store, ported from tests/unit/usage.test.ts (the claude adapter and
//! renderer formatting parts live with those ports), plus reading a file the Electron app wrote.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use wraithgrid_usage::*;

const DAY: i64 = 86_400_000;

/// 2026-10-21 15:00 local time, as `new Date('2026-10-21T15:00:00')`.
fn now() -> i64 {
    Local
        .with_ymd_and_hms(2026, 10, 21, 15, 0, 0)
        .single()
        .unwrap()
        .timestamp_millis()
}

fn turn_usage(cost_usd: f64, input: u64) -> TurnUsage {
    TurnUsage {
        model: Some("claude-opus-5-5".into()),
        cost_usd: Some(cost_usd),
        input,
        output: 5,
        cache_read: 100,
        cache_write: 0,
    }
}

/// The test's `owner` (a1 / w1 / p1 / "api") at `at`.
fn turn(at: i64, usage: TurnUsage) -> Turn {
    Turn {
        at,
        account_id: Some("a1".into()),
        workspace_id: Some("w1".into()),
        pane_id: "p1".into(),
        title: "api".into(),
        usage,
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn a_turn_with_no_cost_counts_its_tokens_and_its_cost_as_unknown() {
    let tokens_only = TurnUsage {
        cost_usd: None,
        ..turn_usage(0.0, 10)
    };
    let mut d = UsageData::default();
    let cx = |at| Turn {
        account_id: Some("cx".into()),
        ..turn(at, tokens_only.clone())
    };
    add_turn(&mut d, &cx(now()));
    add_turn(&mut d, &cx(now() + 1));
    let b = &d.buckets[0];
    assert_eq!((b.cost_usd, b.turns, b.unpriced, b.input), (0.0, 2, 2, 20));
    let sum = summarize_usage(&d, UsageRange::Today, now());
    let a = &sum.accounts["cx"].totals;
    assert_eq!((a.turns, a.unpriced), (2, 2));
    assert!(!a.has_cost());
    assert!(!sum.chats[0].has_cost());
    // Mixed with a priced claude turn: the total has a cost, codex's part adds nothing to it.
    add_turn(
        &mut d,
        &Turn {
            account_id: Some("cl".into()),
            ..turn(now(), turn_usage(0.5, 10))
        },
    );
    let mixed = summarize_usage(&d, UsageRange::Today, now());
    assert_eq!(
        (
            mixed.total.cost_usd,
            mixed.total.turns,
            mixed.total.unpriced
        ),
        (0.5, 3, 2)
    );
    assert!(mixed.total.has_cost());
    // Files written before #53 have no `unpriced`: every turn there had a cost.
    let mut old = serde_json::to_value(&d.buckets[1]).unwrap();
    old.as_object_mut().unwrap().remove("unpriced");
    let parsed = parse_usage_data(&json!({ "buckets": [old] })).unwrap();
    assert_eq!(parsed.buckets[0].unpriced, 0);
}

#[test]
fn adds_turns_into_one_bucket_per_day_account_project_and_chat() {
    let mut d = UsageData::default();
    add_turn(&mut d, &turn(now(), turn_usage(0.5, 10)));
    add_turn(&mut d, &turn(now() + 1000, turn_usage(0.25, 10)));
    add_turn(
        &mut d,
        &Turn {
            pane_id: "p2".into(),
            title: "web".into(),
            ..turn(now(), turn_usage(1.0, 10))
        },
    );
    add_turn(&mut d, &turn(now() - DAY, turn_usage(2.0, 10)));
    assert_eq!(d.buckets.len(), 3);
    let b = &d.buckets[0];
    assert_eq!(b.day, day_key(now()));
    assert!(close(b.cost_usd, 0.75));
    assert_eq!((b.turns, b.input), (2, 20));
}

#[test]
fn summarizes_a_range_totals_per_account_daily_series_projects_and_top_chats() {
    let mut d = UsageData::default();
    add_turn(&mut d, &turn(now(), turn_usage(0.5, 10)));
    add_turn(
        &mut d,
        &Turn {
            pane_id: "p2".into(),
            title: "web".into(),
            workspace_id: Some("w2".into()),
            ..turn(now(), turn_usage(1.0, 10))
        },
    );
    add_turn(&mut d, &turn(now() - 3 * DAY, turn_usage(2.0, 10)));
    add_turn(
        &mut d,
        &Turn {
            account_id: Some("a2".into()),
            ..turn(now() - 20 * DAY, turn_usage(4.0, 10))
        },
    );

    let today = summarize_usage(&d, UsageRange::Today, now());
    assert!(close(today.total.cost_usd, 1.5));
    let a1 = &today.accounts["a1"];
    assert_eq!(a1.daily.len(), 7);
    // The week's trend is there even for "Today"; totals are today's only.
    assert!(close(a1.daily.iter().map(|p| p.cost_usd).sum(), 3.5));
    assert!(close(a1.totals.cost_usd, 1.5));

    let week = summarize_usage(&d, UsageRange::Days7, now());
    assert!(close(week.total.cost_usd, 3.5));
    let ids: Vec<_> = week
        .projects
        .iter()
        .map(|p| p.workspace_id.as_deref())
        .collect();
    assert_eq!(ids, [Some("w1"), Some("w2")]);
    assert_eq!(week.chats[0].pane_id, "p1");
    assert!(close(week.chats[0].cost_usd, 2.5));
    assert!(!week.accounts.contains_key("a2"));

    let month = summarize_usage(&d, UsageRange::Days30, now());
    assert!(close(month.total.cost_usd, 7.5));
    assert_eq!(month.accounts["a2"].daily.len(), 30);
    assert_eq!(month.total.tokens, 4 * 115);
}

#[test]
fn keeps_90_days_and_a_passed_limit_window_reads_as_empty() {
    let mut d = UsageData::default();
    add_turn(&mut d, &turn(now() - 95 * DAY, turn_usage(1.0, 10)));
    prune_usage(&mut d, now());
    assert!(d.buckets.is_empty());
    assert_eq!(
        live_window(
            Some(UsageWindow {
                utilization: 0.8,
                resets_at: now() - 1
            }),
            now()
        ),
        Some(UsageWindow {
            utilization: 0.0,
            resets_at: now() - 1
        })
    );
    assert_eq!(live_window(None, now()), None);
}

#[test]
fn a_broken_usage_file_loads_what_it_can() {
    let r = parse_usage_data(&json!({
        "buckets": "nope",
        "limits": { "a1": { "fiveHour": "x", "sevenDay": null, "at": 1 } }
    }))
    .unwrap();
    assert!(r.buckets.is_empty());
    assert_eq!(
        r.limits["a1"],
        AccountLimits {
            five_hour: None,
            seven_day: None,
            at: 1
        }
    );
    // One bad bucket empties the list, as zod's `.catch([])` does; not an object at all fails.
    let bad = parse_usage_data(&json!({ "buckets": [{ "day": "2026-1-1", "paneId": "p" }] }));
    assert!(bad.unwrap().buckets.is_empty());
    assert_eq!(parse_usage_data(&json!([])), None);
}

/// usage.json as the Electron app writes it (`JSON.stringify`).
const ELECTRON_FILE: &str = r#"{"buckets":[{"day":"2026-10-21","accountId":"a1","workspaceId":"w1","paneId":"p1","title":"api","costUsd":0.13964780000000002,"input":2,"output":14,"cacheRead":8919,"cacheWrite":17197,"turns":1,"unpriced":0},{"day":"2026-10-20","accountId":null,"workspaceId":null,"paneId":"p2","title":"","costUsd":0,"input":10,"output":5,"cacheRead":100,"cacheWrite":0,"turns":2,"unpriced":2}],"limits":{"a1":{"fiveHour":{"utilization":0.26,"resetsAt":1792600800000},"sevenDay":null,"at":1792590000000}}}"#;

#[test]
fn reads_a_file_the_electron_app_wrote_and_writes_the_same_keys() {
    let raw: Value = serde_json::from_str(ELECTRON_FILE).unwrap();
    let d = parse_usage_data(&raw).unwrap();
    assert_eq!(d.buckets.len(), 2);
    assert_eq!(d.buckets[0].cost_usd, 0.13964780000000002);
    assert_eq!(d.buckets[0].cache_write, 17197);
    assert_eq!(d.buckets[1].account_id, None);
    assert_eq!(
        d.limits["a1"].five_hour,
        Some(UsageWindow {
            utilization: 0.26,
            resets_at: 1_792_600_800_000
        })
    );
    // Written back, it reads as the same data, key for key.
    let again: Value = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
    assert_eq!(parse_usage_data(&again).unwrap(), d);
    let keys = |v: &Value| -> Vec<String> {
        v["buckets"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    assert_eq!(keys(&again), keys(&raw));
}

fn store(writes: Arc<Mutex<Vec<String>>>, file: &Path) -> UsageStore {
    let mut deps = UsageStoreDeps::new(
        file,
        Box::new(|id| {
            (id == "p1").then(|| PaneOwner {
                account_id: Some("a1".into()),
                workspace_id: Some("w1".into()),
                title: "api".into(),
            })
        }),
    );
    deps.now = Box::new(now);
    deps.write = Box::new(move |_f, text| {
        writes.lock().unwrap().push(text.to_owned());
        Ok(())
    });
    UsageStore::new(deps)
}

#[test]
fn store_records_turns_and_limits_for_the_panes_account_and_saves_them() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let file = std::env::temp_dir().join(format!("wg-usage-missing-{}.json", std::process::id()));
    let mut s = store(writes.clone(), &file);
    s.load();
    let window = UsageWindow {
        utilization: 0.4,
        resets_at: now() + 1000,
    };
    s.on_chat_event(
        "p1",
        &UsageEvent::Limits(AccountLimits {
            five_hour: Some(window),
            seven_day: None,
            at: now(),
        }),
    );
    s.on_chat_event("p1", &UsageEvent::TurnEnd(Some(turn_usage(0.2, 10))));
    s.on_chat_event("p1", &UsageEvent::TurnEnd(None));
    s.on_chat_event(
        "gone",
        &UsageEvent::Limits(AccountLimits {
            five_hour: None,
            seven_day: None,
            at: now(),
        }),
    );
    let got = s.get();
    assert_eq!(got.limits.len(), 1);
    assert_eq!(got.limits["a1"].five_hour, Some(window));
    assert_eq!(got.buckets.len(), 1);
    assert!(writes.lock().unwrap().is_empty());
    s.tick(); // Not due yet: the clock stands still.
    assert!(writes.lock().unwrap().is_empty());
    s.flush();
    let w = writes.lock().unwrap();
    assert_eq!(w.len(), 1);
    let saved: Value = serde_json::from_str(&w[0]).unwrap();
    assert_eq!(saved["buckets"][0]["accountId"], "a1");
    assert_eq!(saved["buckets"][0]["costUsd"], 0.2);
}

#[test]
fn store_saves_once_the_delay_has_passed_and_loads_its_own_file() {
    let dir = std::env::temp_dir().join(format!("wg-usage-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("usage.json");
    let clock = Arc::new(Mutex::new(now()));
    let make = |clock: Arc<Mutex<i64>>| {
        let mut deps = UsageStoreDeps::new(&file, Box::new(|_| None));
        deps.now = Box::new(move || *clock.lock().unwrap());
        UsageStore::new(deps)
    };
    let mut s = make(clock.clone());
    s.set_limits(
        "a1",
        AccountLimits {
            five_hour: None,
            seven_day: None,
            at: now(),
        },
    );
    assert_eq!(s.save_due_at(), Some(now() + 1500));
    *clock.lock().unwrap() += 1499;
    s.tick();
    assert!(!file.exists());
    *clock.lock().unwrap() += 1;
    s.tick();
    assert_eq!(s.save_due_at(), None);

    let mut again = make(clock);
    again.load();
    assert_eq!(again.get(), s.get());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn writes_atomically_owner_only_and_leaves_no_temp_file() {
    let dir = std::env::temp_dir().join(format!("wg-usage-atomic-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("usage.json");
    write_file_atomic(&file, "{\"a\":1}").unwrap();
    write_file_atomic(&file, "{\"a\":2}").unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "{\"a\":2}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}
