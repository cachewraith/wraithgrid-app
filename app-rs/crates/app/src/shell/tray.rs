//! Keep in tray (`keepInTray`, `refreshTray`, `reopenWindow` in app.ts): "Keep running" closes
//! the window but not the app, so chats mid-turn (they run in this process) and scheduled jobs
//! carry on; the tray icon says how many agents run and brings the window back. The shell
//! outlives its window and is attached again to the next one.

use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::*;

use super::Shell;
use crate::tray::{Tray, TrayCommand};

impl Shell {
    /// "Keep running" from the close button. Without a tray (other systems for now) the app
    /// quits with agents kept, as before.
    pub(super) fn keep_in_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let Some(tray) = Tray::show(tx) else {
            return self.quit(true, cx);
        };
        self.flush();
        self.flush_chats(cx);
        // Screens and dialogs belong to the window that goes.
        self.modal = None;
        self.settings_view = None;
        self.accounts_view = None;
        self.usage_view = None;
        self.mods_view = None;
        self.scheduled_view = None;
        self.project_view = None;
        self.changes = None;
        self.tray = Some(tray);
        let commands = cx.spawn(async move |this, cx| {
            while let Some(c) = rx.next().await {
                if this
                    .update(cx, |this, cx| this.tray_command(c, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let refresh = cx.spawn(async move |this, cx| loop {
            if this.update(cx, |this, cx| this.refresh_tray(cx)).is_err() {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(5)).await;
        });
        self._tray_tasks = vec![commands, refresh];
        window.remove_window();
    }

    pub fn in_tray(&self) -> bool {
        self.tray.is_some()
    }

    fn tray_command(&mut self, c: TrayCommand, cx: &mut Context<Self>) {
        match c {
            TrayCommand::Open => self.reopen_window(cx),
            TrayCommand::StopAndQuit => self.quit(false, cx),
            TrayCommand::QuitKeep => self.quit(true, cx),
        }
    }

    /// Terminal agents in the host plus chats mid-turn (`runningAgents`); a job due within
    /// 10 s brings the window back to run it; with nothing left to keep, the app quits.
    fn refresh_tray(&mut self, cx: &mut Context<Self>) {
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let live = cx.background_spawn(async move { host.live_count() }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(tray) = &this.tray else { return };
                let busy = this.chats.values().filter(|c| c.read(cx).is_busy()).count();
                let live = live + busy;
                tray.set_live(live);
                let jobs = &this.config().schedules;
                let now = wraithgrid_usage::now_ms();
                // Electron opens a hidden window for it; GPUI can't hide one on Linux.
                if wraithgrid_schedule::soonest_run(jobs).is_some_and(|t| t <= now + 10_000) {
                    return this.reopen_window(cx);
                }
                if live == 0 && !jobs.iter().any(|j| j.enabled) {
                    this.quit(false, cx);
                }
            });
        })
        .detach();
    }

    /// `reopenWindow`: the tray goes and the window comes back with this shell in it.
    pub(super) fn reopen_window(&mut self, cx: &mut Context<Self>) {
        if self.tray.take().is_none() {
            return;
        }
        self._tray_tasks.clear();
        let shell = cx.entity();
        cx.defer(move |cx| crate::open_main_window(shell, cx));
    }

    /// A window was opened for this shell: what runs per window starts again in it, and the
    /// chat views kept from the last one listen in this one.
    pub fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let resolved = crate::theme::resolve(self.config(), crate::theme::system_dark(window));
        crate::theme::apply(&resolved, Some(window), cx);
        self.start_polling(window, cx);
        self.start_schedules(window, cx);
        if !self.opened {
            self.opened = true;
            return self.open_from_env(window, cx);
        }
        // Idle chats are made again from their saved views (they resume on the next message);
        // busy ones keep their turn going and are listened to here.
        let idle: Vec<String> = self
            .chats
            .iter()
            .filter(|(_, v)| !v.read(cx).is_busy())
            .map(|(id, _)| id.clone())
            .collect();
        for id in idle {
            if let Some(v) = self.chats.remove(&id) {
                v.update(cx, |c, _| {
                    c.save();
                    c.stop_session();
                });
            }
        }
        let busy: Vec<(String, Entity<crate::chat_view::ChatView>)> = self
            .chats
            .iter()
            .map(|(id, v)| (id.clone(), v.clone()))
            .collect();
        for (id, view) in busy {
            self.subscribe_chat(&view, id, window, cx);
        }
        cx.notify();
    }

    /// Wraithgrid launched again (`second-instance`): show this window, from the tray too.
    pub fn listen_for_second_launch(
        &mut self,
        rx: std::sync::mpsc::Receiver<()>,
        cx: &mut Context<Self>,
    ) {
        let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
        self._second_launch = Some(cx.spawn(async move |this, cx| loop {
            let rx = rx.clone();
            let got = cx
                .background_spawn(async move { rx.lock().unwrap().recv().is_ok() })
                .await;
            if !got {
                break;
            }
            let shown = this.update(cx, |this, cx| {
                if this.in_tray() {
                    return this.reopen_window(cx);
                }
                cx.activate(true);
                if let Some(w) = cx.windows().first().copied() {
                    let _ = w.update(cx, |_, window, _| window.activate_window());
                }
            });
            if shown.is_err() {
                break;
            }
        }));
    }
}
