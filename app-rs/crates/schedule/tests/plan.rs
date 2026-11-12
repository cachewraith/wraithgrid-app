//! The store's scheduling decisions (#77): saving and enabling jobs, "Send later", how a run
//! starts (missing project/account, still going, at the limit), and what a tick does with a
//! due, missed, waiting or sent job. Plus wall-clock edges the TS tests leave implicit.

mod common;

use common::{at, job, TZ};
use wraithgrid_core::config::{ScheduleTarget, ScheduleWhen, ScheduledJob};
use wraithgrid_schedule::*;

fn limits(five: Option<(f64, i64)>, week: Option<(f64, i64)>) -> Limits {
    let w = |x: Option<(f64, i64)>| {
        x.map(|(utilization, resets_at)| WindowLimit {
            utilization,
            resets_at,
        })
    };
    Limits {
        five_hour: w(five),
        seven_day: w(week),
    }
}

fn reset_job(window: &str) -> ScheduledJob {
    ScheduledJob {
        when: ScheduleWhen::Reset {
            window: window.into(),
        },
        ..job("r")
    }
}

const ME: AccountRef = AccountRef {
    id: "a",
    name: "work",
};
const OTHER: AccountRef = AccountRef {
    id: "b",
    name: "home",
};

fn ctx() -> RunContext<'static> {
    RunContext {
        now: 1_000,
        project_exists: true,
        account: Some(ME),
        last_pane_status: None,
        at_limit: false,
        auto_switch_off: false,
        switch_to: Some(OTHER),
    }
}

fn recorded(start: RunStart) -> (String, Option<String>, String) {
    match start {
        RunStart::Record(r) => (r.result, r.pane_id, r.note),
        other => panic!("expected a record, got {other:?}"),
    }
}

#[test]
fn reset_jobs_run_just_after_the_reset_of_their_window() {
    let l = limits(Some((0.5, 100_000)), Some((1.0, 900_000)));
    let five = reset_job("fiveHour");
    assert_eq!(next_of(&TZ, &five, 0, Some(&l)), Some(130_000));
    assert_eq!(
        next_of(&TZ, &reset_job("sevenDay"), 0, Some(&l)),
        Some(930_000)
    );
    // Past the reset (and its grace), or no limits known yet: not known.
    assert_eq!(next_of(&TZ, &five, 130_000, Some(&l)), None);
    assert_eq!(next_of(&TZ, &five, 0, None), None);
    // Waiting at a limit: the later reset still ahead.
    assert_eq!(reset_of(&job("j"), Some(&l), 0), Some(900_000));
    assert_eq!(reset_of(&job("j"), Some(&l), 950_000), None);
}

#[test]
fn saving_works_out_the_next_run_and_keeps_to_the_cap() {
    let mut jobs = vec![];
    let now = at("2026-03-09T10:00"); // a Monday
    assert!(save_job(&TZ, &mut jobs, job("j0"), now, None));
    assert_eq!(to_local(&TZ, jobs[0].next_at.unwrap()), "2026-03-16T09:00");
    // Replacing keeps the place; disabled has no next run.
    let off = ScheduledJob {
        enabled: false,
        ..job("j0")
    };
    assert!(save_job(&TZ, &mut jobs, off, now, None));
    assert_eq!((jobs.len(), jobs[0].next_at), (1, None));
    for i in 1..SCHEDULES_MAX {
        assert!(save_job(&TZ, &mut jobs, job(&format!("j{i}")), now, None));
    }
    assert!(!save_job(&TZ, &mut jobs, job("extra"), now, None));
    assert_eq!(jobs.len(), SCHEDULES_MAX);
    let on = set_enabled(&TZ, jobs[0].clone(), true, now, None);
    assert!(on.enabled && on.next_at.is_some());
    assert_eq!(set_enabled(&TZ, on, false, now, None).next_at, None);
}

