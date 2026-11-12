//! Port of `tests/unit/schedule.test.ts`: scheduled chats' wall-clock maths (against a zone with
//! daylight saving: Europe/Berlin, 26 Oct 2025 03:00 → 02:00), due and missed runs, the run
//! history, the words, and when a running run settles (#77, #117).

mod common;

use common::{at, job, TZ};
use serde_json::{json, Value};
use wraithgrid_core::config::{
    default_config, parse_config, ScheduleTarget, ScheduleWhen, ScheduledJob,
};
use wraithgrid_schedule::*;

fn weekly(days: &[i64], time: &str) -> ScheduleWhen {
    ScheduleWhen::Weekly {
        days: days.to_vec(),
        time: time.into(),
    }
}

fn next(when: &ScheduleWhen, after: i64) -> String {
    to_local(&TZ, next_run(&TZ, when, after, None).expect("a next run"))
}

#[test]
fn reads_and_writes_local_date_times() {
    assert_eq!(to_local(&TZ, at("2026-10-27T09:05")), "2026-10-27T09:05");
    assert_eq!(parse_local(&TZ, "27/10/2026"), None);
}

#[test]
fn weekdays_at_nine_across_a_weekend_and_the_daylight_saving_change() {
    let w = weekly(&[1, 2, 3, 4, 5], "09:00");
    // Friday 24 Oct 2025 10:00 → Monday 27 Oct 09:00 (after the clocks went back on Sunday).
    assert_eq!(next(&w, at("2025-10-24T10:00")), "2025-10-27T09:00");
    // Before 09:00 on a weekday: the same day.
    assert_eq!(next(&w, at("2025-10-27T08:59")), "2025-10-27T09:00");
    // Daily through the change keeps 09:00 local, even though that day has 25 hours.
    let daily = weekly(&[0, 1, 2, 3, 4, 5, 6], "09:00");
    let sat = at("2025-10-25T09:00");
    let sun = next_run(&TZ, &daily, sat, None).unwrap();
    assert_eq!(to_local(&TZ, sun), "2025-10-26T09:00");
    assert_eq!(sun - sat, 25 * 3_600_000);
}

#[test]
fn every_n_hours_from_a_time_each_day_and_once_only_in_the_future() {
    let h = ScheduleWhen::Hours {
        every: 6,
        from: "09:00".into(),
    };
    assert_eq!(next(&h, at("2026-03-10T10:00")), "2026-03-10T15:00");
    assert_eq!(next(&h, at("2026-03-10T21:00")), "2026-03-11T09:00");
    assert_eq!(next(&h, at("2026-03-10T05:00")), "2026-03-10T09:00");
    let once = ScheduleWhen::Once {
        at: "2026-03-10T12:00".into(),
    };
    assert_eq!(
        next_run(&TZ, &once, at("2026-03-10T11:00"), None),
        Some(at("2026-03-10T12:00"))
    );
    assert_eq!(next_run(&TZ, &once, at("2026-03-10T12:00"), None), None);
}

#[test]
fn a_reset_job_waits_for_the_reset_time_it_is_given() {
    let r = ScheduleWhen::Reset {
        window: "fiveHour".into(),
    };
    assert_eq!(next_run(&TZ, &r, 1000, None), None);
    assert_eq!(next_run(&TZ, &r, 1000, Some(5000)), Some(5000));
    assert_eq!(next_run(&TZ, &r, 6000, Some(5000)), None);
}

#[test]
fn runs_a_due_job_records_one_found_over_an_hour_late_as_missed() {
    let mut j = job("j1");
    j.next_at = Some(10_000);
    assert_eq!(due_action(&j, 9_999), None);
    assert_eq!(
        due_action(&j, 10_000 + MISSED_AFTER_MS),
        Some(DueAction::Run)
    );
    assert_eq!(
        due_action(&j, 10_001 + MISSED_AFTER_MS),
        Some(DueAction::Missed)
    );
    let off = ScheduledJob {
        enabled: false,
        ..j.clone()
    };
    assert_eq!(due_action(&off, 20_000), None);
    let soon = ScheduledJob {
        next_at: Some(5),
        ..j.clone()
    };
    let off_sooner = ScheduledJob {
        next_at: Some(1),
        ..off
    };
    assert_eq!(soonest_run(&[j, soon, off_sooner]), Some(5));
}

