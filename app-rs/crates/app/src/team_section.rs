//! A project's team on its page (`TeamSection.tsx`, #79): agents saved once (role, account,
//! model/effort/mode, first message, own branch folder or the project's), started together as
//! chats; their running state, add/edit/remove in place. The plan of a start (`lib/team.ts`) is
//! here too; the window runs it (`shell/team.rs`).

use std::collections::{BTreeMap, HashMap};

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::config::{is_chat_model, Account, Config, Pane, TeamAgent};
use wraithgrid_core::ids::new_id;
use wraithgrid_core::paths::slugify;
use wraithgrid_core::types::{ChatEffort, ChatMode, TEAM_MAX};
use wraithgrid_usage::auto_switch::peak_use;
use wraithgrid_usage::format::fmt_until;
use wraithgrid_usage::model::{live_window, AccountLimits, UsageWindow};

const MENU_W: f32 = 190.0;

/// The agent's own branch: `team/<role>` (its worktree folder is `team-<role>`).
pub fn team_branch(role: &str) -> String {
    let slug = slugify(role);
    format!("team/{}", if slug.is_empty() { "agent" } else { &slug })
}

/// What Start team does with one agent.
#[derive(Debug, PartialEq)]
pub enum TeamStep<'a> {
    /// Already open: show it rather than start it twice.
    Focus(&'a TeamAgent, String),
    Start(&'a TeamAgent, &'a Account),
    /// Its account was removed: skipped.
    Skip(&'a TeamAgent),
}

/// `planTeam`.
pub fn plan_team<'a>(
    team: &[&'a TeamAgent],
    accounts: &'a [Account],
    panes: &[Pane],
) -> Vec<TeamStep<'a>> {
    team.iter()
        .map(|&agent| {
            if let Some(p) = panes
                .iter()
                .find(|p| p.team_agent_id.as_deref() == Some(&agent.id))
            {
                return TeamStep::Focus(agent, p.id.clone());
            }
            match accounts
                .iter()
                .find(|a| Some(&a.id) == agent.account_id.as_ref())
            {
                Some(account) => TeamStep::Start(agent, account),
                None => TeamStep::Skip(agent),
            }
        })
        .collect()
}

/// Why some agents may not run (`teamWarnings`): a missing account, or one at its 5-hour or
/// weekly limit.
pub fn team_warnings<'a>(
    steps: &[TeamStep<'a>],
    limits: &BTreeMap<String, AccountLimits>,
    now: i64,
) -> Vec<(&'a TeamAgent, String)> {
    steps
        .iter()
        .filter_map(|s| match s {
            TeamStep::Skip(a) => Some((*a, "account missing · skipped".to_string())),
            TeamStep::Start(a, account) => peak_use(limits.get(&account.id), now)
                .filter(|p| p.pct >= 100)
                .map(|p| (*a, format!("{} limit {}%", p.window.label(), p.pct))),
            TeamStep::Focus(..) => None,
        })
        .collect()
}

/// The team's open panes, in the team's order.
pub fn team_pane_ids(team: &[TeamAgent], panes: &[Pane]) -> Vec<String> {
    team.iter()
        .flat_map(|a| {
            panes
                .iter()
                .filter(|p| p.team_agent_id.as_deref() == Some(&a.id))
                .map(|p| p.id.clone())
        })
        .collect()
}

/// Accounts that two or more agents use: they share its limit (#100). (account, roles)
pub fn shared_accounts(team: &[TeamAgent]) -> Vec<(String, Vec<String>)> {
    let mut by: Vec<(String, Vec<String>)> = vec![];
    for a in team {
        let Some(id) = &a.account_id else { continue };
        match by.iter_mut().find(|(k, _)| k == id) {
            Some((_, roles)) => roles.push(a.role.clone()),
            None => by.push((id.clone(), vec![a.role.clone()])),
        }
    }
    by.retain(|(_, roles)| roles.len() > 1);
    by
}