#[test]
fn send_later_is_a_one_shot_for_one_chat() {
    let at_ms = at("2026-03-10T12:00");
    let j = queue_message(
        &TZ,
        "q".into(),
        "w",
        "a",
        "p1",
        "  hi  ",
        SendLater::At(at_ms),
        None,
    )
    .unwrap();
    assert_eq!(j.prompt, "hi");
    assert_eq!(
        j.when,
        ScheduleWhen::Once {
            at: "2026-03-10T12:00".into()
        }
    );
    assert_eq!(
        j.target,
        ScheduleTarget::Chat {
            pane_id: "p1".into()
        }
    );
    assert!(j.one_shot && j.enabled && j.at_limit == "wait");
    // At the reset of the fuller window.
    let l = limits(Some((0.4, 1)), Some((1.0, 2)));
    let r = queue_message(
        &TZ,
        "q".into(),
        "w",
        "a",
        "p1",
        "x",
        SendLater::Reset,
        Some(&l),
    );
    assert_eq!(
        r.unwrap().when,
        ScheduleWhen::Reset {
            window: "sevenDay".into()
        }
    );
    let r = queue_message(&TZ, "q".into(), "w", "a", "p1", "x", SendLater::Reset, None);
    assert_eq!(
        r.unwrap().when,
        ScheduleWhen::Reset {
            window: "fiveHour".into()
        }
    );
    let blank = queue_message(&TZ, "q".into(), "w", "a", "p", " ", SendLater::Reset, None);
    assert!(blank.is_none());
}

#[test]
fn a_run_starts_unless_its_project_account_or_previous_run_says_no() {
    let j = job("j");
    let gone = RunContext {
        project_exists: false,
        account: None,
        ..ctx()
    };
    assert_eq!(
        recorded(start_run(&j, &gone)),
        ("failed".into(), None, "project missing".into())
    );
    let no_account = RunContext {
        account: None,
        ..ctx()
    };
    assert_eq!(recorded(start_run(&j, &no_account)).2, "account missing");
    // The previous run's chat is still busy: skipped.
    let going = with_run(j.clone(), running(500, "p1", ""));
    let busy = RunContext {
        last_pane_status: Some("approval"),
        ..ctx()
    };
    assert_eq!(
        recorded(start_run(&going, &busy)),
        (
            "skipped".into(),
            Some("p1".into()),
            "previous run still going".into()
        )
    );
    let idle = RunContext {
        last_pane_status: Some("idle"),
        ..ctx()
    };
    assert_eq!(
        start_run(&going, &idle),
        RunStart::NewChat {
            account_id: "a".into(),
            note: String::new()
        }
    );
    let chat = ScheduledJob {
        target: ScheduleTarget::Chat {
            pane_id: "p9".into(),
        },
        ..j
    };
    assert_eq!(
        start_run(&chat, &ctx()),
        RunStart::Chat {
            pane_id: "p9".into()
        }
    );
    assert_eq!(chat_gone(5).note, "that chat is gone");
    assert_eq!(no_folder(5).note, "project has no folder");
    assert_eq!(running(5, "p1", "as home").result, "running");
}

#[test]
fn at_the_limit_it_waits_or_switches_account() {
    let full = RunContext {
        at_limit: true,
        ..ctx()
    };
    let wait = job("j");
    assert_eq!(
        recorded(start_run(&wait, &full)),
        ("waiting".into(), None, "work is at its limit".into())
    );
    let switch = ScheduledJob {
        at_limit: "switch".into(),
        ..job("j")
    };
    assert_eq!(
        start_run(&switch, &full),
        RunStart::NewChat {
            account_id: "b".into(),
            note: "as home".into()
        }
    );
    // Auto-switch off, or nobody with room: it waits.
    let off = RunContext {
        auto_switch_off: true,
        ..full
    };
    assert_eq!(recorded(start_run(&switch, &off)).0, "waiting");
    let none = RunContext {
        switch_to: None,
        ..full
    };
    assert_eq!(recorded(start_run(&switch, &none)).0, "waiting");
}

