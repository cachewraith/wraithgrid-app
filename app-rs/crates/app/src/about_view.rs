//! About (`AboutView.tsx`, Settings → About, #124): the Updates card with each install step
//! (Download · Verify · Ready · Restart), and What's new: the versions beside the chosen one's
//! notes, New as cards and Fixed as a list. The update itself is the window's (`Shell`'s
//! updater, the same one the title bar button drives); this view shows it and asks it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::a11y::A11y as _;
use wraithgrid_core::paths::current_user_data_dir;
use wraithgrid_core::release_notes::{
    cmp_versions, note_parts, parse_whats_new, release_page_url, NoteParts, NotesView,
};
use wraithgrid_core::update::{self, CheckResult, PackageKind, ReleaseInfo};

use crate::runtime::APP_VERSION;
use crate::settings::SettingsView;
use crate::shell::{Shell, UpdateState};

const WHATS_NEW: &str = include_str!("../../../../docs/WHATS-NEW.md");
/// Unauthenticated GitHub API calls are limited to 60 an hour: repeated clicks wait.
const MIN_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const STEPS: [&str; 4] = ["Download", "Verify", "Ready", "Restart"];

fn notes_file() -> PathBuf {
    current_user_data_dir().join("release-notes.json")
}

/// Records this run in release-notes.json (the version that ran last), once at launch; the
/// version that ran before when the app was updated.
pub fn record_launch() -> Option<String> {
    wraithgrid_core::release_notes::start(&notes_file(), APP_VERSION)
}

/// After every check that reached GitHub: keeps the latest release's notes for after the
/// update; a launch check that found a newer release returns the OS notice to show, once per
/// version (`update-notify.ts`).
pub fn after_check(result: &CheckResult, launch: bool) -> Option<(String, String)> {
    let (CheckResult::Available { latest, .. } | CheckResult::Current { latest, .. }) = result
    else {
        return None;
    };
    wraithgrid_core::release_notes::remember(&notes_file(), latest);
    let state = current_user_data_dir().join("update-notified.json");
    (launch && matches!(result, CheckResult::Available { .. }))
        .then(|| wraithgrid_core::release_notes::notice_once(&state, APP_VERSION, latest))
        .flatten()
}

/// Gives Settings its About tab, reading and driving `shell`'s updater.
pub fn attach(
    settings: &Entity<SettingsView>,
    shell: WeakEntity<Shell>,
    on_launch: bool,
    cx: &mut App,
) {
    let about = cx.new(|cx| AboutView::new(shell, on_launch, cx));
    settings.update(cx, |s, cx| s.set_about(about, cx));
}

pub enum AboutEvent {
    /// Settings → check for updates when Wraithgrid starts.
    CheckOnLaunch(bool),
}

pub struct AboutView {
    shell: WeakEntity<Shell>,
    kind: PackageKind,
    on_launch: bool,
    notes: NotesView,
    bundled_parts: HashMap<String, NoteParts>,
    /// The newest release a check found, kept while it downloads.
    latest: Option<ReleaseInfo>,
    picked: Option<String>,
    later: bool,
    last_check: Option<Instant>,
    markdown: HashMap<String, Entity<TextViewState>>,
    _observe: Option<Subscription>,
}

struct Entry {
    version: String,
    title: String,
    date: Option<String>,
    notes: String,
    tag: Option<&'static str>,
}

impl AboutView {
    fn new(shell: WeakEntity<Shell>, on_launch: bool, cx: &mut Context<Self>) -> Self {
        let bundled = parse_whats_new(WHATS_NEW);
        let notes = wraithgrid_core::release_notes::view(&notes_file(), APP_VERSION, &bundled);
        let exe = std::env::current_exe().unwrap_or_default();
        let observe = shell
            .upgrade()
            .map(|s| cx.observe(&s, |_, _, cx| cx.notify()));
        AboutView {
            shell,
            kind: update::package_kind(&exe, |k| std::env::var(k).ok()),
            on_launch,
            notes,
            bundled_parts: HashMap::new(),
            latest: None,
            picked: None,
            later: false,
            last_check: None,
            markdown: HashMap::new(),
            _observe: observe,
        }
    }

    fn state(&self, cx: &App) -> UpdateState {
        self.shell
            .upgrade()
            .map_or(UpdateState::Idle, |s| s.read(cx).update_state().clone())
    }

    fn with_shell(&self, cx: &mut App, f: impl FnOnce(&mut Shell, &mut Context<Shell>)) {
        if let Some(shell) = self.shell.upgrade() {
            shell.update(cx, f);
        }
    }

