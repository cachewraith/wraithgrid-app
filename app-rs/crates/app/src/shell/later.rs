//! "Send later" (#77) on the window side: a chat's queued message becomes a one-shot job, and
//! each chat is told its queued messages and when its account's limit resets.

use gpui_kit::*;
use wraithgrid_core::config::{ScheduleTarget, ScheduleWhen};
use wraithgrid_core::ids::new_id;
use wraithgrid_schedule::{fmt_when, SendLater};
use wraithgrid_usage::format::fmt_until;
use wraithgrid_usage::model::live_window;

use super::Shell;
use crate::chat_view::Queued;

impl Shell {
    /// `queueMessage`: the text goes to this chat at the time, or when its limit resets.
    pub(super) fn queue_message(
        &mut self,
        pane_id: &str,
        text: &str,
        when: SendLater,
        cx: &mut Context<Self>,
    ) {
        let c = self.config();
        let Some((ws, pane)) = c
            .workspaces
            .iter()
            .find_map(|w| w.panes.iter().find(|p| p.id == pane_id).map(|p| (w, p)))
        else {
            return;
        };
        let Some(account) = pane.account_id.clone() else {
            return;
        };
        let tz = chrono::Local;
        let limits = self.job_limits(&account);
        let Some(job) = wraithgrid_schedule::queue_message(
            &tz,
            new_id("j"),
            &ws.id,
            &account,
            pane_id,
            text,
            when,
            limits.as_ref(),
        ) else {
            return;
        };
        let now = wraithgrid_usage::now_ms();
        self.update(cx, |c| {
            wraithgrid_schedule::save_job(&tz, &mut c.schedules, job, now, limits.as_ref());
        });
    }

    /// Each chat's queued messages, and when its account's fuller window resets.
    pub(super) fn refresh_chat_later(&mut self, cx: &mut Context<Self>) {
        let now = wraithgrid_usage::now_ms();
        let tz = chrono::Local;
        let c = self.config().clone();
        let chats: Vec<(String, Entity<crate::chat_view::ChatView>)> = self
            .chats
            .iter()
            .map(|(id, v)| (id.clone(), v.clone()))
            .collect();
        for (pane_id, view) in chats {
            let account = c
                .workspaces
                .iter()
                .flat_map(|w| &w.panes)
                .find(|p| p.id == pane_id)
                .and_then(|p| p.account_id.as_deref())
                .and_then(|id| c.account(id));
            let queued: Vec<Queued> = c
                .schedules
                .iter()
                .filter(|j| {
                    j.one_shot
                        && matches!(&j.target, ScheduleTarget::Chat { pane_id: p } if *p == pane_id)
                })
                .map(|j| Queued {
                    id: j.id.clone(),
                    prompt: j.prompt.clone(),
                    when: match (&j.when, j.next_at) {
                        (ScheduleWhen::Reset { .. }, at) => format!(
                            "when {} limit resets{}",
                            account.map_or("the", |a| a.name.as_str()),
                            at.map_or(String::new(), |t| format!(" · {}", fmt_until(t, now)))
                        ),
                        (_, Some(t)) => {
                            format!("at {} · {}", fmt_when(&tz, t, now), fmt_until(t, now))
                        }
                        (_, None) => "queued".into(),
                    },
                })
                .collect();
            let reset = account.and_then(|a| {
                let l = self.limits.get(&a.id)?;
                let at = [l.five_hour, l.seven_day]
                    .into_iter()
                    .filter_map(|w| live_window(w, now))
                    .filter(|w| w.utilization > 0.0 && w.resets_at > now)
                    .map(|w| w.resets_at)
                    .min()?;
                Some((
                    format!("When {}'s limit resets", a.name),
                    fmt_until(at, now),
                ))
            });
            view.update(cx, |v, cx| v.set_later(queued, reset, cx));
        }
    }
}
