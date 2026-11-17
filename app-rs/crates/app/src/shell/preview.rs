//! Web preview panes (#74) on the window side: one view per preview pane, the project's command
//! and address saved on it, "Attach to chat" handing the report to the project's chat, and the
//! native page hidden whenever something is drawn over its pane.

use gpui_kit::*;
use wraithgrid_core::config::Pane;
use wraithgrid_core::paths::{home_dir, resolve_user_path};
use wraithgrid_core::types::PaneView;

use super::Shell;
use crate::preview_view::{PreviewView, PreviewViewEvent};

impl Shell {
    /// The pane's preview view, made the first time it is shown.
    pub(super) fn preview(
        &mut self,
        pane: &Pane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PreviewView> {
        if let Some(v) = self.previews.get(&pane.id) {
            return v.clone();
        }
        let c = self.config();
        let ws = c
            .workspaces
            .iter()
            .find(|w| w.panes.iter().any(|p| p.id == pane.id));
        let (name, command, url) = ws.map_or_else(Default::default, |w| {
            (
                w.name.clone(),
                w.preview.command.clone(),
                w.preview.url.clone(),
            )
        });
        let cwd = resolve_user_path(&pane.cwd, &home_dir());
        let view = cx.new(|cx| PreviewView::new(name, cwd, command, url, window, cx));
        let id = pane.id.clone();
        cx.subscribe_in(
            &view,
            window,
            move |this, view, e: &PreviewViewEvent, window, cx| match e {
                PreviewViewEvent::Save { command, url } => {
                    let (id, command, url) = (id.clone(), command.clone(), url.clone());
                    this.update(cx, |c| {
                        if let Some(w) = c
                            .workspaces
                            .iter_mut()
                            .find(|w| w.panes.iter().any(|p| p.id == id))
                        {
                            w.preview.command = command.chars().take(1000).collect();
                            w.preview.url = url;
                        }
                    });
                }
                PreviewViewEvent::Attach { report, .. } => {
                    let error = this.attach_to_chat(&id, report, window, cx);
                    view.update(cx, |v, cx| v.set_attach_error(error, cx));
                }
                PreviewViewEvent::Close => this.close_pane(id.clone(), cx),
            },
        )
        .detach();
        self.previews.insert(pane.id.clone(), view.clone());
        view
    }

    /// `previewAttach`: the report goes into the project's last chat (a new one when it has
    /// none), unsent. An error to show, or None.
    fn attach_to_chat(
        &mut self,
        preview_id: &str,
        report: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let c = self.config().clone();
        let Some(ws) = c
            .workspaces
            .iter()
            .find(|w| w.panes.iter().any(|p| p.id == preview_id))
        else {
            return Some("That preview is gone.".into());
        };
        let chats: Vec<&Pane> = ws
            .panes
            .iter()
            .filter(|p| p.view == PaneView::Chat && !self.as_terminal.contains(&p.id))
            .collect();
        let focused = self
            .focused
            .as_ref()
            .and_then(|f| chats.iter().find(|p| &p.id == f));
        let target = match focused.or(chats.last()) {
            Some(p) => (*p).clone(),
            None => {
                let account = ws
                    .account_id
                    .as_deref()
                    .or(c.settings.default_account_id.as_deref())
                    .and_then(|id| c.account(id))
                    .or_else(|| c.accounts.iter().find(|a| a.cli.spec().chat && a.signed_in))?;
                let cwd = ws.project_folder().unwrap_or_else(|| {
                    ws.panes
                        .iter()
                        .find(|p| p.id == preview_id)
                        .map(|p| p.cwd.clone())
                        .unwrap_or_default()
                });
                let id = self.add_pane(
                    Some(ws.id.clone()),
                    Some(account.id.clone()),
                    cwd,
                    vec![],
                    false,
                    true,
                    cx,
                );
                match self
                    .config()
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.panes)
                    .find(|p| p.id == id)
                {
                    Some(p) => p.clone(),
                    None => return Some("Add an account to chat with first.".into()),
                }
            }
        };
        let chat = self.chat(&target, window, cx);
        chat.update(cx, |v, cx| v.add_to_draft(report, window, cx));
        self.focused = Some(target.id.clone());
        cx.notify();
        None
    }

    /// `openPreview`: the project's preview pane, made when it has none.
    pub(super) fn open_preview(
        &mut self,
        ws_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let c = self.config();
        let Some(ws) = c.workspaces.iter().find(|w| w.id == ws_id) else {
            return;
        };
        // Preview runs on this computer only (H5).
        if ws.machine_id.is_some() {
            return;
        }
        if let Some(p) = ws.panes.iter().find(|p| p.view == PaneView::Preview) {
            let (ws, p) = (ws.id.clone(), p.id.clone());
            return self.reveal_chat(&ws, &p, window, cx);
        }
        let Some(cwd) = ws.project_folder() else {
            // No folder yet: the project page asks for one (Edit project).
            let ws = ws.id.clone();
            return self.open_project(ws, window, cx);
        };
        let ws = ws.id.clone();
        if self.config().active_workspace != ws {
            self.select_workspace(ws.clone(), cx);
        }
        let id = self.add_pane(Some(ws), None, cwd, vec![], true, false, cx);
        self.update(cx, |c| {
            if let Some(p) = c
                .workspaces
                .iter_mut()
                .flat_map(|w| &mut w.panes)
                .find(|p| p.id == id)
            {
                p.view = PaneView::Preview;
                p.title = "Preview".into();
            }
        });
    }

    /// A native page draws over whatever is above it: shown only while its pane is in the
    /// grid and no screen, dialog or menu is open.
    pub(super) fn sync_preview_visibility(&mut self, shown: &[String], cx: &mut Context<Self>) {
        let covered = self.modal.is_some()
            || self.settings_view.is_some()
            || self.accounts_view.is_some()
            || self.usage_view.is_some()
            || self.mods_view.is_some()
            || self.scheduled_view.is_some()
            || self.project_view.is_some()
            || self.sidebar_popup_open()
            || self.git_menu.is_some()
            || self.pane_menu.is_some()
            || self.utility_menu.is_some()
            || self.closing.is_some();
        let views: Vec<(String, Entity<PreviewView>)> = self
            .previews
            .iter()
            .map(|(id, v)| (id.clone(), v.clone()))
            .collect();
        for (id, v) in views {
            let visible = !covered && shown.contains(&id);
            v.update(cx, |v, cx| v.set_visible(visible, cx));
        }
    }
}
