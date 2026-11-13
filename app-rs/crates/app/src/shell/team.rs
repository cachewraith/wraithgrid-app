//! The window's side of a project's team (#79, `store.ts` `saveTeamAgent`/`startTeam`/
//! `stopTeam`): saving agents, starting them as chats titled with their role (each on its own
//! `team/<role>` worktree, or in the project folder) with their first message, laying the team
//! out as a grid, and stopping it. Asks first when an agent may not run or is still working.

use std::time::Duration;

use gpui_kit::*;
use wraithgrid_chat::ItemLevel;
use wraithgrid_core::config::{Pane, TeamAgent};
use wraithgrid_core::layout::auto_grid;
use wraithgrid_core::paths::{contract_home, home_dir, resolve_user_path, worktrees_root};
use wraithgrid_core::types::{PaneView, TEAM_MAX};
use wraithgrid_git::{add_worktree, GitCli, WorktreeFrom};

use super::{Modal, Shell};
use crate::dialogs::{ChoiceDialog, ChoiceEvent};
use crate::project_view::ProjectView;
use crate::team_section::{
    plan_team, team_branch, team_pane_ids, team_warnings, TeamEvent, TeamStep,
};

impl Shell {
    /// Hooks the project page's team section to the window; its usage figures follow the
    /// window's while the page is open.
    pub(super) fn wire_team(
        &mut self,
        view: &Entity<ProjectView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let team = view.read(cx).team();
        let limits = self.limits.clone();
        team.update(cx, |t, cx| t.set_limits(limits, cx));
        cx.subscribe_in(
            &team,
            window,
            |this, _, e: &TeamEvent, window, cx| match e {
                TeamEvent::Save(ws, agent) => this.save_team_agent(ws, agent.clone(), cx),
                TeamEvent::Remove(ws, id) => this.remove_team_agent(ws, id, cx),
                TeamEvent::Start(ws, only) => {
                    this.start_team(ws.clone(), only.clone(), false, window, cx)
                }
                TeamEvent::Stop(ws) => this.stop_team(ws.clone(), false, window, cx),
            },
        )
        .detach();
        let weak = team.downgrade();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let Some(team) = weak.upgrade() else { break };
            let Ok(limits) = this.update(cx, |this, _| this.limits.clone()) else {
                break;
            };
            team.update(cx, |t, cx| t.set_limits(limits, cx));
        })
        .detach();
    }

    fn team_pane(&self, id: &str) -> Option<Pane> {
        self.config()
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .find(|p| p.id == id)
            .cloned()
    }

    /// `saveTeamAgent`: adds or replaces an agent (it applies from its next start).
    fn save_team_agent(&mut self, ws_id: &str, agent: TeamAgent, cx: &mut Context<Self>) {
        self.update(cx, |c| {
            let Some(w) = c.workspaces.iter_mut().find(|w| w.id == ws_id) else {
                return;
            };
            match w.team.iter().position(|a| a.id == agent.id) {
                Some(at) => w.team[at] = agent,
                None if w.team.len() < TEAM_MAX => w.team.push(agent),
                None => {}
            }
        });
    }

    fn remove_team_agent(&mut self, ws_id: &str, agent_id: &str, cx: &mut Context<Self>) {
        self.update(cx, |c| {
            if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == ws_id) {
                w.team.retain(|a| a.id != agent_id);
            }
        });
    }

    /// `startTeam`: opens the team's agents as chats (`only`: one of them), showing the open
    /// ones; asks first if an agent's account is missing or at its limit (`force` skips that).
    pub(super) fn start_team(
        &mut self,
        ws_id: String,
        only: Option<String>,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.blocked.is_some() {
            return;
        }
        let c = self.config().clone();
        let Some(ws) = c.workspaces.iter().find(|w| w.id == ws_id) else {
            return;
        };
        let Some(base) = ws.project_folder() else {
            // No folder anywhere: ask for one (saved on the project) rather than start in ~.
            let pick = cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: None,
            });
            cx.spawn_in(window, async move |this, cx| {
                let Ok(Ok(Some(paths))) = pick.await else {
                    return;
                };
                let Some(dir) = paths.into_iter().next() else {
                    return;
                };
                let _ = this.update_in(cx, |this, window, cx| {
                    let home = home_dir().to_string_lossy().into_owned();
                    let folder = contract_home(&dir.to_string_lossy(), &home);
                    let id = ws_id.clone();
                    this.update(cx, |c| {
                        if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == id) {
                            w.folder = folder;
                        }
                    });
                    this.start_team(ws_id, only, force, window, cx);
                });
            })
            .detach();
            return;
        };
        let team: Vec<&TeamAgent> = ws
            .team
            .iter()
            .filter(|a| only.as_ref().is_none_or(|o| &a.id == o))
            .collect();
        let steps = plan_team(&team, &c.accounts, &ws.panes);
        let warnings = team_warnings(&steps, &self.limits, wraithgrid_usage::now_ms());
        if !force && !warnings.is_empty() {
            let lines: Vec<String> = warnings
                .iter()
                .map(|(a, text)| {
                    let account = a
                        .account_id
                        .as_deref()
                        .and_then(|id| c.account(id))
                        .map_or(String::new(), |x| format!(" · {}", x.name));
                    format!("{}{account}: {text}", a.role)
                })
                .collect();
            let message = format!("Some agents may not run:\n{}", lines.join("\n"));
            let dialog = cx.new(|_| {
                ChoiceDialog::new(
                    "Start the team?",
                    message,
                    vec![("start".into(), "Start anyway".into(), String::new())],
                    "Cancel",
                )
            });
            cx.subscribe_in(
                &dialog,
                window,
                move |this, _, e: &ChoiceEvent, window, cx| {
                    this.close_modal(window, cx);
                    if let ChoiceEvent::Pick(_) = e {
                        this.start_team(ws_id.clone(), only.clone(), true, window, cx);
                    }
                },
            )
            .detach();
            self.modal = Some(Modal::Choice(dialog));
            return cx.notify();
        }
        let starts: Vec<TeamAgent> = steps
            .iter()
            .filter_map(|s| match s {
                TeamStep::Start(a, _) => Some((*a).clone()),
                _ => None,
            })
            .collect();
        self.project_view = None;
        if c.active_workspace != ws_id {
            self.select_workspace(ws_id.clone(), cx);
        }
        if starts.iter().all(|a| a.folder != "worktree") {
            let cwds = starts.iter().map(|_| (base.clone(), None)).collect();
            return self.open_team_agents(&ws_id, only, starts, cwds, window, cx);
        }
        // Branch folders first (git can be slow); then the panes, in the team's order.
        let folders: Vec<(bool, String)> = starts
            .iter()
            .map(|a| (a.folder == "worktree", team_branch(&a.role)))
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let cwds = cx
                .background_spawn(async move {
                    let home = home_dir();
                    let (git, root) = (GitCli::default(), worktrees_root(&home));
                    let dir = resolve_user_path(&base, &home);
                    folders
                        .into_iter()
                        .map(|(worktree, branch)| {
                            if !worktree {
                                return (base.clone(), None);
                            }
                            match add_worktree(&git, &dir, &branch, &root, true, WorktreeFrom::New)
                            {
                                Ok(d) => (
                                    contract_home(
                                        &d.to_string_lossy(),
                                        &home.to_string_lossy(),
                                    ),
                                    None,
                                ),
                                Err(e) => (
                                    base.clone(),
                                    Some(format!(
                                        "No branch folder ({e}); this agent runs in the project folder."
                                    )),
                                ),
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.open_team_agents(&ws_id, only, starts, cwds, window, cx)
            });
        })
        .detach();
    }

    /// Adds each agent's chat (role as its title, its model/effort/mode, its first message),
    /// then lays the whole team out by its size and focuses the first (or `only`) agent.
    fn open_team_agents(
        &mut self,
        ws_id: &str,
        only: Option<String>,
        agents: Vec<TeamAgent>,
        cwds: Vec<(String, Option<String>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (agent, (cwd, note)) in agents.into_iter().zip(cwds) {
            let id = self.add_pane(
                Some(ws_id.to_string()),
                agent.account_id.clone(),
                cwd,
                vec![],
                false,
                true,
                cx,
            );
            self.update(cx, |c| {
                if let Some(p) = c
                    .workspaces
                    .iter_mut()
                    .flat_map(|w| &mut w.panes)
                    .find(|p| p.id == id)
                {
                    p.title = agent.role.clone();
                    p.model = agent.model.clone();
                    p.effort = agent.effort;
                    p.chat_mode = agent.chat_mode;
                    p.team_agent_id = Some(agent.id.clone());
                }
            });
            let Some(pane) = self.team_pane(&id) else {
                continue;
            };
            if pane.view != PaneView::Chat {
                continue;
            }
            let chat = self.chat(&pane, window, cx);
            if let Some(note) = note {
                chat.update(cx, |v, cx| v.add_notice(ItemLevel::Info, &note, cx));
            }
            let prompt = agent.prompt.trim().to_string();
            if !prompt.is_empty() {
                chat.update(cx, |v, cx| v.resend(&prompt, window, cx));
            }
        }
        let ws = self.config().workspaces.iter().find(|w| w.id == ws_id);
        let ids = ws.map_or(vec![], |w| team_pane_ids(&w.team, &w.panes));
        let first = match &only {
            Some(o) => ws.and_then(|w| {
                ids.iter()
                    .find(|id| {
                        w.panes
                            .iter()
                            .any(|p| &&p.id == id && p.team_agent_id.as_ref() == Some(o))
                    })
                    .cloned()
            }),
            None => ids.first().cloned(),
        };
        if only.is_none() && !ids.is_empty() {
            self.update(cx, |c| {
                if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == ws_id) {
                    w.layout = auto_grid(&ids);
                }
            });
        }
        self.zoomed = None;
        if first.is_some() {
            self.focused = first;
        }
        self.close_modal(window, cx);
    }

    /// `stopTeam`: closes the team's panes (their chats go to History); asks first if one is
    /// working. Unlike closing each, it doesn't offer to remove every agent's branch folder.
    pub(super) fn stop_team(
        &mut self,
        ws_id: String,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.config().workspaces.iter().find(|w| w.id == ws_id) else {
            return;
        };
        let ids = team_pane_ids(&ws.team, &ws.panes);
        let busy = ids.iter().any(|id| {
            matches!(
                self.pane_status(id),
                Some("running" | "approval" | "background")
            )
        });
        if !force && busy {
            let dialog = cx.new(|_| {
                ChoiceDialog::new(
                    "Stop the team?",
                    "An agent is still working and will be stopped. Its chat goes to History.",
                    vec![("stop".into(), "Stop team".into(), String::new())],
                    "Keep running",
                )
            });
            cx.subscribe_in(
                &dialog,
                window,
                move |this, _, e: &ChoiceEvent, window, cx| {
                    this.close_modal(window, cx);
                    if let ChoiceEvent::Pick(_) = e {
                        this.stop_team(ws_id.clone(), true, window, cx);
                    }
                },
            )
            .detach();
            self.modal = Some(Modal::Choice(dialog));
            return cx.notify();
        }
        for id in ids {
            let archive = self
                .team_pane(&id)
                .is_some_and(|p| !p.shell && p.has_conversation);
            self.drop_pane(&id, archive, cx);
        }
    }
}