#[test]
fn keeps_the_newest_runs_and_drops_a_bad_job_rather_than_the_list() {
    let mut j = job("j1");
    for i in 0..25 {
        j = with_run(j, run_record(i, RunResult::Done, None, ""));
    }
    assert_eq!(j.runs.len(), SCHEDULE_RUNS_MAX);
    assert_eq!(j.runs[0].at, 24);

    // The schema lives in core's config parser; a job there gets its defaults, a bad one drops.
    let ok = json!({
        "id": "j1",
        "wsId": "w",
        "accountId": "a",
        "prompt": "Review PRs",
        "when": { "kind": "weekly", "days": [1], "time": "09:00" }
    });
    let parse = |jobs: Value| {
        let mut v: Value = serde_json::from_str(&default_config().to_json()).unwrap();
        v["schedules"] = jobs;
        parse_config(&v, false).unwrap().schedules
    };
    let got = parse(json!([ok]));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].target, ScheduleTarget::New);
    assert_eq!(got[0].at_limit, "wait");
    let mut bad = ok.clone();
    bad["when"] = json!({ "kind": "weekly", "days": [], "time": "9" });
    assert!(parse(json!([bad])).is_empty());
}

#[test]
fn says_when_in_words() {
    let say = |w: &ScheduleWhen, name| describe_when(&TZ, w, name);
    assert_eq!(
        say(&weekly(&[5, 1, 2, 3, 4], "09:00"), None),
        "weekdays 09:00"
    );
    assert_eq!(
        say(&weekly(&[0, 1, 2, 3, 4, 5, 6], "18:30"), None),
        "daily 18:30"
    );
    assert_eq!(say(&weekly(&[1, 4], "09:00"), None), "Mon, Thu 09:00");
    let hours = ScheduleWhen::Hours {
        every: 6,
        from: "09:00".into(),
    };
    assert_eq!(say(&hours, None), "every 6 hours");
    let reset = ScheduleWhen::Reset {
        window: "fiveHour".into(),
    };
    assert_eq!(say(&reset, Some("work")), "work 5-hour limit resets");
}

#[test]
fn only_a_run_this_launch_started_says_done_a_stale_one_settles_quietly() {
    let run = run_record(1_000, RunResult::Running, Some("p1".into()), "");
    let view = RunView {
        live: true,
        pane: PaneWhere::Open,
        exited: false,
        quiet: true,
        now: 60_000,
    };
    let settle = |result, note: &str, notify| {
        Some(Settle {
            result,
            note: note.into(),
            notify,
        })
    };
    // Its own turn ended: done, with the notification.
    assert_eq!(settle_of(&run, &view), settle(RunResult::Done, "", true));
    // Still going, or quiet within 10 s of the send: not yet.
    let v = |f: fn(&mut RunView)| {
        let mut v = view;
        f(&mut v);
        v
    };
    assert_eq!(settle_of(&run, &v(|v| v.quiet = false)), None);
    assert_eq!(settle_of(&run, &v(|v| v.now = 5_000)), None);
    // Left running by an earlier launch: an unrelated later turn in that chat never notifies.
    assert_eq!(
        settle_of(&run, &v(|v| v.live = false)),
        settle(
            RunResult::Failed,
            "interrupted: Wraithgrid closed during the run",
            false
        )
    );
    let stale_busy = v(|v| {
        v.live = false;
        v.quiet = false;
    });
    assert!(!settle_of(&run, &stale_busy).unwrap().notify);
    // Closed to History (it ran), or gone: settled, never notified.
    let history = v(|v| {
        v.live = false;
        v.pane = PaneWhere::History;
    });
    assert_eq!(
        settle_of(&run, &history),
        settle(RunResult::Done, "", false)
    );
    assert_eq!(
        settle_of(&run, &v(|v| v.pane = PaneWhere::Gone))
            .unwrap()
            .result,
        RunResult::Failed
    );
    let exited = settle_of(
        &run,
        &v(|v| {
            v.exited = true;
            v.quiet = false;
        }),
    )
    .unwrap();
    assert_eq!((exited.result, exited.notify), (RunResult::Failed, false));
    // Only running runs with a chat are settled.
    let done = run_record(1_000, RunResult::Done, Some("p1".into()), "");
    assert_eq!(settle_of(&done, &view), None);
    let no_chat = run_record(1_000, RunResult::Running, None, "");
    assert_eq!(settle_of(&no_chat, &view), None);
}
