//! The scheduling decisions of the renderer store (#77), kept pure: given the jobs, the time and
//! what the app knows of each account's limits, say what to do (run, record a miss, wait for a
//! reset, delete a sent "Send later" message). The app does the side effects (opening a chat,
//! sending, notifying) and records the outcome through these functions. Port of the
//! `saveJob`/`setJobEnabled`/`runJobNow`/`queueMessage`/`tickSchedules` logic in
//! `src/renderer/app/store.ts`.

use chrono::TimeZone;
use wraithgrid_core::config::{ScheduleRun, ScheduleTarget, ScheduleWhen, ScheduledJob};

use crate::runs::{due_action, run_record, with_run, DueAction, RunResult, SCHEDULES_MAX};
use crate::when::{next_run, to_local};

/// How often scheduled jobs are checked.
pub const SCHEDULE_TICK_MS: u64 = 15_000;
/// The first check after launch: runs due while the app was closed run (under an hour late) or
/// are recorded as missed.
pub const FIRST_TICK_MS: u64 = 1_500;
/// A reset job (or one waiting at a limit) runs this long after the reset.
pub const RESET_GRACE_MS: i64 = 30_000;
/// Waiting at a limit whose reset is unknown: try again after this.
pub const WAIT_RETRY_MS: i64 = 10 * 60_000;
/// Pane statuses in which the previous run is still going.
pub const RUN_BUSY: [&str; 4] = ["running", "starting", "approval", "background"];

/// One limit window of an account, as the CLI last reported it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowLimit {
    /// Used, 0–1.
    pub utilization: f64,
    /// When it starts over (ms).
    pub resets_at: i64,
}

/// An account's limit windows (the app maps them from the usage store).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Limits {
    pub five_hour: Option<WindowLimit>,
    pub seven_day: Option<WindowLimit>,
}

/// An account as a run needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountRef<'a> {
    pub id: &'a str,
    pub name: &'a str,
}

/// The key of a run this launch started (`jobId:at`); only these may notify "done" (#117).
pub fn live_key(job_id: &str, at: i64) -> String {
    format!("{job_id}:{at}")
}

/// The account window's reset, for `reset` jobs and for waiting at a limit (the later reset
/// still ahead when both windows have one). `limits`: the job's account's.
pub fn reset_of(job: &ScheduledJob, limits: Option<&Limits>, now: i64) -> Option<i64> {
    let l = limits?;
    if let ScheduleWhen::Reset { window } = &job.when {
        let w = match window.as_str() {
            "fiveHour" => l.five_hour,
            "sevenDay" => l.seven_day,
            _ => None,
        };
        return w.map(|w| w.resets_at);
    }
    [l.five_hour, l.seven_day]
        .into_iter()
        .flatten()
        .map(|w| w.resets_at)
        .filter(|t| *t > now)
        .max()
}

/// When the job runs next after `after`: a reset job waits a little past the reset.
pub fn next_of<Tz: TimeZone>(
    tz: &Tz,
    job: &ScheduledJob,
    after: i64,
    limits: Option<&Limits>,
) -> Option<i64> {
    if !matches!(job.when, ScheduleWhen::Reset { .. }) {
        return next_run(tz, &job.when, after, None);
    }
    reset_of(job, limits, after)
        .map(|r| r + RESET_GRACE_MS)
        .filter(|r| *r > after)
}

/// Adds or replaces a job, working out its next run. False (nothing changed): a new job when the
/// list already holds [`SCHEDULES_MAX`].
pub fn save_job<Tz: TimeZone>(
    tz: &Tz,
    jobs: &mut Vec<ScheduledJob>,
    mut job: ScheduledJob,
    now: i64,
    limits: Option<&Limits>,
) -> bool {
    job.next_at = if job.enabled {
        next_of(tz, &job, now, limits)
    } else {
        None
    };
    match jobs.iter().position(|j| j.id == job.id) {
        Some(at) => jobs[at] = job,
        None if jobs.len() >= SCHEDULES_MAX => return false,
        None => jobs.push(job),
    }
    true
}

/// The job switched on (with its next run) or off.
pub fn set_enabled<Tz: TimeZone>(
    tz: &Tz,
    mut job: ScheduledJob,
    enabled: bool,
    now: i64,
    limits: Option<&Limits>,
) -> ScheduledJob {
    job.enabled = enabled;
    job.next_at = if enabled {
        next_of(tz, &job, now, limits)
    } else {
        None
    };
    job
}

