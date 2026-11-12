//! What to do with a job now and how its runs are kept (#77): due or missed, the run history,
//! and when a `running` run is over. Port of the run half of `src/shared/schedule.ts`; pure, so
//! the app's tick asks and then acts.

use wraithgrid_core::config::{ScheduleRun, ScheduledJob};
pub use wraithgrid_core::types::{SCHEDULES_MAX, SCHEDULE_RUNS_MAX};

/// A run found later than this (asleep, app closed) is recorded as missed, not run.
pub const MISSED_AFTER_MS: i64 = 60 * 60 * 1000;
/// A run's chat must stay quiet this long after the send before the run counts as done.
pub const SETTLE_AFTER_MS: i64 = 10_000;

/// How a run went; `ScheduleRun::result` holds it as its string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunResult {
    Running,
    Done,
    Failed,
    Missed,
    Skipped,
    Waiting,
}

impl RunResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Missed => "missed",
            Self::Skipped => "skipped",
            Self::Waiting => "waiting",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "missed" => Self::Missed,
            "skipped" => Self::Skipped,
            "waiting" => Self::Waiting,
            _ => return None,
        })
    }
}

/// A run record.
pub fn run_record(
    at: i64,
    result: RunResult,
    pane_id: Option<String>,
    note: impl Into<String>,
) -> ScheduleRun {
    ScheduleRun {
        at,
        result: result.as_str().into(),
        pane_id,
        note: note.into(),
    }
}

/// What to do with a due job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DueAction {
    Run,
    /// Over [`MISSED_AFTER_MS`] late: record it as missed instead.
    Missed,
}

/// What to do with a job now: run it, record a missed run, or nothing yet (None).
pub fn due_action(job: &ScheduledJob, now: i64) -> Option<DueAction> {
    let next = job.next_at.filter(|n| job.enabled && now >= *n)?;
    Some(if now - next > MISSED_AFTER_MS {
        DueAction::Missed
    } else {
        DueAction::Run
    })
}

/// The job with a run added, keeping the newest [`SCHEDULE_RUNS_MAX`].
pub fn with_run(mut job: ScheduledJob, run: ScheduleRun) -> ScheduledJob {
    job.runs.insert(0, run);
    job.runs.truncate(SCHEDULE_RUNS_MAX);
    job
}

/// Where a run's chat is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneWhere {
    Open,
    /// Closed to the project's History.
    History,
    Gone,
}

/// What the app sees of a running run's chat when it checks whether the run is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunView {
    /// Started by this launch (a run left `running` by an earlier one is stale, #117).
    pub live: bool,
    pub pane: PaneWhere,
    pub exited: bool,
    /// Its message was sent and the chat is idle again (not busy, no background work).
    pub quiet: bool,
    pub now: i64,
}

/// How a running run ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settle {
    pub result: RunResult,
    pub note: String,
    /// Say "Scheduled run done" out loud.
    pub notify: bool,
}

/// How a `running` run ends, or None while it goes on. Only a run this launch started can say
/// "done" out loud: a stale one is settled quietly, so later chat activity never finishes it.
pub fn settle_of(run: &ScheduleRun, v: &RunView) -> Option<Settle> {
    if run.result != RunResult::Running.as_str() || run.pane_id.is_none() {
        return None;
    }
    let end = |result, note: &str, notify| {
        Some(Settle {
            result,
            note: note.into(),
            notify,
        })
    };
    match v.pane {
        PaneWhere::Gone => end(RunResult::Failed, "that chat is gone", false),
        PaneWhere::History => end(RunResult::Done, &run.note, false),
        PaneWhere::Open if !v.live => end(
            RunResult::Failed,
            "interrupted: Wraithgrid closed during the run",
            false,
        ),
        PaneWhere::Open if v.exited => end(RunResult::Failed, &run.note, false),
        PaneWhere::Open if v.quiet && v.now - run.at > SETTLE_AFTER_MS => {
            end(RunResult::Done, &run.note, true)
        }
        PaneWhere::Open => None,
    }
}

/// The job with its newest run settled as `end` says.
pub fn settled(mut job: ScheduledJob, end: &Settle) -> ScheduledJob {
    if let Some(run) = job.runs.first_mut() {
        run.result = end.result.as_str().into();
        run.note = end.note.clone();
    }
    job
}

/// The soonest run of any enabled job, for the app to wake a closed window in time.
pub fn soonest_run(jobs: &[ScheduledJob]) -> Option<i64> {
    jobs.iter()
        .filter(|j| j.enabled)
        .filter_map(|j| j.next_at)
        .min()
}