    fn check(&mut self, cx: &mut Context<Self>) {
        if self
            .last_check
            .is_some_and(|t| t.elapsed() < MIN_CHECK_INTERVAL)
        {
            return;
        }
        self.last_check = Some(Instant::now());
        self.with_shell(cx, |s, cx| s.check_updates(false, cx));
    }

    fn open_page(&self, version: &str, cx: &mut App) {
        let url = self
            .latest
            .as_ref()
            .filter(|r| r.version == version)
            .map_or_else(|| release_page_url(version), |r| r.url.clone());
        cx.open_url(&url);
    }

    fn parts(&mut self, notes: &str) -> NoteParts {
        self.bundled_parts
            .entry(notes.to_string())
            .or_insert_with(|| note_parts(notes))
            .clone()
    }

    fn md(&mut self, text: &str, cx: &mut Context<Self>) -> TextView {
        let state = self
            .markdown
            .entry(text.to_string())
            .or_insert_with(|| cx.new(|cx| TextViewState::markdown(text, cx)))
            .clone();
        TextView::new(&state).selectable(true)
    }

    /// One clear state at a time: up to date · available (its headlines) · downloading ·
    /// verifying · ready to restart · restarting · failed.
    fn updates_card(&mut self, state: &UpdateState, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let manual = self.kind == PackageKind::Manual;
        let mut buttons: Vec<AnyElement> = Vec::new();
        let mut steps: Option<Div> = None;
        let mut failed = false;
        let mut headlines: Option<(String, Vec<String>)> = None;
        let ok = |t: String| status_line("✓", theme.success, t);
        let warn = |t: String| status_line("!", theme.danger, t);
        let target: Option<String>;
        let status: Div = match state {
            UpdateState::Downloading {
                version, percent, ..
            } => {
                target = Some(version.clone());
                // The file is checked against SHA256SUMS once it's all here: the Verify step.
                let verifying = *percent >= 100;
                buttons.push(
                    btn("upd-cancel", "Cancel", false, cx)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.with_shell(cx, |s, _| s.cancel_update())
                        }))
                        .into_any_element(),
                );
                let pct = *percent;
                steps = Some(steps_row(
                    if verifying { 1 } else { 0 },
                    false,
                    pct,
                    |i| match i {
                        0 => format!("{pct}%"),
                        1 if verifying => "checking…".into(),
                        _ => String::new(),
                    },
                    cx,
                ));
                plain(if verifying {
                    "Checking the download".to_string()
                } else {
                    format!("Downloading {version}")
                })
            }
            UpdateState::Ready { release, .. } => {
                target = Some(release.version.clone());
                if !self.later {
                    buttons.push(
                        btn("upd-later", "Later", false, cx)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.later = true;
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                }
                buttons.push(
                    btn("upd-restart", "↻ Restart now", true, cx)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.with_shell(cx, |s, cx| s.run_update(cx))
                        }))
                        .into_any_element(),
                );
                steps = Some(steps_row(2, true, 100, checked, cx));
                ok(if self.later {
                    format!(
                        "{} is downloaded. Restart when you are ready.",
                        release.version
                    )
                } else {
                    format!(
                        "{} is downloaded. Restart Wraithgrid to finish; running panes close.",
                        release.version
                    )
                })
            }
            UpdateState::Installing(version) => {
                target = Some(version.clone());
                steps = Some(steps_row(3, false, 100, checked, cx));
                plain(format!(
                    "Installing {version}. Wraithgrid restarts when it is done."
                ))
            }
            UpdateState::Failed { release, error } => {
                target = Some(release.version.clone());
                failed = true;
                if error != update::MANUAL {
                    buttons.push(
                        btn("upd-retry", "↻ Retry", false, cx)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.with_shell(cx, |s, cx| s.run_update(cx))
                            }))
                            .into_any_element(),
                    );
                }
                let v = release.version.clone();
                buttons.push(
                    btn("upd-page", "Open release page", true, cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.open_page(&v, cx)))
                        .into_any_element(),
                );
                warn(error.clone())
            }
            UpdateState::Checking => {
                target = None;
                plain("Checking GitHub for a newer release…".into())
            }
            UpdateState::Idle => {
                target = None;
                plain("Not checked yet.".into())
            }
            UpdateState::Error(e) => {
                target = None;
                warn(e.clone())
            }
            UpdateState::Current(at) => {
                target = None;
                let ago = ago(*at, wraithgrid_usage::now_ms());
                ok(format!(
                    "Up to date · checked {}",
                    if ago == "now" {
                        "just now".into()
                    } else {
                        format!("{ago} ago")
                    }
                ))
            }
            UpdateState::Available(r) => {
                target = Some(r.version.clone());
                let v = r.version.clone();
                buttons.push(if manual {
                    btn("upd-page", "Open release page", true, cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.open_page(&v, cx)))
                        .into_any_element()
                } else {
                    btn("upd-download", &format!("Download {}", r.version), true, cx)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.with_shell(cx, |s, cx| s.run_update(cx))
                        }))
                        .into_any_element()
                });
                let added = self.parts(&r.notes).added;
                if !added.is_empty() {
                    headlines = Some((
                        r.version.clone(),
                        added.into_iter().take(4).map(|i| i.name).collect(),
                    ));
                }
                let date = r.published_at.as_deref().and_then(format_date);
                status_line(
                    "↑",
                    theme.primary,
                    format!(
                        "{} is out{}",
                        r.version,
                        date.map(|d| format!(" · {d}")).unwrap_or_default()
                    ),
                )
            }
        };
        let busy = matches!(
            state,
            UpdateState::Downloading { .. }
                | UpdateState::Ready { .. }
                | UpdateState::Installing(_)
        );
        if !busy && !failed {
            let checking = matches!(state, UpdateState::Checking);
            buttons.push(
                btn(
                    "upd-check",
                    if checking {
                        "Checking…"
                    } else {
                        "↻ Check for updates"
                    },
                    false,
                    cx,
                )
                .when(checking, |d| d.opacity(0.6))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !checking {
                        this.check(cx);
                    }
                }))
                .into_any_element(),
            );
        }
        let on = self.on_launch;
        div()
            .id("updates")
            .p_5()
            .flex()
            .flex_col()
            .gap_4()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.35))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("WRAITHGRID"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_2()
                                    .text_2xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(APP_VERSION)
                                    .when_some(target.filter(|t| t != APP_VERSION), |d, t| {
                                        d.child(div().text_color(theme.muted_foreground).child("→"))
                                            .child(t)
                                    }),
                            )
                            .child(status.text_sm()),
                    )
                    .child(div().flex().gap_2().children(buttons)),
            )
            .children(steps)
            .when_some(headlines, |d, (version, items)| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("In {version}")),
                        )
                        .children(
                            items
                                .into_iter()
                                .map(|h| div().text_sm().child(format!("•  {h}"))),
                        ),
                )
            })
            .child(
                div()
                    .pt_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(
                        switch("upd-launch", on, cx)
                            .aria_label("Check when Wraithgrid starts")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.on_launch = !on;
                                cx.emit(AboutEvent::CheckOnLaunch(!on));
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1().child("Check when Wraithgrid starts"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if manual {
                                "GitHub releases"
                            } else {
                                "GitHub releases · on Linux your password is asked for"
                            }),
                    ),
            )
            .into_any_element()
    }

    /// Every known version, newest first: the release a check found, the installed one
    /// (listed even without notes, so it can say so), and the saved/bundled earlier ones.
    fn entries(&mut self) -> Vec<Entry> {
        let mut out: Vec<Entry> = Vec::new();
        if let Some(r) = self.latest.clone() {
            let title = self.parts(&r.notes).title.unwrap_or_default();
            out.push(Entry {
                version: r.version.clone(),
                title,
                date: r.published_at.clone(),
                notes: r.notes.clone(),
                tag: Some("new"),
            });
        }
        let saved = self.notes.installed.iter().chain(&self.notes.earlier);
        for n in saved {
            if !out.iter().any(|e| e.version == n.version) {
                out.push(Entry {
                    version: n.version.clone(),
                    title: n.title.clone(),
                    date: n.date.clone(),
                    notes: n.notes.clone(),
                    tag: (n.version == APP_VERSION).then_some("installed"),
                });
            }
        }
        if !out.iter().any(|e| e.version == APP_VERSION) {
            out.push(Entry {
                version: APP_VERSION.into(),
                title: String::new(),
                date: None,
                notes: String::new(),
                tag: Some("installed"),
            });
        }
        out.sort_by(|a, b| cmp_versions(&b.version, &a.version));
        out
    }

    fn whats_new(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let entries = self.entries();
        let sel = entries
            .iter()
            .position(|e| Some(&e.version) == self.picked.as_ref())
            .or_else(|| entries.iter().position(|e| e.tag == Some("new")))
            .or_else(|| entries.iter().position(|e| e.version == APP_VERSION))
            .unwrap_or(0);
        let list = entries.iter().enumerate().map(|(i, e)| {
            let parts = self.parts(&e.notes);
            let title = cap(if e.title.is_empty() {
                parts.title.as_deref().unwrap_or("Release notes")
            } else {
                &e.title
            });
            let version = e.version.clone();
            div()
                .id(SharedString::from(format!("ver-{}", e.version)))
                .a11y_button(format!("Version {}", e.version))
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .when(i == sel, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(e.version.clone())
                        .when_some(e.tag, |d, tag| {
                            d.child(
                                div()
                                    .px_1p5()
                                    .rounded_sm()
                                    .text_xs()
                                    .bg(if tag == "new" {
                                        theme.primary
                                    } else {
                                        theme.border
                                    })
                                    .text_color(if tag == "new" {
                                        theme.primary_foreground
                                    } else {
                                        theme.foreground
                                    })
                                    .child(tag),
                            )
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(title),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.picked = Some(version.clone());
                    cx.notify();
                }))
        });
        let list: Vec<_> = list.collect();
        let sel_version = entries[sel].version.clone();
        let sel_parts = self.parts(&entries[sel].notes);
        let empty =
            sel_parts.intro.is_empty() && sel_parts.added.is_empty() && sel_parts.fixed.is_empty();
        let notes = if empty {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(sel_version.clone()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No notes for this version."),
                )
                .into_any_element()
        } else {
            self.version_notes(&entries[sel], &sel_parts, false, cx)
        };
        // A patch release (x.y.1) says "everything from x.y.0": show that one below it.
        let mut nums = sel_version.split('.');
        let (maj, min, pat) = (nums.next(), nums.next(), nums.next());
        let base = match (maj, min, pat) {
            (Some(maj), Some(min), Some(pat)) if pat != "0" => {
                let want = format!("{maj}.{min}.0");
                entries.iter().find(|e| e.version == want)
            }
            _ => None,
        };
        let base = base.map(|b| {
            let parts = self.parts(&b.notes);
            self.version_notes(b, &parts, true, cx)
        });
        let page_version = sel_version.clone();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("What’s new"),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_5()
                    .child(
                        div()
                            .w(px(220.))
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .children(list)
                            .child(
                                div()
                                    .id("ab-all")
                                    .a11y_button("Release notes on GitHub")
                                    .mt_2()
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .hover(|d| d.bg(theme.secondary))
                                    .child("↗ Release notes on GitHub")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_page(&page_version, cx)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_6()
                            .child(notes)
                            .children(base.map(|b| {
                                div()
                                    .pt_4()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .child(b)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// One version's notes: intro, New as cards, Fixed as a list.
    fn version_notes(
        &mut self,
        e: &Entry,
        parts: &NoteParts,
        small: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let title = cap(if e.title.is_empty() {
            parts.title.as_deref().unwrap_or("")
        } else {
            &e.title
        });
        let date = e.date.as_deref().and_then(format_date);
        let intro = (!parts.intro.is_empty()).then(|| self.md(&parts.intro, cx));
        let cards: Vec<Div> = parts
            .added
            .chunks(2)
            .map(|pair| {
                let cells: Vec<Div> = pair
                    .iter()
                    .map(|it| {
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(it.name.clone()),
                            )
                            .child(div().text_sm().child(self.md(&it.text, cx)))
                    })
                    .collect();
                let lone = cells.len() == 1;
                div()
                    .flex()
                    .gap_3()
                    .children(cells)
                    .when(lone, |d| d.child(div().flex_1()))
            })
            .collect();
        let fixes: Vec<Div> = parts
            .fixed
            .iter()
            .map(|it| {
                div()
                    .flex()
                    .flex_col()
                    .text_sm()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(it.name.clone()),
                    )
                    .child(self.md(&it.text, cx))
            })
            .collect();
        let group = |label: &'static str, color: Hsla| {
            div()
                .mt_2()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color)
                .child(label)
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_3()
                    .child(
                        div()
                            .when(small, |d| d.text_base())
                            .when(!small, |d| d.text_lg())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if small && !title.is_empty() {
                                format!("{} · {title}", e.version)
                            } else {
                                e.version.clone()
                            }),
                    )
                    .when_some(date, |d, date| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(date),
                        )
                    }),
            )
            .when(!small && !title.is_empty(), |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(title.clone()),
                )
            })
            .when_some(intro, |d, intro| d.child(div().text_sm().child(intro)))
            .when(!cards.is_empty(), |d| {
                d.child(group("NEW", theme.primary))
                    .child(div().flex().flex_col().gap_3().children(cards))
            })
            .when(!fixes.is_empty(), |d| {
                d.child(group("FIXED", theme.success))
                    .child(div().flex().flex_col().gap_2().children(fixes))
            })
            .into_any_element()
    }
}