/// When a "Send later" message goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendLater {
    /// At this time (ms).
    At(i64),
    /// When the account's full window resets.
    Reset,
}

/// "Send later" (#77): a one-shot job for one chat, or None for an empty message. `limits`: the
/// chat's account's; a reset waits for the fuller window. The app then [`save_job`]s it.
#[allow(clippy::too_many_arguments)]
pub fn queue_message<Tz: TimeZone>(
    tz: &Tz,
    id: String,
    ws_id: &str,
    account_id: &str,
    pane_id: &str,
    text: &str,
    when: SendLater,
    limits: Option<&Limits>,
) -> Option<ScheduledJob> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let use_of = |w: Option<WindowLimit>| w.map_or(0.0, |w| w.utilization);
    // The window that is full (the later reset when both are).
    let window =
        if use_of(limits.and_then(|l| l.seven_day)) > use_of(limits.and_then(|l| l.five_hour)) {
            "sevenDay"
        } else {
            "fiveHour"
        };
    Some(ScheduledJob {
        id,
        ws_id: ws_id.into(),
        account_id: account_id.into(),
        prompt: text.into(),
        model: None,
        effort: None,
        chat_mode: None,
        when: match when {
            SendLater::Reset => ScheduleWhen::Reset {
                window: window.into(),
            },
            SendLater::At(ms) => ScheduleWhen::Once {
                at: to_local(tz, ms),
            },
        },
        target: ScheduleTarget::Chat {
            pane_id: pane_id.into(),
        },
        at_limit: "wait".into(),
        enabled: true,
        one_shot: true,
        next_at: None,
        runs: vec![],
    })
}

/// What the app knows when it starts a run of a job.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunContext<'a> {
    pub now: i64,
    /// The job's project still exists.
    pub project_exists: bool,
    /// The job's account; None when it is gone.
    pub account: Option<AccountRef<'a>>,
    /// Status of the pane the job's last run is in (`runs[0].pane_id`), if it has one.
    pub last_pane_status: Option<&'a str>,
    /// The account's fullest window is at 100% (usage's `peak_use`).
    pub at_limit: bool,
    /// Settings: auto-switch is off.
    pub auto_switch_off: bool,
    /// The account to carry on as at the limit (usage's `next_account` at the auto-switch
    /// threshold); only used when the job switches at the limit.
    pub switch_to: Option<AccountRef<'a>>,
}

/// How to start a run.
#[derive(Clone, Debug, PartialEq)]
pub enum RunStart {
    /// Nothing to start; record this run (failed, skipped or waiting).
    Record(ScheduleRun),
    /// Send the prompt to the target chat. The app reopens it from History if need be; when it
    /// cannot, it records [`chat_gone`], otherwise [`running`]`(now, pane_id, "")`.
    Chat { pane_id: String },
    /// Open a new chat in the project as `account_id` with the prompt as its first message. The
    /// app records [`no_folder`] when the project has no folder, otherwise
    /// [`running`]`(now, new_pane_id, note)`.
    NewChat { account_id: String, note: String },
}

/// Decides how one run of a job starts (`startJobRun`, before its side effects).
pub fn start_run(job: &ScheduledJob, ctx: &RunContext) -> RunStart {
    let at = ctx.now;
    let record = |result, pane_id: Option<&str>, note: String| {
        RunStart::Record(run_record(at, result, pane_id.map(Into::into), note))
    };
    let Some(mut account) = ctx.account.filter(|_| ctx.project_exists) else {
        let note = if ctx.project_exists {
            "account missing"
        } else {
            "project missing"
        };
        return record(RunResult::Failed, None, note.into());
    };
    if let Some(last) = job.runs.first() {
        let busy = ctx.last_pane_status.is_some_and(|s| RUN_BUSY.contains(&s));
        if last.result == RunResult::Running.as_str() && busy {
            if let Some(pane) = last.pane_id.as_deref() {
                return record(
                    RunResult::Skipped,
                    Some(pane),
                    "previous run still going".into(),
                );
            }
        }
    }
    if ctx.at_limit {
        // A scheduled run has nobody to ask (#161): Ask first switches it on its own too.
        let other = if job.at_limit == "switch" && !ctx.auto_switch_off {
            ctx.switch_to
        } else {
            None
        };
        match other {
            Some(other) => account = other,
            None => {
                let note = format!("{} is at its limit", account.name);
                return record(RunResult::Waiting, None, note);
            }
        }
    }
    match &job.target {
        ScheduleTarget::Chat { pane_id } => RunStart::Chat {
            pane_id: pane_id.clone(),
        },
        ScheduleTarget::New => RunStart::NewChat {
            account_id: account.id.into(),
            note: if account.id == job.account_id {
                String::new()
            } else {
                format!("as {}", account.name)
            },
        },
    }
}

