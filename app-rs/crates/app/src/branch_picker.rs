//! Switch the branch of a pane's folder (`BranchPicker.tsx`): local branches newest first,
//! remote ones not checked out yet (as a tracking branch), or a new branch from HEAD. Every pane
//! on the folder sees a switch, so with other panes there it asks first and offers a new pane
//! on its own worktree instead (#162); `Open` mode only does that. Git runs here, off the UI
//! thread, so its error stays in the picker; the window opens the pane.

use std::path::PathBuf;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::paths::{contract_home, home_dir, worktrees_root};
use wraithgrid_git::{
    add_worktree, list_branches, switch_branch, valid_branch_name, GitBranches, GitCli,
    SwitchTarget, WorktreeFrom,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickerMode {
    Switch,
    /// The branch opens in a new pane on its own worktree; this folder stays as it is.
    Open,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Local,
    Remote,
    Create,
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Choice {
    kind: Kind,
    name: String,
    sub: String,
    current: bool,
}

pub enum BranchEvent {
    /// The folder is on another branch now.
    Switched,
    /// The branch's worktree is ready (home-relative): open a pane like this one on it.
    Opened(String),
    Cancel,
}

pub struct BranchPicker {
    /// The pane's folder, resolved.
    cwd: PathBuf,
    mode: PickerMode,
    /// Other panes on the same folder: they switch with it.
    sharing: usize,
    /// The pane's agent is working.
    running: bool,
    query: Entity<InputState>,
    branches: Option<GitBranches>,
    selected: usize,
    /// Picked while other panes share the folder: switch them all, or a new pane instead.
    confirm: Option<Choice>,
    busy: bool,
    error: Option<String>,
    _subscription: Subscription,
}

/// What the list offers for `query` (`options` in BranchPicker.tsx): the checked-out branch
/// can't be opened again in another folder; a typed name no branch has can be created.
fn choices(b: &GitBranches, query: &str, mode: PickerMode) -> Vec<Choice> {
    let q = query.trim();
    let hit = |name: &str| {
        let hay = name.to_lowercase();
        q.to_lowercase().split_whitespace().all(|w| hay.contains(w))
    };
    let current = b.current.as_deref();
    let mut all: Vec<Choice> = b
        .local
        .iter()
        .filter(|l| mode == PickerMode::Switch || Some(l.name.as_str()) != current)
        .map(|l| {
            let here = Some(l.name.as_str()) == current;
            Choice {
                kind: Kind::Local,
                name: l.name.clone(),
                sub: [here.then(|| "current".to_string()), Some(l.when.clone())]
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · "),
                current: here,
            }
        })
        .chain(b.remote.iter().map(|r| Choice {
            kind: Kind::Remote,
            name: r.name.clone(),
            sub: format!("remote · {}", r.when),
            current: false,
        }))
        .filter(|c| hit(&c.name))
        .collect();
    if !q.is_empty() && !b.local.iter().any(|l| l.name == q) && valid_branch_name(q) {
        all.push(Choice {
            kind: Kind::Create,
            name: q.to_string(),
            sub: format!("new, from {}", current.unwrap_or("HEAD")),
            current: false,
        });
    }
    all
}

impl BranchPicker {
    pub fn new(
        cwd: PathBuf,
        mode: PickerMode,
        sharing: usize,
        running: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder(match mode {
                PickerMode::Open => "Branch to open in a new pane…",
                PickerMode::Switch => "Find or create a branch…",
            })
        });
        let sub = cx.subscribe_in(
            &query,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { .. } => match this.confirm.clone() {
                    // Its default answer, as the Electron picker focuses it.
                    Some(c) => this.act(c, PickerMode::Open, cx),
                    None => {
                        if let Some(c) = this.options(cx).get(this.selected).cloned() {
                            this.pick(c, cx);
                        }
                    }
                },
                InputEvent::Change => {
                    this.selected = 0;
                    cx.notify();
                }
                _ => {}
            },
        );
        window.focus(&query.focus_handle(cx), cx);
        let dir = cwd.clone();
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_spawn(async move { list_branches(&GitCli::default(), &dir) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match listed {
                    Ok(b) => this.branches = Some(b),
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
        BranchPicker {
            cwd,
            mode,
            sharing,
            running,
            query,
            branches: None,
            selected: 0,
            confirm: None,
            busy: false,
            error: None,
            _subscription: sub,
        }
    }

    fn options(&self, cx: &App) -> Vec<Choice> {
        self.branches.as_ref().map_or_else(Vec::new, |b| {
            choices(b, &self.query.read(cx).value(), self.mode)
        })
    }

    fn pick(&mut self, c: Choice, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.mode == PickerMode::Open {
            return self.act(c, PickerMode::Open, cx);
        }
        if c.current {
            return cx.emit(BranchEvent::Cancel);
        }
        // Other panes on this folder would move too: ask (#162).
        if self.sharing > 0 {
            self.error = None;
            self.confirm = Some(c);
            return cx.notify();
        }
        self.act(c, PickerMode::Switch, cx);
    }

    fn act(&mut self, c: Choice, how: PickerMode, cx: &mut Context<Self>) {
        self.confirm = None;
        self.busy = true;
        self.error = None;
        cx.notify();
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_spawn(async move {
                    let git = GitCli::default();
                    match how {
                        PickerMode::Open => {
                            let home = home_dir();
                            let from = match c.kind {
                                Kind::Local => WorktreeFrom::Local,
                                Kind::Remote => WorktreeFrom::Remote,
                                Kind::Create => WorktreeFrom::New,
                            };
                            add_worktree(&git, &cwd, &c.name, &worktrees_root(&home), false, from)
                                .map(|dir| {
                                    Some(contract_home(
                                        &dir.to_string_lossy(),
                                        &home.to_string_lossy(),
                                    ))
                                })
                        }
                        PickerMode::Switch => {
                            let to = match c.kind {
                                Kind::Local => SwitchTarget::Local(c.name),
                                Kind::Remote => SwitchTarget::Remote(c.name),
                                Kind::Create => SwitchTarget::Create(c.name),
                            };
                            switch_branch(&git, &cwd, &to).map(|_| None)
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match done {
                    Ok(Some(dir)) => cx.emit(BranchEvent::Opened(dir)),
                    Ok(None) => cx.emit(BranchEvent::Switched),
                    Err(e) => {
                        this.error = Some(e);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

impl EventEmitter<BranchEvent> for BranchPicker {}

impl Render for BranchPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let options = self.options(cx);
        let count = options.len();
        let open = self.mode == PickerMode::Open;
        let note = |text: String| {
            div()
                .px_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let rows = options.iter().enumerate().map(|(i, o)| {
            let pick = o.clone();
            div()
                .id(SharedString::from(format!("branch-{i}")))
                .px_2()
                .py_1()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .when(i == self.selected, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
                .child(
                    div()
                        .w(px(14.))
                        .text_color(theme.muted_foreground)
                        .child(if o.kind == Kind::Create { "+" } else { "⑂" }),
                )
                .child(div().flex_1().truncate().child(if o.kind == Kind::Create {
                    format!(
                        "{} {}",
                        if open { "New branch" } else { "Create branch" },
                        o.name
                    )
                } else {
                    o.name.clone()
                }))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(o.sub.clone()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.pick(pick.clone(), cx)))
        });
        let sharing = self.sharing;
        let s = if sharing == 1 { "" } else { "s" };
        let hint = if open {
            Some(format!(
                "Opens in a new pane with its own folder (a git worktree); this one stays on {}.",
                self.branches
                    .as_ref()
                    .and_then(|b| b.current.clone())
                    .unwrap_or_else(|| "its branch".into())
            ))
        } else if self.confirm.is_none() && (sharing > 0 || self.running) {
            Some(
                [
                    (sharing > 0).then(|| format!("{sharing} other pane{s} on this folder: you can open the branch in a new pane instead.")),
                    self.running.then(|| "This pane’s agent is working: its files change under it.".to_string()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" "),
            )
        } else {
            None
        };
        let button = |id: &'static str, label: String, primary: bool| {
            div()
                .id(id)
                .px_2()
                .py(px(2.))
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .when(primary, |d| {
                    d.bg(theme.primary).text_color(theme.primary_foreground)
                })
                .when(!primary, |d| d.border_1().border_color(theme.border))
                .child(label)
        };
        div()
            .w(px(460.))
            .max_h(px(440.))
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .capture_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => {
                        if this.confirm.take().is_some() {
                            cx.notify();
                        } else {
                            cx.emit(BranchEvent::Cancel);
                        }
                        cx.stop_propagation();
                    }
                    "down" | "up" if count > 0 && this.confirm.is_none() => {
                        let step = if e.keystroke.key == "down" { 1 } else { count - 1 };
                        this.selected = (this.selected + step) % count;
                        cx.notify();
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .child(Input::new(&self.query))
            .child(
                div()
                    .id("branch-list")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .when(self.branches.is_none() && self.error.is_none(), |d| {
                        d.child(note("Reading branches…".into()))
                    })
                    .when(self.branches.is_some() && count == 0, |d| {
                        d.child(note("No branch matches.".into()))
                    })
                    .children(rows),
            )
            .when_some(self.confirm.clone(), |d, c| {
                let them = if sharing == 1 { "it" } else { "them" };
                let uses = if sharing == 1 { "uses" } else { "use" };
                let (all, new) = (c.clone(), c.clone());
                d.child(
                    div()
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .rounded_md()
                        .bg(theme.secondary)
                        .text_sm()
                        .child(format!(
                            "{sharing} other pane{s} {uses} this folder. Switching to {} moves {them} too.",
                            c.name
                        ))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    button("branch-all", format!("Switch all {}", sharing + 1), false)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.act(all.clone(), PickerMode::Switch, cx)
                                        })),
                                )
                                .child(
                                    button("branch-new-pane", "Open in a new pane".into(), true)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.act(new.clone(), PickerMode::Open, cx)
                                        })),
                                ),
                        ),
                )
            })
            .when(self.busy, |d| {
                d.child(note(
                    if open { "Creating worktree…" } else { "Working…" }.into(),
                ))
            })
            .when_some(self.error.clone(), |d, e| {
                d.child(div().px_2().text_xs().text_color(theme.danger).child(e))
            })
            .when_some(hint, |d, h| d.child(note(h)))
    }
}

#[cfg(test)]
mod tests {
    use super::{choices, Kind, PickerMode};
    use wraithgrid_git::{GitBranches, LocalBranch, RemoteBranch};

    fn branches() -> GitBranches {
        let local = |name: &str| LocalBranch {
            name: name.into(),
            when: "2 hours ago".into(),
            upstream: None,
        };
        GitBranches {
            current: Some("main".into()),
            local: vec![local("main"), local("feat/login")],
            remote: vec![RemoteBranch {
                name: "origin/fix".into(),
                when: "1 day ago".into(),
            }],
        }
    }

    #[test]
    fn lists_local_then_remote_and_offers_to_create_a_new_name() {
        let all = choices(&branches(), "", PickerMode::Switch);
        assert_eq!(
            all.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["main", "feat/login", "origin/fix"]
        );
        assert!(all[0].current);
        assert_eq!(all[0].sub, "current · 2 hours ago");
        assert_eq!(all[2].sub, "remote · 1 day ago");
        let typed = choices(&branches(), "feat/new", PickerMode::Switch);
        assert_eq!(typed.last().unwrap().kind, Kind::Create);
        assert_eq!(typed.last().unwrap().sub, "new, from main");
        // An existing name or an option-like one is never offered as new.
        assert!(choices(&branches(), "main", PickerMode::Switch)
            .iter()
            .all(|c| c.kind != Kind::Create));
        assert!(choices(&branches(), "--force", PickerMode::Switch).is_empty());
    }

    #[test]
    fn open_mode_leaves_out_the_branch_checked_out_here() {
        let all = choices(&branches(), "", PickerMode::Open);
        assert!(all.iter().all(|c| c.name != "main"));
        assert_eq!(
            choices(&branches(), "LOGIN", PickerMode::Open)[0].name,
            "feat/login"
        );
    }
}