impl EventEmitter<AboutEvent> for AboutView {}

impl Render for AboutView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state(cx);
        if let UpdateState::Available(r)
        | UpdateState::Ready { release: r, .. }
        | UpdateState::Failed { release: r, .. } = &state
        {
            self.latest = Some(r.clone());
        }
        if !matches!(state, UpdateState::Ready { .. }) {
            self.later = false;
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_6()
            .child(self.updates_card(&state, cx))
            .child(self.whats_new(cx))
    }
}

// ---- Pieces --------------------------------------------------------------------------------

fn plain(text: String) -> Div {
    div().child(text)
}

fn status_line(icon: &'static str, color: Hsla, text: String) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_color(color)
                .font_weight(FontWeight::BOLD)
                .child(icon),
        )
        .child(text)
}

fn checked(i: usize) -> String {
    if i == 1 {
        "checked".into()
    } else {
        String::new()
    }
}

fn btn(id: &'static str, label: &str, primary: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .a11y_button(label.to_string())
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .text_sm()
        .cursor_pointer()
        .border_color(if primary { theme.primary } else { theme.border })
        .when(primary, |d| {
            d.bg(theme.primary).text_color(theme.primary_foreground)
        })
        .when(!primary, |d| d.hover(|d| d.bg(theme.secondary)))
        .child(label.to_string())
}