/// A run that started in `pane_id`.
pub fn running(at: i64, pane_id: &str, note: impl Into<String>) -> ScheduleRun {
    run_record(at, RunResult::Running, Some(pane_id.into()), note)
}

/// The target chat is neither open nor in History.
pub fn chat_gone(at: i64) -> ScheduleRun {
    run_record(at, RunResult::Failed, None, "that chat is gone")
}

/// A new chat has no folder to start in.
pub fn no_folder(at: i64) -> ScheduleRun {
    run_record(at, RunResult::Failed, None, "project has no folder")
}

/// What becomes of a job.
// Returned and consumed at once, never stored: boxing the job would only add a step.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum JobChange {
    /// Store this in its place.
    Update(ScheduledJob),
    /// Remove it (a "Send later" message that went).
    Delete,
}

/// "Run now" (off its schedule; its next run stays): what becomes of the job after `run`.
pub fn after_run_now(job: ScheduledJob, run: ScheduleRun) -> JobChange {
    if job.one_shot && run.result == RunResult::Running.as_str() {
        return JobChange::Delete;
    }
    JobChange::Update(with_run(job, run))
}

/// What one tick does with a job (after the app settled its running run).
// Returned and consumed at once, never stored: boxing the job would only add a step.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum Tick {
    /// Nothing now.
    Idle,
    /// Store this in its place: it learned its next run, or a missed run was recorded.
    Update(ScheduledJob),
    /// It is due: [`start_run`], do it, then [`after_tick_run`].
    Run,
}

/// One tick's decision for a job (`tickSchedules`, up to starting the run).
pub fn tick_job<Tz: TimeZone>(
    tz: &Tz,
    job: &ScheduledJob,
    now: i64,
    limits: Option<&Limits>,
) -> Tick {
    if !job.enabled {
        return Tick::Idle;
    }
    // A reset job learns its time once the CLI has said; a waiting one its reset.
    if job.next_at.is_none() {
        return match next_of(tz, job, now, limits) {
            Some(n) => Tick::Update(ScheduledJob {
                next_at: Some(n),
                ..job.clone()
            }),
            None => Tick::Idle,
        };
    }
    match due_action(job, now) {
        None => Tick::Idle,
        Some(DueAction::Run) => Tick::Run,
        Some(DueAction::Missed) => {
            let mut j = job.clone();
            j.next_at = next_of(tz, &j, now, limits);
            j.enabled = !matches!(j.when, ScheduleWhen::Once { .. });
            let run = run_record(now, RunResult::Missed, None, "Wraithgrid was not running");
            Tick::Update(with_run(j, run))
        }
    }
}

/// What becomes of a job a tick ran, given the run it recorded.
pub fn after_tick_run<Tz: TimeZone>(
    tz: &Tz,
    job: ScheduledJob,
    run: ScheduleRun,
    now: i64,
    limits: Option<&Limits>,
) -> JobChange {
    if run.result == RunResult::Waiting.as_str() {
        let reset = reset_of(&job, limits, now);
        // Record the wait once; try again just after the reset.
        let waiting = job
            .runs
            .first()
            .is_some_and(|r| r.result == RunResult::Waiting.as_str());
        let mut j = if waiting { job } else { with_run(job, run) };
        j.next_at = Some(reset.map_or(now + WAIT_RETRY_MS, |r| r + RESET_GRACE_MS));
        return JobChange::Update(j);
    }
    // A queued message is done once sent (or when its chat is gone).
    if job.one_shot && run.result != RunResult::Skipped.as_str() {
        return JobChange::Delete;
    }
    let once = matches!(job.when, ScheduleWhen::Once { .. });
    let mut j = job;
    j.next_at = if once {
        None
    } else {
        next_of(tz, &j, now, limits)
    };
    j.enabled = !once && j.enabled;
    JobChange::Update(with_run(j, run))
}
