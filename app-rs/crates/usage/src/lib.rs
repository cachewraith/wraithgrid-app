//! What chats used and how close each account is to its plan limits: the usage model and its
//! summaries (`model`), usage.json (`store`), the live claude limits poll (`poll`) and the
//! switch-account-at-the-limit decision (`auto_switch`). A pure library: the app injects the
//! clock, file paths and HTTP, and drives the save and poll timers itself.

pub mod auto_switch;
pub mod format;
pub mod model;
pub mod poll;
pub mod store;

use std::path::PathBuf;

pub use auto_switch::{
    decide_switch, is_limit_error, next_account, peak_use, LimitWindow, PeakUse, SwitchDecision,
    CONTINUE_AS_CLIS,
};
pub use model::{
    add_turn, day_key, day_key_before, has_cost, live_window, parse_usage_data, prune_usage,
    summarize_usage, AccountLimits, AccountUsage, ChatUsage, DailyCost, ProjectUsage, Turn,
    TurnUsage, UsageBucket, UsageData, UsageRange, UsageSummary, UsageTotals, UsageWindow,
    USAGE_DAYS,
};
pub use poll::{
    parse_oauth_usage, read_claude_token, ureq_fetch, FetchRequest, FetchResponse, OAuthToken,
    UsagePollDeps, UsagePoller, BACKOFF_MS, FETCH_TIMEOUT, POLL_MS, USAGE_URL,
};
pub use store::{write_file_atomic, PaneOwner, UsageEvent, UsageStore, UsageStoreDeps};

/// The parts of an account usage needs. The app builds these from its config, so this crate
/// does not depend on the core types.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageAccount {
    pub id: String,
    pub name: String,
    /// The agent CLI its panes run (`claude`, `codex`, …).
    pub cli: String,
    pub signed_in: bool,
    /// Its config dir, `~` already resolved. Only the poll reads it.
    pub config_dir: PathBuf,
}

/// The system clock in ms since the epoch, as `Date.now()`.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
