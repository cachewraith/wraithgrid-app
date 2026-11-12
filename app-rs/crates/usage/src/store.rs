//! Records chat-turn usage and plan limits as the chat events pass through the app, into
//! userData/usage.json (90 days, written a moment after the last change). Nothing is sent
//! anywhere: the CLI reported these numbers, or the app polled the limits (poll.rs).
//! Port of `src/main/usage-store.ts`. No timer of its own: the app calls `tick` on its timer
//! (or at `save_due_at`) and `flush` on quit.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::model::{
    add_turn, parse_usage_data, prune_usage, AccountLimits, Turn, TurnUsage, UsageData,
};
use crate::now_ms;

/// Who a pane belongs to right now, from the app's config copy.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneOwner {
    pub account_id: Option<String>,
    pub workspace_id: Option<String>,
    pub title: String,
}

/// The two chat events usage cares about (ChatEvent's `limits` and `turnEnd`).
#[derive(Debug, Clone, PartialEq)]
pub enum UsageEvent {
    Limits(AccountLimits),
    /// None: the CLI's result carried no usage.
    TurnEnd(Option<TurnUsage>),
}

pub type OwnerFn = Box<dyn Fn(&str) -> Option<PaneOwner> + Send>;
pub type ClockFn = Box<dyn Fn() -> i64 + Send>;
pub type WriteFn = Box<dyn FnMut(&Path, &str) -> io::Result<()> + Send>;

pub struct UsageStoreDeps {
    pub file: PathBuf,
    /// Who a pane belongs to right now, from the app's config copy.
    pub owner: OwnerFn,
    pub now: ClockFn,
    pub save_delay_ms: i64,
    pub write: WriteFn,
}

impl UsageStoreDeps {
    /// The real clock, a 1.5 s save delay and an atomic write.
    pub fn new(file: impl Into<PathBuf>, owner: OwnerFn) -> Self {
        Self {
            file: file.into(),
            owner,
            now: Box::new(now_ms),
            save_delay_ms: 1500,
            write: Box::new(write_file_atomic),
        }
    }
}

pub struct UsageStore {
    deps: UsageStoreDeps,
    data: UsageData,
    /// When the pending save is due (ms); None when nothing is pending.
    save_at: Option<i64>,
}

impl UsageStore {
    pub fn new(deps: UsageStoreDeps) -> Self {
        Self {
            deps,
            data: UsageData::default(),
            save_at: None,
        }
    }

    pub fn load(&mut self) {
        // No file yet, or a broken one: start empty (the next save replaces it).
        let Ok(text) = fs::read_to_string(&self.deps.file) else {
            return;
        };
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        if let Some(mut data) = parse_usage_data(&raw) {
            prune_usage(&mut data, (self.deps.now)());
            self.data = data;
        }
    }

    pub fn get(&self) -> UsageData {
        let mut d = self.data.clone();
        prune_usage(&mut d, (self.deps.now)());
        d
    }

    /// An account's limits from elsewhere than a chat turn (the poll).
    pub fn set_limits(&mut self, account_id: &str, limits: AccountLimits) {
        self.data.limits.insert(account_id.to_owned(), limits);
        self.schedule_save();
    }

    pub fn on_chat_event(&mut self, pane_id: &str, e: &UsageEvent) {
        match e {
            UsageEvent::Limits(limits) => {
                let owner = (self.deps.owner)(pane_id);
                let Some(account_id) = owner.and_then(|o| o.account_id) else {
                    return;
                };
                if account_id.is_empty() {
                    return;
                }
                self.data.limits.insert(account_id, *limits);
                self.schedule_save();
            }
            UsageEvent::TurnEnd(Some(usage)) => {
                let owner = (self.deps.owner)(pane_id);
                let (account_id, workspace_id, title) = match owner {
                    Some(o) => (o.account_id, o.workspace_id, o.title),
                    None => (None, None, String::new()),
                };
                let turn = Turn {
                    at: (self.deps.now)(),
                    account_id,
                    workspace_id,
                    pane_id: pane_id.to_owned(),
                    title,
                    usage: usage.clone(),
                };
                add_turn(&mut self.data, &turn);
                self.schedule_save();
            }
            UsageEvent::TurnEnd(None) => {}
        }
    }

    /// When the pending save is due (ms), for an app that sets a one-shot timer.
    pub fn save_due_at(&self) -> Option<i64> {
        self.save_at
    }

    /// Writes if a pending save is due. The app calls this on its timer.
    pub fn tick(&mut self) {
        if self.save_at.is_some_and(|at| at <= (self.deps.now)()) {
            self.save_at = None;
            self.save();
        }
    }

    /// Writes now if a save is pending (quit).
    pub fn flush(&mut self) {
        if self.save_at.take().is_some() {
            self.save();
        }
    }

    fn schedule_save(&mut self) {
        if self.save_at.is_none() {
            self.save_at = Some((self.deps.now)() + self.deps.save_delay_ms);
        }
    }

    fn save(&mut self) {
        let text = match serde_json::to_string(&self.get()) {
            Ok(t) => t,
            Err(_) => return,
        };
        // A failed write is dropped, as in TS: the next change tries again.
        let _ = (self.deps.write)(&self.deps.file, &text);
    }
}

/// Writes through a temp file and a rename, so a crash never leaves half a file
/// (`writeFileAtomic` in config-store.ts). Owner-only on Unix.
pub fn write_file_atomic(file: &Path, data: &str) -> io::Result<()> {
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(format!(".{}.{}.tmp", std::process::id(), now_ms()));
    let tmp = PathBuf::from(tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    {
        let mut f = opts.open(&tmp)?;
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
    }
    // Windows refuses the rename while antivirus or the indexer holds the target open.
    let mut attempt = 0;
    loop {
        match fs::rename(&tmp, file) {
            Ok(()) => return Ok(()),
            Err(e)
                if attempt < 5
                    && matches!(
                        e.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::ResourceBusy
                    ) =>
            {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(50 * attempt));
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
}