fn switch(id: &'static str, on: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .role(Role::Switch)
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .w(px(30.))
        .h(px(17.))
        .flex_shrink_0()
        .rounded_full()
        .p(px(2.))
        .cursor_pointer()
        .flex()
        .when(on, |d| d.justify_end())
        .bg(if on { theme.primary } else { theme.border })
        .child(div().size(px(13.)).rounded_full().bg(theme.background))
}

/// The four install steps: `at` is the one in progress, earlier ones are done.
fn steps_row(
    at: usize,
    ready: bool,
    percent: u8,
    detail: impl Fn(usize) -> String,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .gap_3()
        .children(STEPS.iter().enumerate().map(|(i, name)| {
            let done = i < at || (ready && i == at);
            let on = i == at && !done;
            let width = if done {
                1.0
            } else if on {
                if i == 0 {
                    percent as f32 / 100.0
                } else {
                    1.0
                }
            } else {
                0.0
            };
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(if done || on {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(if done {
                            format!("✓ {name}")
                        } else {
                            name.to_string()
                        }),
                )
                .child(
                    div()
                        .h(px(3.))
                        .w_full()
                        .rounded_full()
                        .bg(theme.border)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .w(relative(width))
                                .bg(if done { theme.success } else { theme.primary }),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(detail(i)),
                )
        }))
}

/// "web preview, …" → "Web preview, …": headings in WHATS-NEW.md start lower case.
fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// `5m`, `3h`, `2d`, `4w` (`lib/time.ts` `ago`).
fn ago(then: i64, now: i64) -> String {
    let s = ((now - then) / 1000).max(0);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=604_799 => format!("{}d", s / 86_400),
        _ => format!("{}w", s / 604_800),
    }
}

/// "Nov 5, 2026" from `2026-11-05` or an ISO time.
fn format_date(iso: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut p = iso.get(..10)?.split('-');
    let (y, m, d) = (
        p.next()?,
        p.next()?.parse::<usize>().ok()?,
        p.next()?.parse::<u8>().ok()?,
    );
    Some(format!("{} {d}, {y}", MONTHS.get(m.checked_sub(1)?)?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates_and_ago() {
        assert_eq!(
            super::format_date("2026-11-05").as_deref(),
            Some("Nov 5, 2026")
        );
        assert_eq!(
            super::format_date("2026-01-30T10:00:00Z").as_deref(),
            Some("Jan 30, 2026")
        );
        assert_eq!(super::format_date("junk"), None);
        assert_eq!(super::ago(0, 30_000), "now");
        assert_eq!(super::ago(0, 3 * 3_600_000), "3h");
        assert_eq!(super::cap("web preview"), "Web preview");
        // The bundled notes parse into versions with New items.
        let notes = wraithgrid_core::release_notes::parse_whats_new(super::WHATS_NEW);
        assert!(notes
            .iter()
            .any(|n| !super::note_parts(&n.notes).added.is_empty()));
    }
}
