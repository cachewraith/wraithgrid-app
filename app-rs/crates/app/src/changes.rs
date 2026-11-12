//! The Changes panel (`DiffPanel.tsx`): the focused pane's repo, its branch and ahead/behind,
//! the changed files with their diff, and Commit / Pull / Push (push, and a pull over
//! uncommitted changes, after a confirm; the Git menu asks here too, #162). Git runs off the UI
//! thread through the `wraithgrid-git` crate; the panel refreshes every 5 s while it is open.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_git::{
    commit_all, git_diff, git_log, git_show, git_status, layout_graph, parse_patch, pull_branch,
    push_branch, ref_labels, DiffFile, DiffLineKind, DiffStatus, GitCli, GitCommit, GitStatus,
    GraphCommit, GraphRow, RefKind,
};

const POLL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Changes,
    History,
}

/// Lane colors of the commit graph, by the layout's color index.
const LANES: [u32; 6] = [0x7c5cff, 0x2dd4bf, 0xf5a524, 0xf43f5e, 0x60a5fa, 0xa3e635];
const LANE_W: f32 = 12.0;
const ROW_H: f32 = 24.0;

/// What the Git menu asks the panel to do (#162): it never sends anything by itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GitAsk {
    Commit,
    Pull,
    Push,
}

pub struct ChangesPanel {
    tab: Tab,
    /// A pull or push waiting for its confirm.
    asking: Option<GitAsk>,
    commits: Vec<GitCommit>,
    graph: Vec<GraphRow>,
    more: bool,
    shown_commit: Option<String>,
    commit_files: Vec<DiffFile>,
    history_error: Option<String>,
    cwd: PathBuf,
    status: GitStatus,
    files: Vec<DiffFile>,
    untracked: Vec<String>,
    truncated: bool,
    selected: usize,
    message: Entity<InputState>,
    busy: Option<&'static str>,
    note: Option<(bool, String)>,
    _poll: Task<()>,
    _subscription: Subscription,
}

struct Snapshot {
    status: GitStatus,
    files: Vec<DiffFile>,
    untracked: Vec<String>,
    truncated: bool,
}

fn snapshot(cwd: &std::path::Path) -> Snapshot {
    let git = GitCli::default();
    let status = git_status(&git, cwd);
    let diff = if status.repo {
        git_diff(&git, cwd).ok()
    } else {
        None
    };
    Snapshot {
        status,
        files: diff
            .as_ref()
            .map(|d| parse_patch(&d.patch))
            .unwrap_or_default(),
        untracked: diff
            .as_ref()
            .map(|d| d.untracked.clone())
            .unwrap_or_default(),
        truncated: diff.is_some_and(|d| d.truncated),
    }
}

