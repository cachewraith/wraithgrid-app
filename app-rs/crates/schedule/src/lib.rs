//! Scheduled chats (#77) for the GPUI app (#166): when a job runs next, what to do when one is
//! due, and how its runs are recorded. Port of `src/shared/schedule.ts` and the scheduling
//! decisions of the renderer store. Pure: the jobs are core's `ScheduledJob`s, the time and time
//! zone come in as arguments, and the app carries out what these functions decide.

pub mod plan;
pub mod runs;
pub mod when;

pub use plan::*;
pub use runs::*;
pub use when::*;
