//! A chat continued in another CLI (#78, `chatSwitchAccount` across CLIs): the conversation
//! can't move between CLIs, so its visible text goes as the first message of a new chat in the
//! same place; the old chat is kept in the project's History. A CLI without a chat view gets a
//! terminal with that account instead.

use gpui_kit::*;
use wraithgrid_chat::compose::build_handoff;
use wraithgrid_chat::ItemLevel;
use wraithgrid_core::config::{ArchivedChat, Pane};
use wraithgrid_core::ids::new_id;
use wraithgrid_core::layout::swap_panes;
use wraithgrid_core::types::{PaneView, HISTORY_MAX};

use super::Shell;

impl Shell {
    pub(super) fn continue_in_cli(
        &mut self,
        pane_id: &str,
        to_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let c = self.config().clone();
        let Some((ws_id, pane)) = c.workspaces.iter().find_map(|w| {
            w.panes
                .iter()
                .find(|p| p.id == pane_id)
                .map(|p| (w.id.clone(), p.clone()))
        }) else {
            return;
        };
        let from = pane.account_id.as_deref().and_then(|id| c.account(id));
        let (Some(from), Some(to)) = (from, c.account(to_id)) else {
            return self.chat_notice(pane_id, "That account is gone.", cx);
        };
        if !to.signed_in {
            return self.chat_notice(pane_id, &format!("{} is not signed in yet.", to.name), cx);
        }
        let chat = self.chats.get(pane_id).cloned();
        if chat.as_ref().is_some_and(|v| v.read(cx).is_busy()) {
            return self.chat_notice(
                pane_id,
                "Wait for the answer to finish, or stop it first.",
                cx,
            );
        }
        // No chat view in that CLI: a terminal with the account, beside the chat.
        if !to.cli.spec().chat {
            self.add_pane(
                Some(ws_id),
                Some(to.id.clone()),
                pane.cwd.clone(),
                vec![],
                false,
                false,
                cx,
            );
            return;
        }
        let items = chat
            .as_ref()
            .map(|v| v.read(cx).items().to_vec())
            .unwrap_or_default();
        let (text, messages) = build_handoff(
            &items,
            from.cli.spec().provider_label,
            &from.name,
            20_000,
            60,
        );
        let id = new_id("p");
        let next = Pane {
            id: id.clone(),
            account_id: Some(to.id.clone()),
            args: vec![],
            view: PaneView::Chat,
            resume: false,
            has_conversation: true,
            chat_session_id: None,
            model: None,
            effort: None,
            chat_mode: None,
            ..pane.clone()
        };
        // The old chat keeps its saved view and session under its own id, in History.
        if let Some(v) = self.chats.remove(pane_id) {
            v.update(cx, |v, _| {
                v.save();
                v.stop_session();
            });
        }
        let now = wraithgrid_usage::now_ms();
        let (old, fresh) = (pane.clone(), next.clone());
        self.update(cx, |c| {
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == ws_id) {
                if let Some(p) = w.panes.iter_mut().find(|p| p.id == old.id) {
                    *p = fresh.clone();
                }
                w.layout = w.layout.as_ref().map(|l| swap_panes(l, &old.id, &fresh.id));
                w.history.insert(
                    0,
                    ArchivedChat {
                        pane: old.clone(),
                        closed_at: now,
                    },
                );
                w.history.truncate(HISTORY_MAX);
            }
        });
        if self.focused.as_deref() == Some(pane_id) {
            self.focused = Some(id.clone());
        }
        if self.zoomed.as_deref() == Some(pane_id) {
            self.zoomed = Some(id.clone());
        }
        let divider = format!(
            "continued in {} as {} · {}",
            to.cli.spec().provider_label,
            to.name,
            super::chrono_like_time()
        );
        let (from_cli, to_cli) = (from.cli.as_str(), to.cli.as_str());
        let view = self.chat(&next, window, cx);
        view.update(cx, |v, cx| {
            v.start_handoff(&text, messages, from_cli, to_cli, &divider, cx)
        });
        cx.notify();
    }

    fn chat_notice(&mut self, pane_id: &str, text: &str, cx: &mut Context<Self>) {
        if let Some(v) = self.chats.get(pane_id).cloned() {
            v.update(cx, |v, cx| v.add_notice(ItemLevel::Error, text, cx));
        }
    }
}