#[test]
fn a_tick_learns_the_next_run_runs_due_jobs_and_records_missed_ones() {
    let now = at("2026-03-09T10:00");
    assert_eq!(
        tick_job(
            &TZ,
            &ScheduledJob {
                enabled: false,
                ..job("j")
            },
            now,
            None
        ),
        Tick::Idle
    );
    // No next run yet: it learns it, and runs on a later tick.
    let Tick::Update(learned) = tick_job(&TZ, &job("j"), now, None) else {
        panic!("expected an update")
    };
    assert_eq!(to_local(&TZ, learned.next_at.unwrap()), "2026-03-16T09:00");
    assert_eq!(tick_job(&TZ, &reset_job("fiveHour"), now, None), Tick::Idle);
    let due = ScheduledJob {
        next_at: Some(now - 1),
        ..job("j")
    };
    assert_eq!(tick_job(&TZ, &due, now, None), Tick::Run);
    // Found two hours late (the app was closed): recorded as missed, next one planned.
    let late = ScheduledJob {
        next_at: Some(now - 2 * 3_600_000),
        ..job("j")
    };
    let Tick::Update(missed) = tick_job(&TZ, &late, now, None) else {
        panic!("expected an update")
    };
    assert_eq!(missed.runs[0].result, "missed");
    assert_eq!(missed.runs[0].note, "Wraithgrid was not running");
    assert!(missed.enabled && missed.next_at.is_some());
    // A missed one-off is off for good.
    let once = ScheduledJob {
        when: ScheduleWhen::Once {
            at: "2026-03-09T08:00".into(),
        },
        ..late
    };
    let Tick::Update(once) = tick_job(&TZ, &once, now, None) else {
        panic!("expected an update")
    };
    assert!(!once.enabled);
    assert_eq!(once.next_at, None);
}

#[test]
fn after_a_run_the_job_waits_is_deleted_or_plans_its_next() {
    let now = 1_000_000;
    let l = limits(Some((1.0, 2_000_000)), None);
    let wait = run_record(now, RunResult::Waiting, None, "work is at its limit");
    // Waiting: recorded once, tried again just after the reset (or in 10 min when unknown).
    let JobChange::Update(w) = after_tick_run(&TZ, job("j"), wait.clone(), now, Some(&l)) else {
        panic!("expected an update")
    };
    assert_eq!((w.runs.len(), w.next_at), (1, Some(2_030_000)));
    let JobChange::Update(w) = after_tick_run(&TZ, w, wait.clone(), now, None) else {
        panic!("expected an update")
    };
    assert_eq!((w.runs.len(), w.next_at), (1, Some(now + WAIT_RETRY_MS)));
    // A sent "Send later" message goes; skipped, it stays. Waiting keeps it too.
    let one = ScheduledJob {
        one_shot: true,
        ..job("j")
    };
    let sent = running(now, "p1", "");
    assert_eq!(
        after_tick_run(&TZ, one.clone(), sent.clone(), now, None),
        JobChange::Delete
    );
    let failed = chat_gone(now);
    assert_eq!(
        after_tick_run(&TZ, one.clone(), failed, now, None),
        JobChange::Delete
    );
    let skipped = run_record(now, RunResult::Skipped, Some("p1".into()), "");
    assert!(matches!(
        after_tick_run(&TZ, one.clone(), skipped, now, None),
        JobChange::Update(_)
    ));
    assert!(matches!(
        after_tick_run(&TZ, one.clone(), wait, now, None),
        JobChange::Update(_)
    ));
    // A one-off ran: off, no next run.
    let once = ScheduledJob {
        when: ScheduleWhen::Once {
            at: "2026-03-09T08:00".into(),
        },
        next_at: Some(now),
        ..job("j")
    };
    let JobChange::Update(o) = after_tick_run(&TZ, once, sent.clone(), now, None) else {
        panic!("expected an update")
    };
    assert_eq!((o.enabled, o.next_at, o.runs.len()), (false, None, 1));
    // Run now: a sent queued message goes; anything else is recorded, the schedule kept.
    assert_eq!(after_run_now(one, sent.clone()), JobChange::Delete);
    let JobChange::Update(r) = after_run_now(job("j"), sent) else {
        panic!("expected an update")
    };
    assert_eq!(r.runs[0].pane_id.as_deref(), Some("p1"));
    let s = settled(
        r,
        &Settle {
            result: RunResult::Done,
            note: "ok".into(),
            notify: true,
        },
    );
    assert_eq!(
        (s.runs[0].result.as_str(), s.runs[0].note.as_str()),
        ("done", "ok")
    );
    assert_eq!(live_key("j", 42), "j:42");
}