impl ChangesPanel {
    pub fn new(cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| InputState::new(window, cx).placeholder("Commit message"));
        let sub = cx.subscribe_in(&message, window, |this, _, e: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = e {
                this.commit(cx);
            }
        });
        let poll_cwd = cwd.clone();
        let poll = cx.spawn(async move |this, cx| loop {
            let cwd = poll_cwd.clone();
            let snap = cx.background_spawn(async move { snapshot(&cwd) }).await;
            if this.update(cx, |this, cx| this.apply(snap, cx)).is_err() {
                break;
            }
            cx.background_executor().timer(POLL).await;
        });
        ChangesPanel {
            tab: Tab::Changes,
            asking: None,
            commits: vec![],
            graph: vec![],
            more: false,
            shown_commit: None,
            commit_files: vec![],
            history_error: None,
            cwd,
            status: GitStatus::default(),
            files: vec![],
            untracked: vec![],
            truncated: false,
            selected: 0,
            message,
            busy: None,
            note: None,
            _poll: poll,
            _subscription: sub,
        }
    }

    fn apply(&mut self, snap: Snapshot, cx: &mut Context<Self>) {
        self.status = snap.status;
        self.files = snap.files;
        self.untracked = snap.untracked;
        self.truncated = snap.truncated;
        self.selected = self.selected.min(self.files.len().saturating_sub(1));
        cx.notify();
    }

    /// Runs a git write off the UI thread, then shows its outcome and refreshes.
    fn run(
        &mut self,
        what: &'static str,
        job: impl FnOnce(&GitCli, &PathBuf) -> Result<String, String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(what);
        self.note = None;
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            let (result, snap) = cx
                .background_spawn(async move {
                    let result = job(&GitCli::default(), &cwd);
                    (result, snapshot(&cwd))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.note = Some(match result {
                    Ok(out) => (
                        true,
                        if out.trim().is_empty() {
                            format!("{what} done.")
                        } else {
                            out.trim().to_string()
                        },
                    ),
                    Err(e) => (false, e),
                });
                this.apply(snap, cx);
            });
        })
        .detach();
        cx.notify();
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Commit focuses the message; pull and push wait for their confirm (`gitAsk`).
    pub fn ask(&mut self, what: GitAsk, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Changes;
        match what {
            GitAsk::Commit => window.focus(&self.message.focus_handle(cx), cx),
            _ => self.asking = Some(what),
        }
        cx.notify();
    }

    fn pull(&mut self, cx: &mut Context<Self>) {
        if self.status.changed > 0 {
            self.asking = Some(GitAsk::Pull);
            return cx.notify();
        }
        self.run_asked(GitAsk::Pull, cx);
    }

    fn run_asked(&mut self, what: GitAsk, cx: &mut Context<Self>) {
        self.asking = None;
        match what {
            GitAsk::Pull => self.run("Pull", |g, c| pull_branch(g, c), cx),
            GitAsk::Push => self.run("Push", |g, c| push_branch(g, c), cx),
            GitAsk::Commit => self.commit(cx),
        }
    }

    pub fn show_history(&mut self, cx: &mut Context<Self>) {
        self.tab = Tab::History;
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            let log = cx
                .background_spawn(async move { git_log(&GitCli::default(), &cwd, 0) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match log {
                    Ok(log) => {
                        this.graph = layout_graph(
                            &log.commits
                                .iter()
                                .map(GraphCommit::from)
                                .collect::<Vec<_>>(),
                        );
                        this.commits = log.commits;
                        this.more = log.more;
                        this.history_error = None;
                    }
                    Err(e) => this.history_error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn show_commit(&mut self, sha: String, cx: &mut Context<Self>) {
        self.shown_commit = Some(sha.clone());
        self.commit_files.clear();
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            let shown = cx
                .background_spawn(async move { git_show(&GitCli::default(), &cwd, &sha) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match shown {
                    Ok(s) => this.commit_files = parse_patch(&s.patch),
                    Err(e) => this.history_error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        let message = self.message.read(cx).value().trim().to_string();
        if message.is_empty() {
            self.note = Some((false, "Write a commit message.".into()));
            return cx.notify();
        }
        self.run("Commit", move |git, cwd| commit_all(git, cwd, &message), cx);
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let s = &self.status;
        if !s.repo {
            return div()
                .size_full()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(format!("{} is not a git repository.", self.cwd.display()))
                .into_any_element();
        }
        let button = |id: &'static str, label: String, cx: &mut Context<Self>| {
            div()
                .id(id)
                .px_2()
                .py(px(2.))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .text_xs()
                .cursor_pointer()
                .hover(|d| d.bg(cx.theme().secondary))
                .child(label)
        };
        let tab = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .px_2()
                .py(px(2.))
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(on, |d| d.bg(theme.secondary))
                .child(label)
        };
        let tabs = div()
            .flex()
            .gap_1()
            .px_2()
            .pt_2()
            .child(
                tab("tab-changes", "Changes", self.tab == Tab::Changes).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.tab = Tab::Changes;
                        cx.notify();
                    },
                )),
            )
            .child(
                tab("tab-history", "History", self.tab == Tab::History)
                    .on_click(cx.listener(|this, _, _, cx| this.show_history(cx))),
            );
        if self.tab == Tab::History {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .bg(theme.background)
                .child(tabs)
                .child(self.history(cx))
                .into_any_element();
        }
        let branch = s.branch.clone().unwrap_or_else(|| "detached HEAD".into());
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(theme.border)
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Changes"))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("⑂ {branch}")),
            )
            .child(div().flex_1())
            .child(
                button(
                    "pull",
                    if s.behind > 0 {
                        format!("Pull ↓{}", s.behind)
                    } else {
                        "Pull".into()
                    },
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.pull(cx))),
            )
            .child(
                button(
                    "push",
                    if s.ahead > 0 {
                        format!("Push ↑{}", s.ahead)
                    } else {
                        "Push".into()
                    },
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.asking = Some(GitAsk::Push);
                    cx.notify();
                })),
            );
        let files = self.files.iter().enumerate().map(|(i, f)| {
            let (letter, color) = match f.status {
                DiffStatus::Added => ("A", theme.success),
                DiffStatus::Deleted => ("D", theme.danger),
                DiffStatus::Renamed => ("R", theme.info),
                DiffStatus::Modified => ("M", theme.warning),
            };
            div()
                .id(SharedString::from(format!("file-{i}")))
                .px_2()
                .py(px(2.))
                .flex()
                .gap_2()
                .text_xs()
                .cursor_pointer()
                .when(i == self.selected, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
                .child(div().w(px(12.)).text_color(color).child(letter))
                .child(div().flex_1().truncate().child(f.path.clone()))
                .child(
                    div()
                        .text_color(theme.success)
                        .child(format!("+{}", f.added)),
                )
                .child(
                    div()
                        .text_color(theme.danger)
                        .child(format!("−{}", f.removed)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected = i;
                    cx.notify();
                }))
        });
        let untracked = self.untracked.iter().map(|p| {
            div()
                .px_2()
                .py(px(2.))
                .flex()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .w(px(12.))
                        .text_color(theme.muted_foreground)
                        .child("?"),
                )
                .child(div().truncate().child(p.clone()))
        });
        let diff_lines: Vec<AnyElement> = self
            .files
            .get(self.selected)
            .map(|f| {
                if f.binary {
                    return vec![div()
                        .p_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Binary file")
                        .into_any_element()];
                }
                f.lines
                    .iter()
                    .take(2000)
                    .map(|l| {
                        let (bg, fg, sign) = match l.kind {
                            DiffLineKind::Add => {
                                (Some(theme.success.opacity(0.12)), theme.foreground, "+")
                            }
                            DiffLineKind::Del => {
                                (Some(theme.danger.opacity(0.12)), theme.foreground, "-")
                            }
                            DiffLineKind::Hunk => {
                                (Some(theme.secondary), theme.muted_foreground, "")
                            }
                            DiffLineKind::Meta => (None, theme.muted_foreground, ""),
                            DiffLineKind::Ctx => (None, theme.foreground, " "),
                        };
                        div()
                            .px_2()
                            .text_xs()
                            .font_family(crate::terminal::mono(cx))
                            .whitespace_nowrap()
                            .text_color(fg)
                            .when_some(bg, |d, bg| d.bg(bg))
                            .child(format!("{sign}{}", l.text))
                            .into_any_element()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let empty = self.files.is_empty() && self.untracked.is_empty();
        let _ = window;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(tabs)
            .child(header)
            .when(empty, |d| {
                d.child(
                    div()
                        .p_4()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No changes."),
                )
            })
            .child(
                div()
                    .id("files")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(files)
                    .children(untracked),
            )
            .when(self.truncated, |d| {
                d.child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(theme.warning)
                        .child("The diff is too large to show in full."),
                )
            })
            .child(
                div()
                    .id("diff")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_scroll()
                    .border_t_1()
                    .border_color(theme.border)
                    .children(diff_lines),
            )
            .child(
                div()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .when_some(self.asking.zip(s.branch.clone()), |d, (what, branch)| {
                        d.child(self.confirm(what, &branch, cx))
                    })
                    .child(Input::new(&self.message))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .truncate()
                                    .when_some(self.note.clone(), |d, (ok, n)| {
                                        d.text_color(if ok {
                                            theme.muted_foreground
                                        } else {
                                            theme.danger
                                        })
                                        .child(n)
                                    })
                                    .when_some(self.busy, |d, b| {
                                        d.text_color(theme.muted_foreground).child(format!("{b}…"))
                                    }),
                            )
                            .child(
                                div()
                                    .id("commit")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(theme.primary)
                                    .text_color(theme.primary_foreground)
                                    .text_sm()
                                    .cursor_pointer()
                                    .child("Commit all")
                                    .on_click(cx.listener(|this, _, _, cx| this.commit(cx))),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl ChangesPanel {
    /// "Push main to its remote?" with Cancel and the action (`dp-ask`).
    fn confirm(&self, what: GitAsk, branch: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let n = self.status.changed;
        let (text, label) = match what {
            GitAsk::Push => (format!("Push {branch} to its remote?"), "Push"),
            GitAsk::Pull if n > 0 => (
                format!(
                    "{n} uncommitted {} in this folder. Pull {branch} anyway? git stops if they are in the way.",
                    if n == 1 { "change" } else { "changes" }
                ),
                "Pull",
            ),
            _ => (format!("Pull {branch} from its remote?"), "Pull"),
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .child(div().flex_1().child(text))
            .child(
                div()
                    .id("ask-cancel")
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .child("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.asking = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("ask-ok")
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .bg(theme.primary)
                    .text_color(theme.primary_foreground)
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.run_asked(what, cx))),
            )
            .into_any_element()
    }

    fn history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if let Some(e) = &self.history_error {
            return div()
                .p_4()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element();
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let rows = self.commits.iter().zip(&self.graph).map(|(c, g)| {
            let sha = c.sha.clone();
            let row = g.clone();
            let graph_w = (row.width.max(1) as f32) * LANE_W + 4.0;
            let labels = ref_labels(&c.refs).into_iter().map(|r| {
                let color = match r.kind {
                    RefKind::Head => theme.primary,
                    RefKind::Branch => theme.success,
                    RefKind::Remote => theme.info,
                    RefKind::Tag => theme.warning,
                };
                div()
                    .px_1()
                    .rounded_sm()
                    .text_xs()
                    .border_1()
                    .border_color(color)
                    .text_color(color)
                    .child(r.text)
            });
            div()
                .id(SharedString::from(format!("commit-{}", c.sha)))
                .h(px(ROW_H))
                .flex()
                .items_center()
                .gap_2()
                .pr_2()
                .text_xs()
                .cursor_pointer()
                .when(self.shown_commit.as_ref() == Some(&c.sha), |d| {
                    d.bg(theme.secondary)
                })
                .hover(|d| d.bg(theme.secondary))
                .child(
                    canvas(
                        |_, _, _| (),
                        move |b, _, window, _| draw_graph_row(&row, b, window),
                    )
                    .w(px(graph_w))
                    .h_full()
                    .flex_shrink_0(),
                )
                .children(labels)
                .child(div().flex_1().truncate().child(c.subject.clone()))
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(c.author.clone()),
                )
                .child(
                    div()
                        .w(px(56.))
                        .text_right()
                        .text_color(theme.muted_foreground)
                        .child(ago(c.time, now)),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.show_commit(sha.clone(), cx)))
        });
        let files = self.commit_files.iter().map(|f| {
            div()
                .px_2()
                .flex()
                .gap_2()
                .text_xs()
                .child(div().flex_1().truncate().child(f.path.clone()))
                .child(
                    div()
                        .text_color(theme.success)
                        .child(format!("+{}", f.added)),
                )
                .child(
                    div()
                        .text_color(theme.danger)
                        .child(format!("−{}", f.removed)),
                )
        });
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(
                div()
                    .id("history")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .when(self.more, |d| {
                d.child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Older commits aren't shown yet."),
                )
            })
            .when(!self.commit_files.is_empty(), |d| {
                d.child(
                    div()
                        .id("commit-files")
                        .max_h(px(200.))
                        .overflow_y_scroll()
                        .border_t_1()
                        .border_color(theme.border)
                        .py_1()
                        .children(files),
                )
            })
            .into_any_element()
    }
}

/// One row of the commit graph: the lines through its top and bottom halves, then the dot.
fn draw_graph_row(row: &GraphRow, b: Bounds<Pixels>, window: &mut Window) {
    let x = |col: usize| b.left() + px(LANE_W * col as f32 + LANE_W / 2.0 + 2.0);
    let (top, mid, bottom) = (b.top(), b.top() + b.size.height / 2.0, b.bottom());
    let lane = |i: usize| -> Hsla { rgb(LANES[i % LANES.len()]).into() };
    for (edges, y0, y1) in [(&row.top, top, mid), (&row.bottom, mid, bottom)] {
        for e in edges {
            let mut p = PathBuilder::stroke(px(1.5));
            p.move_to(point(x(e.from), y0));
            p.line_to(point(x(e.to), y1));
            if let Ok(path) = p.build() {
                window.paint_path(path, lane(e.color));
            }
        }
    }
    let r = px(3.5);
    window.paint_quad(
        fill(
            Bounds::new(point(x(row.col) - r, mid - r), size(r * 2.0, r * 2.0)),
            lane(row.color),
        )
        .corner_radii(r),
    );
}

/// `5m`, `3h`, `2d`, `4w`: how long ago a commit was made.
fn ago(ms: i64, now: i64) -> String {
    let s = ((now - ms) / 1000).max(0);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=2_419_199 => format!("{}d", s / 86_400),
        _ => format!("{}w", s / 604_800),
    }
}