/// "Builder and Tests", "A, B and C".
fn list_roles(roles: &[String]) -> String {
    match roles {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `CHAT_MODE_LABEL`.
fn mode_label(m: ChatMode) -> &'static str {
    match m {
        ChatMode::Default => "Ask before edits",
        ChatMode::AcceptEdits => "Accept edits",
        ChatMode::Plan => "Plan",
        ChatMode::Auto => "Auto",
        ChatMode::DontAsk => "Don't ask",
        ChatMode::BypassPermissions => "Full access",
    }
}

/// "opus · high · plan", or "default" when nothing is set.
fn settings_line(a: &TeamAgent) -> String {
    let parts: Vec<String> = [
        a.model.clone(),
        a.effort.map(|e| e.as_str().to_string()),
        a.chat_mode.map(|m| mode_label(m).to_lowercase()),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "default".into()
    } else {
        parts.join(" · ")
    }
}

fn blank(account_id: Option<String>) -> TeamAgent {
    TeamAgent {
        id: new_id("t"),
        role: String::new(),
        account_id,
        model: None,
        effort: None,
        chat_mode: None,
        prompt: String::new(),
        folder: "worktree".into(),
    }
}

pub enum TeamEvent {
    /// Add or replace an agent: (project, agent).
    Save(String, TeamAgent),
    /// (project, agent id)
    Remove(String, String),
    /// Start the team, or one agent of it: (project, only).
    Start(String, Option<String>),
    Stop(String),
}

/// The agent being edited in place, or a new one.
struct AgentForm {
    agent: TeamAgent,
    new: bool,
    role: Entity<InputState>,
    model: Entity<InputState>,
    prompt: Entity<TextareaState>,
}

pub struct TeamSection {
    config: Config,
    ws_id: String,
    statuses: HashMap<String, &'static str>,
    limits: BTreeMap<String, AccountLimits>,
    form: Option<AgentForm>,
    /// The agent whose menu is open, at a point of the window.
    menu: Option<(String, Point<Pixels>)>,
}

impl TeamSection {
    pub fn new(config: Config, ws_id: String, statuses: HashMap<String, &'static str>) -> Self {
        TeamSection {
            config,
            ws_id,
            statuses,
            limits: BTreeMap::new(),
            form: None,
            menu: None,
        }
    }

    pub fn set_data(
        &mut self,
        config: Config,
        statuses: HashMap<String, &'static str>,
        cx: &mut Context<Self>,
    ) {
        self.config = config;
        self.statuses = statuses;
        cx.notify();
    }

    pub fn set_limits(&mut self, limits: BTreeMap<String, AccountLimits>, cx: &mut Context<Self>) {
        if self.limits != limits {
            self.limits = limits;
            cx.notify();
        }
    }

    fn team(&self) -> Vec<TeamAgent> {
        self.config
            .workspaces
            .iter()
            .find(|w| w.id == self.ws_id)
            .map(|w| w.team.clone())
            .unwrap_or_default()
    }

    fn edit(&mut self, agent: TeamAgent, new: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let role = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Reviewer")
                .default_value(agent.role.clone())
        });
        let model = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("default")
                .default_value(agent.model.clone().unwrap_or_default())
        });
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("You review every change in this repo for security and missing tests.")
                .auto_grow(3, 8)
                .default_value(agent.prompt.clone())
        });
        // Typing changes the branch shown and what Save allows; Enter saves, as a form submits.
        for input in [&role, &model] {
            cx.subscribe(input, |this, _, e: &InputEvent, cx| match e {
                InputEvent::PressEnter { .. } => this.save(cx),
                _ => cx.notify(),
            })
            .detach();
        }
        cx.observe(&prompt, |_, _, cx| cx.notify()).detach();
        role.update(cx, |r, cx| r.focus(window, cx));
        self.form = Some(AgentForm {
            agent,
            new,
            role,
            model,
            prompt,
        });
        cx.notify();
    }

    /// The form's agent as it would be saved, if Save is allowed.
    fn ready(&self, cx: &App) -> Option<TeamAgent> {
        let f = self.form.as_ref()?;
        let role: String = f.role.read(cx).value().trim().chars().take(40).collect();
        let model = f.model.read(cx).value().trim().to_string();
        let prompt: String = f
            .prompt
            .read(cx)
            .value()
            .trim()
            .chars()
            .take(4000)
            .collect();
        let ok = !role.is_empty()
            && f.agent.account_id.is_some()
            && (model.is_empty() || is_chat_model(&model));
        ok.then(|| TeamAgent {
            role,
            model: (!model.is_empty()).then_some(model),
            prompt,
            ..f.agent.clone()
        })
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(agent) = self.ready(cx) {
            self.form = None;
            cx.emit(TeamEvent::Save(self.ws_id.clone(), agent));
            cx.notify();
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.form = None;
        cx.notify();
    }

    fn agent_menu(&self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let started = self
            .config
            .workspaces
            .iter()
            .find(|w| w.id == self.ws_id)
            .is_some_and(|w| {
                w.panes
                    .iter()
                    .any(|p| p.team_agent_id.as_deref() == Some(id))
            });
        let item = |key: &'static str, label: &'static str, danger: bool| {
            div()
                .id(key)
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .when(danger, |d| d.text_color(theme.danger))
                .child(label)
        };
        let (edit, start, remove) = (id.to_string(), id.to_string(), id.to_string());
        div()
            .id("team-menu")
            .absolute()
            .left(at.x)
            .top(at.y)
            .w(px(MENU_W))
            .p_1()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .text_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(item("team-menu-edit", "Edit", false).on_click(cx.listener(
                move |this, _, window, cx| {
                    if let Some(a) = this.team().into_iter().find(|a| a.id == edit) {
                        this.edit(a, false, window, cx);
                    }
                },
            )))
            .child(
                item(
                    "team-menu-start",
                    if started {
                        "Show this agent"
                    } else {
                        "Start this agent"
                    },
                    false,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    cx.emit(TeamEvent::Start(this.ws_id.clone(), Some(start.clone())));
                    cx.notify();
                })),
            )
            .child(
                div()
                    .mt_1()
                    .pt_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        item("team-menu-remove", "Remove", true).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.menu = None;
                                cx.emit(TeamEvent::Remove(this.ws_id.clone(), remove.clone()));
                                cx.notify();
                            },
                        )),
                    ),
            )
            .into_any_element()
    }

    fn form_view(&self, f: &AgentForm, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let a = &f.agent;
        let label = |t: &'static str| div().text_xs().text_color(theme.muted_foreground).child(t);
        let chip = |id: String, text: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .px_2()
                .py(px(2.))
                .rounded_md()
                .border_1()
                .text_sm()
                .cursor_pointer()
                .border_color(if on { theme.ring } else { theme.border })
                .when(on, |d| d.bg(theme.background))
                .child(text)
        };
        let row = || div().flex().flex_wrap().gap_2();
        let mut accounts: Vec<AnyElement> = vec![];
        if let Some(id) = a
            .account_id
            .as_ref()
            .filter(|id| self.config.account(id).is_none())
        {
            accounts.push(
                chip(format!("team-acct-{id}"), "Account missing".into(), true)
                    .text_color(theme.warning)
                    .into_any_element(),
            );
        }
        accounts.extend(self.config.accounts.iter().map(|x| {
            let id = x.id.clone();
            chip(
                format!("team-acct-{}", x.id),
                format!("{} · {}", x.name, x.cli.spec().label),
                a.account_id.as_ref() == Some(&x.id),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(f) = &mut this.form {
                    f.agent.account_id = Some(id.clone());
                }
                cx.notify();
            }))
            .into_any_element()
        }));
        let efforts = std::iter::once(None)
            .chain(ChatEffort::ALL.iter().copied().map(Some))
            .map(|e| {
                let name = e.map_or("default", |e| e.as_str());
                chip(format!("team-effort-{name}"), name.into(), a.effort == e).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if let Some(f) = &mut this.form {
                            f.agent.effort = e;
                        }
                        cx.notify();
                    }),
                )
            });
        let modes = ChatMode::ALL.iter().copied().map(|m| {
            let on = a.chat_mode.unwrap_or(ChatMode::Default) == m;
            chip(
                format!("team-mode-{}", m.as_str()),
                mode_label(m).into(),
                on,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(f) = &mut this.form {
                    f.agent.chat_mode = (m != ChatMode::Default).then_some(m);
                }
                cx.notify();
            }))
        });
        let role_now = f.role.read(cx).value().to_string();
        let model_now = f.model.read(cx).value().trim().to_string();
        let model_ok = model_now.is_empty() || is_chat_model(&model_now);
        let radio = |value: &'static str, title: AnyElement, note: &'static str| {
            let on = a.folder == value;
            div()
                .id(SharedString::from(format!("team-folder-{value}")))
                .flex()
                .gap_2()
                .cursor_pointer()
                .child(
                    div()
                        .mt(px(3.))
                        .size(px(12.))
                        .flex_shrink_0()
                        .rounded_full()
                        .border_1()
                        .border_color(if on {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .when(on, |d| d.bg(theme.foreground)),
                )
                .child(
                    div().flex().flex_col().child(title).child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(note),
                    ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(f) = &mut this.form {
                        f.agent.folder = value.into();
                    }
                    cx.notify();
                }))
        };
        let ready = self.ready(cx).is_some();
        let button = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(text)
        };
        let remove = (!f.new).then(|| f.agent.id.clone());
        div()
            .id("team-edit")
            .px_3()
            .pt_4()
            .pb_4()
            .flex()
            .flex_col()
            .gap_3()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.cancel(cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .gap_4()
                    .child(
                        div()
                            .w(px(160.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(label("Role"))
                            .child(Input::new(&f.role)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(label("Account"))
                            .child(row().children(accounts)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_4()
                    .child(
                        div()
                            .w(px(160.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(label("Model"))
                            .child(
                                div()
                                    .rounded_md()
                                    .when(!model_ok, |d| d.border_1().border_color(theme.danger))
                                    .child(Input::new(&f.model)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(label("Effort"))
                            .child(row().children(efforts)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(label("Mode"))
                    .child(row().children(modes)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(label("First message"))
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(crate::terminal::mono(cx))
                                    .text_color(theme.muted_foreground)
                                    .child("optional, sent when it starts"),
                            ),
                    )
                    .child(Textarea::new(&f.prompt)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(label("Folder"))
                    .child(radio(
                        "worktree",
                        div()
                            .flex()
                            .gap_1()
                            .child("Its own branch folder")
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(crate::terminal::mono(cx))
                                    .text_color(theme.muted_foreground)
                                    .child(team_branch(&role_now)),
                            )
                            .into_any_element(),
                        "A git worktree under ~/.wraithgrid/worktrees, so agents can't overwrite each other. Not a git repo: the project folder, with a notice.",
                    ))
                    .child(radio(
                        "shared",
                        div().child("The project folder").into_any_element(),
                        "Shared with the other agents: they can overwrite each other's edits.",
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when_some(remove, |d, id| {
                        d.child(button("team-remove", "Remove").on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.form = None;
                                cx.emit(TeamEvent::Remove(this.ws_id.clone(), id.clone()));
                                cx.notify();
                            },
                        )))
                    })
                    .child(div().flex_1())
                    .child(
                        button("team-cancel", "Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .child(
                        div()
                            .id("team-save")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .when(!ready, |d| d.opacity(0.5))
                            .when(ready, |d| d.cursor_pointer())
                            .child("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
            .into_any_element()
    }
}

impl EventEmitter<TeamEvent> for TeamSection {}

/// "42%" next to an account, amber from the auto-switch threshold (#100); the tooltip has both
/// windows and their resets.
fn account_use(
    limits: Option<&AccountLimits>,
    now: i64,
    warn_at: i64,
    id: String,
    cx: &App,
) -> Option<AnyElement> {
    let peak = peak_use(limits, now)?;
    let limits = limits?;
    let line = |name: &str, w: Option<UsageWindow>| {
        live_window(w, now).map(|l| {
            format!(
                "{name} {}% · resets in {}",
                (l.utilization * 100.0).round() as i64,
                fmt_until(l.resets_at, now)
            )
        })
    };
    let tip = [
        line("5-hour", limits.five_hour),
        line("Weekly", limits.seven_day),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    let theme = cx.theme();
    Some(
        div()
            .id(SharedString::from(format!("team-use-{id}")))
            .flex_none()
            .text_xs()
            .font_family(crate::terminal::mono(cx))
            .text_color(if peak.pct >= warn_at {
                theme.warning
            } else {
                theme.muted_foreground
            })
            .child(format!("{}%", peak.pct))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            })
            .into_any_element(),
    )
}

impl Render for TeamSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let now = wraithgrid_usage::now_ms();
        let Some(ws) = self
            .config
            .workspaces
            .iter()
            .find(|w| w.id == self.ws_id)
            .cloned()
        else {
            return div().id("team");
        };
        let team = &ws.team;
        let pane_of = |a: &TeamAgent| {
            ws.panes
                .iter()
                .find(|p| p.team_agent_id.as_deref() == Some(&a.id))
        };
        let running = team.iter().filter(|a| pane_of(a).is_some()).count();
        let editing = self.form.as_ref().map(|f| f.agent.id.clone());
        let mono = crate::terminal::mono(cx);
        let warn_at = self.config.settings.auto_switch_at;
        // (text, color); no status yet: its chat view was never made, so not running.
        let state_of = |a: &TeamAgent| -> (&'static str, Hsla) {
            if editing.as_ref() == Some(&a.id) {
                return ("editing", theme.muted_foreground);
            }
            if a.account_id
                .as_ref()
                .is_none_or(|id| self.config.account(id).is_none())
            {
                return ("skipped", theme.muted_foreground);
            }
            let Some(pane) = pane_of(a) else {
                return ("not started", theme.muted_foreground);
            };
            match self.statuses.get(&pane.id).copied().unwrap_or("idle") {
                "approval" => ("approve", theme.warning),
                "running" | "starting" | "background" => ("working", theme.success),
                _ => ("idle", theme.muted_foreground),
            }
        };
        let meta = |t: String| {
            div()
                .text_xs()
                .font_family(mono.clone())
                .text_color(theme.muted_foreground)
                .truncate()
                .child(t)
        };

        let mut rows: Vec<AnyElement> = vec![];
        for a in team {
            let account = a.account_id.as_ref().and_then(|id| self.config.account(id));
            let (state, tone) = state_of(a);
            let sel = editing.as_ref() == Some(&a.id);
            let (menu_at, menu_btn) = (a.id.clone(), a.id.clone());
            let acct = match account {
                Some(x) => {
                    let color = crate::shell::parse_hex(&x.color).unwrap_or(theme.primary);
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .min_w(px(0.))
                        .child(div().size(px(8.)).flex_none().rounded_full().bg(color))
                        .child(
                            div()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(x.name.clone()),
                        )
                        .children(account_use(
                            self.limits.get(&x.id),
                            now,
                            warn_at,
                            a.id.clone(),
                            cx,
                        ))
                }
                None => div().text_color(theme.warning).child("Account missing"),
            };
            rows.push(
                div()
                    .id(SharedString::from(format!("team-row-{}", a.id)))
                    .h(px(44.))
                    .pl(px(10.))
                    .pr_1()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .hover(|d| d.bg(theme.secondary))
                    .when(sel, |d| d.bg(theme.secondary))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            this.menu = Some((menu_at.clone(), e.position));
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .w(px(130.))
                            .flex_none()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(a.role.clone()),
                    )
                    .child(div().flex_1().min_w(px(0.)).child(acct))
                    .child(div().flex_1().min_w(px(0.)).child(meta(settings_line(a))))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(meta(if a.folder == "worktree" {
                                format!("⑂ {}", team_branch(&a.role))
                            } else {
                                "▭ project folder".into()
                            })),
                    )
                    .child(
                        div()
                            .w(px(84.))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .text_xs()
                            .font_family(mono.clone())
                            .text_color(tone)
                            .child(state),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("team-more-{}", a.id)))
                            .w(px(24.))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.background))
                            .child("⋯")
                            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                                let at = point(
                                    (e.position().x - px(MENU_W))
                                        .min(window.viewport_size().width - px(MENU_W + 8.))
                                        .max(px(8.)),
                                    e.position().y + px(10.),
                                );
                                this.menu = Some((menu_btn.clone(), at));
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
            if let Some(f) = self.form.as_ref().filter(|f| f.agent.id == a.id) {
                rows.push(self.form_view(f, cx));
            }
        }
        let shares = shared_accounts(team).into_iter().map(|(id, roles)| {
            let name = self
                .config
                .account(&id)
                .map_or("an account".to_string(), |a| a.name.clone());
            div()
                .px(px(10.))
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("{} share {name}: one limit", list_roles(&roles)))
        });
        let new_form = self
            .form
            .as_ref()
            .filter(|f| f.new && !team.iter().any(|a| a.id == f.agent.id))
            .map(|f| self.form_view(f, cx));
        let default_account = ws
            .account_id
            .clone()
            .filter(|id| self.config.account(id).is_some())
            .or_else(|| self.config.accounts.first().map(|a| a.id.clone()));
        let add = (new_form.is_none() && team.len() < TEAM_MAX).then(|| {
            div()
                .id("team-add")
                .h(px(40.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .text_color(theme.muted_foreground)
                .hover(|d| d.text_color(theme.foreground))
                .child("+")
                .child("Add agent")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.edit(blank(default_account.clone()), true, window, cx)
                }))
        });
        let summary = if team.is_empty() {
            "Save a set of agents and start them together".to_string()
        } else if running > 0 {
            format!("{running} of {} running", team.len())
        } else {
            format!(
                "{} {}",
                team.len(),
                if team.len() == 1 { "agent" } else { "agents" }
            )
        };
        let ws_id = ws.id.clone();
        let action = (!team.is_empty()).then(|| {
            let base = div()
                .id("team-start")
                .ml_auto()
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer();
            if running > 0 {
                base.border_1()
                    .border_color(theme.border)
                    .hover(|d| d.bg(theme.secondary))
                    .child("■ Stop team")
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(TeamEvent::Stop(ws_id.clone()))),
                    )
            } else {
                base.bg(theme.primary)
                    .text_color(theme.primary_foreground)
                    .child("▶ Start team")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(TeamEvent::Start(ws_id.clone(), None))
                    }))
            }
        });
        let viewport = window.viewport_size();
        let menu = self
            .menu
            .clone()
            .filter(|(id, _)| team.iter().any(|a| &a.id == id))
            .map(|(id, at)| self.agent_menu(&id, at, cx));

        div()
            .id("team")
            .mt_1()
            .mb(px(22.))
            .flex()
            .flex_col()
            .child(
                div()
                    .min_h(px(32.))
                    .mb(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Team"))
                    .child(meta(summary))
                    .children(action),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(theme.border)
                    .children(rows)
                    .children(shares)
                    .children(new_form)
                    .children(add),
            )
            .when_some(menu, |d, menu| {
                // A scrim over the window closes the menu on any click outside it.
                d.child(
                    deferred(
                        anchored().position(point(px(0.), px(0.))).child(
                            div()
                                .id("team-scrim")
                                .occlude()
                                .w(viewport.width)
                                .h(viewport.height)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.menu = None;
                                        cx.stop_propagation();
                                        cx.notify();
                                    }),
                                )
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(|this, _, _, cx| {
                                        this.menu = None;
                                        cx.notify();
                                    }),
                                )
                                .child(menu),
                        ),
                    )
                    .with_priority(1),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        blank, list_roles, plan_team, settings_line, shared_accounts, team_branch, team_pane_ids,
        team_warnings, TeamStep,
    };
    use wraithgrid_core::config::{Account, Extra, Pane, TeamAgent};
    use wraithgrid_core::types::{ChatEffort, ChatMode};
    use wraithgrid_usage::model::{AccountLimits, UsageWindow};

    fn agent(id: &str, role: &str, account: Option<&str>) -> TeamAgent {
        TeamAgent {
            id: id.into(),
            role: role.into(),
            account_id: account.map(Into::into),
            ..blank(None)
        }
    }

    fn account(id: &str) -> Account {
        Account {
            id: id.into(),
            name: id.into(),
            cli: wraithgrid_core::agent_cli::AgentCli::Claude,
            config_dir: String::new(),
            color: String::new(),
            signed_in: true,
            imported: false,
            icon: String::new(),
            folder_id: None,
            extra: Extra::default(),
        }
    }

    fn pane(id: &str, agent: Option<&str>) -> Pane {
        Pane {
            id: id.into(),
            account_id: None,
            cwd: "~/x".into(),
            args: vec![],
            shell: false,
            title: id.into(),
            resume: true,
            has_conversation: false,
            view: wraithgrid_core::types::PaneView::Chat,
            chat_session_id: None,
            model: None,
            effort: None,
            chat_mode: None,
            team_agent_id: agent.map(Into::into),
            extra: Extra::default(),
        }
    }

    #[test]
    fn branches_are_team_slash_the_role_slug() {
        assert_eq!(team_branch("Code Reviewer!"), "team/code-reviewer");
        assert_eq!(team_branch("!!"), "team/agent");
    }

    #[test]
    fn plans_open_agents_as_focus_and_skips_missing_accounts() {
        let team = [
            agent("t1", "A", Some("a1")),
            agent("t2", "B", Some("gone")),
            agent("t3", "C", Some("a1")),
        ];
        let accounts = [account("a1")];
        let panes = [pane("p1", Some("t3"))];
        let refs: Vec<&TeamAgent> = team.iter().collect();
        let steps = plan_team(&refs, &accounts, &panes);
        assert!(matches!(steps[0], TeamStep::Start(a, _) if a.id == "t1"));
        assert!(matches!(steps[1], TeamStep::Skip(a) if a.id == "t2"));
        assert_eq!(steps[2], TeamStep::Focus(&team[2], "p1".into()));
        let warns = team_warnings(&steps, &BTreeMap::new(), 0);
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0].1, "account missing · skipped");
    }

    #[test]
    fn warns_about_an_account_at_its_limit() {
        let team = [agent("t1", "A", Some("a1"))];
        let accounts = [account("a1")];
        let refs: Vec<&TeamAgent> = team.iter().collect();
        let steps = plan_team(&refs, &accounts, &[]);
        let limits = BTreeMap::from([(
            "a1".to_string(),
            AccountLimits {
                five_hour: Some(UsageWindow {
                    utilization: 1.0,
                    resets_at: 10_000,
                }),
                seven_day: None,
                at: 0,
            },
        )]);
        let warns = team_warnings(&steps, &limits, 0);
        assert_eq!(warns[0].1, "5-hour limit 100%");
    }

    #[test]
    fn shared_accounts_list_their_roles_in_team_order() {
        let team = [
            agent("t1", "Builder", Some("a1")),
            agent("t2", "Solo", Some("a2")),
            agent("t3", "Tests", Some("a1")),
        ];
        let shared = shared_accounts(&team);
        assert_eq!(
            shared,
            vec![("a1".into(), vec!["Builder".into(), "Tests".into()])]
        );
        assert_eq!(list_roles(&shared[0].1), "Builder and Tests");
        assert_eq!(
            list_roles(&["A".into(), "B".into(), "C".into()]),
            "A, B and C"
        );
        let panes = [
            pane("p3", Some("t3")),
            pane("p1", Some("t1")),
            pane("x", None),
        ];
        assert_eq!(team_pane_ids(&team, &panes), vec!["p1", "p3"]);
    }

    #[test]
    fn settings_line_reads_like_the_row() {
        let mut a = agent("t1", "A", None);
        assert_eq!(settings_line(&a), "default");
        a.model = Some("opus".into());
        a.effort = Some(ChatEffort::High);
        a.chat_mode = Some(ChatMode::Plan);
        assert_eq!(settings_line(&a), "opus · high · plan");
    }
}