#[test]
fn wall_clock_edges() {
    // The hour the clocks skip (29 Mar 2026 02:00 → 03:00) reads like JavaScript: 02:30 is 03:30.
    assert_eq!(to_local(&TZ, at("2026-03-29T02:30")), "2026-03-29T03:30");
    // The hour that repeats (25 Oct 2026 02:00–03:00): the first one, two hours before 03:30.
    assert_eq!(
        at("2026-10-25T03:30") - at("2026-10-25T02:30"),
        2 * 3_600_000
    );
    // Days and months past the end roll over, as `new Date(y, m, d)` does.
    assert_eq!(to_local(&TZ, at("2026-02-30T09:00")), "2026-03-02T09:00");
    assert_eq!(to_local(&TZ, at("2026-13-01T09:00")), "2027-01-01T09:00");
    // Every 6 hours on the day the clocks go forward keeps the wall times.
    let h = ScheduleWhen::Hours {
        every: 6,
        from: "00:00".into(),
    };
    let t = next_run(&TZ, &h, at("2026-03-29T00:00"), None).unwrap();
    assert_eq!(to_local(&TZ, t), "2026-03-29T06:00");
    assert_eq!(t - at("2026-03-29T00:00"), 5 * 3_600_000);
    // The words and the form.
    let now = at("2026-10-27T08:00");
    assert_eq!(fmt_day(&TZ, now), "Tue 27 Oct");
    assert_eq!(fmt_when(&TZ, at("2026-10-27T09:00"), now), "09:00");
    assert_eq!(fmt_when(&TZ, at("2026-10-28T09:00"), now), "Wed 09:00");
    let once = ScheduleWhen::Once {
        at: "2026-10-27T09:00".into(),
    };
    assert_eq!(describe_when(&TZ, &once, None), "once Tue 27 Oct 09:00");
    assert!(when_ok(&TZ, &once, now));
    assert!(!when_ok(&TZ, &once, at("2026-10-27T09:00")));
    assert!(!when_ok(
        &TZ,
        &ScheduleWhen::Weekly {
            days: vec![],
            time: "09:00".into()
        },
        now
    ));
    let daily = ScheduleWhen::Weekly {
        days: vec![0, 1, 2, 3, 4, 5, 6],
        time: "09:00".into(),
    };
    let next3: Vec<String> = upcoming(&TZ, &daily, now, 3)
        .into_iter()
        .map(|t| to_local(&TZ, t))
        .collect();
    assert_eq!(
        next3,
        ["2026-10-27T09:00", "2026-10-28T09:00", "2026-10-29T09:00"]
    );
    assert!(upcoming(
        &TZ,
        &ScheduleWhen::Reset {
            window: "fiveHour".into()
        },
        now,
        3
    )
    .is_empty());
    assert_eq!(
        default_once_at(&TZ, at("2026-10-27T08:05")),
        "2026-10-27T09:00"
    );
    assert!(is_time("23:59") && !is_time("24:00") && !is_time("9:00"));
    assert!(is_local_datetime("2026-10-27T09:00") && !is_local_datetime("2026-10-27 09:00"));
    assert_eq!(RunResult::parse("waiting"), Some(RunResult::Waiting));
    assert_eq!(RunResult::parse("nope"), None);
}
