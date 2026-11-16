//! A folder picker inside the app (`FolderPicker.tsx`), for New chat and New project instead of
//! the system file chooser: a breadcrumb, the folders of the current one (filter as you type,
//! or type/paste a path to jump), git repos marked, pinned and recent folders on top. Keyboard
//! first: ↑/↓, Enter opens, Backspace goes up, Ctrl+Enter picks, Esc goes back.

use crate::a11y::A11y as _;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::config::Config;
use wraithgrid_core::folders::{
    crumbs, join_path, list_folder, looks_like_path, split_folder, toggle_pin, FolderEntry,
    FolderList,
};
use wraithgrid_core::paths::{contract_home, home_dir, resolve_user_path};

pub enum FolderPickerEvent {
    Pick(String),
    /// Back to the dialog, nothing picked.
    Close,
    /// Pin or unpin this folder (`settings.pinnedFolders`).
    TogglePin(String),
}

pub struct FolderPicker {
    dir: String,
    list: Option<FolderList>,
    /// Which listing is wanted; older answers are dropped.
    asked: usize,
    query: Entity<InputState>,
    active: usize,
    hidden: bool,
    note: Option<String>,
    recent: Vec<String>,
    pinned: Vec<String>,
    scroll: ScrollHandle,
    _subscription: Subscription,
}

const MOD_KEY: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl"
};

impl FolderPicker {
    pub fn new(
        start: String,
        recent: Vec<String>,
        pinned: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter, or type a path like ~/code")
        });
        let sub = cx.subscribe(&query, |this, _, e: &InputEvent, cx| {
            if let InputEvent::Change = e {
                this.active = 0;
                this.note = None;
                cx.notify();
            }
        });
        window.focus(&query.focus_handle(cx), cx);
        let mut this = FolderPicker {
            dir: start,
            list: None,
            asked: 0,
            query,
            active: 0,
            hidden: false,
            note: None,
            recent,
            pinned,
            scroll: ScrollHandle::new(),
            _subscription: sub,
        };
        this.load(cx);
        this
    }

    /// Lists `self.dir` off the main thread (a big or slow folder must not freeze the window).
    fn load(&mut self, cx: &mut Context<Self>) {
        self.asked += 1;
        let asked = self.asked;
        let dir = self.dir.clone();
        let task = cx.background_spawn(async move { list_folder(&dir, &home_dir()) });
        cx.spawn(async move |this, cx| {
            let list = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.asked == asked {
                    this.list = Some(list);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn go(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.note = None;
        self.active = 0;
        self.dir = path;
        self.list = None;
        self.query.update(cx, |q, cx| q.set_value("", window, cx));
        window.focus(&self.query.focus_handle(cx), cx);
        self.load(cx);
        cx.notify();
    }

    fn here(&self) -> String {
        self.list
            .as_ref()
            .map_or_else(|| self.dir.clone(), |l| l.path.clone())
    }

    fn typed(&self, cx: &App) -> String {
        self.query.read(cx).value().trim().to_string()
    }

    fn items(&self, cx: &App) -> Vec<FolderEntry> {
        let q = self.typed(cx).to_lowercase();
        let Some(Ok(listed)) = self.list.as_ref().map(|l| &l.result) else {
            return vec![];
        };
        if looks_like_path(&q) {
            return vec![];
        }
        listed
            .entries
            .iter()
            .filter(|e| {
                (self.hidden || !e.hidden || q.starts_with('.'))
                    && (q.is_empty() || e.name.to_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }

    /// Picks `target` if it lists; otherwise shows where it failed and why.
    fn pick(&mut self, target: String, window: &mut Window, cx: &mut Context<Self>) {
        let l = list_folder(&target, &home_dir());
        if l.ok() {
            cx.emit(FolderPickerEvent::Pick(l.path));
        } else {
            let why = l.result.clone().err();
            self.go(l.path, window, cx);
            self.note = why;
        }
    }

    pub fn set_pinned(&mut self, pinned: Vec<String>, cx: &mut Context<Self>) {
        self.pinned = pinned;
        cx.notify();
    }

    fn system_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            let home = home_dir().to_string_lossy().into_owned();
            let p = contract_home(&dir.to_string_lossy(), &home);
            let _ = this.update(cx, |_, cx| cx.emit(FolderPickerEvent::Pick(p)));
        })
        .detach();
    }

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.typed(cx);
        let jump = looks_like_path(&typed);
        let items = self.items(cx);
        let at = self.active.min(items.len().saturating_sub(1));
        match e.keystroke.key.as_str() {
            "escape" => cx.emit(FolderPickerEvent::Close),
            "enter" if e.keystroke.modifiers.secondary(cx) => {
                let target = if jump { typed } else { self.here() };
                self.pick(target, window, cx);
            }
            "enter" => {
                if jump {
                    self.go(typed, window, cx);
                } else if let (Some(e), Some(l)) = (items.get(at), &self.list) {
                    let p = join_path(&l.path, &e.name);
                    self.go(p, window, cx);
                }
            }
            "down" => {
                self.active = (at + 1).min(items.len().saturating_sub(1));
                self.scroll.scroll_to_item(self.active);
                cx.notify();
            }
            "up" => {
                self.active = at.saturating_sub(1);
                self.scroll.scroll_to_item(self.active);
                cx.notify();
            }
            "backspace" if typed.is_empty() && self.query.read(cx).value().is_empty() => {
                match self.list.as_ref().and_then(|l| l.parent.clone()) {
                    Some(parent) => self.go(parent, window, cx),
                    None => return,
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl EventEmitter<FolderPickerEvent> for FolderPicker {}

impl Render for FolderPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let here = self.here();
        let typed = self.typed(cx);
        let jump = looks_like_path(&typed);
        let items = self.items(cx);
        let at = self.active.min(items.len().saturating_sub(1));
        let is_pinned = self.pinned.contains(&here);
        let ok = self.list.as_ref().is_some_and(FolderList::ok);
        let listed = self.list.as_ref().and_then(|l| l.result.as_ref().ok());

        let all = crumbs(&here);
        let n = all.len();
        let mut crumb_row = div().flex().flex_wrap().items_center().gap_0p5().text_sm();
        for (i, (label, path)) in all.into_iter().enumerate() {
            let last = i + 1 == n;
            let path_is_root = path == "/";
            crumb_row = crumb_row.child(
                div()
                    .id(SharedString::from(format!("fp-crumb-{i}")))
                    .a11y_button(path.clone())
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.secondary))
                    .when(last, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .when(!last, |d| d.text_color(theme.muted_foreground))
                    .child(label)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.go(path.clone(), window, cx)),
                    ),
            );
            // `/` is its own separator.
            if !last && (i > 0 || !path_is_root) {
                crumb_row = crumb_row.child(div().text_color(theme.muted_foreground).child("/"));
            }
        }
        if let Some(branch) = listed.and_then(|l| l.branch.clone()) {
            crumb_row = crumb_row.child(
                div()
                    .ml_2()
                    .px_1p5()
                    .rounded_sm()
                    .text_xs()
                    .bg(theme.secondary)
                    .text_color(theme.muted_foreground)
                    .child(format!("⎇ {branch}")),
            );
        }

        let shortcuts = |list: Vec<&String>| -> Vec<String> {
            list.into_iter()
                .filter(|p| **p != here)
                .take(4)
                .cloned()
                .collect()
        };
        let top: Vec<(String, &str)> = if typed.is_empty() {
            let pinned = shortcuts(self.pinned.iter().collect());
            let recent = shortcuts(
                self.recent
                    .iter()
                    .filter(|r| !self.pinned.contains(r))
                    .collect(),
            );
            pinned
                .into_iter()
                .map(|p| (p, "Pinned"))
                .chain(recent.into_iter().map(|p| (p, "Recent")))
                .collect()
        } else {
            vec![]
        };
        let quick = top.into_iter().enumerate().map(|(i, (p, kind))| {
            let (name, parent) = split_folder(&p);
            div()
                .id(SharedString::from(format!("fp-q-{i}")))
                .a11y_button(format!("{kind}: {p}"))
                .px_2()
                .py_0p5()
                .rounded_md()
                .flex()
                .gap_2()
                .items_center()
                .text_sm()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(
                    div()
                        .w(px(48.))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(kind),
                )
                .child(div().child(name))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(parent),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.go(p.clone(), window, cx)))
        });
        let quick: Vec<_> = quick.collect();

        let note = |text: String, err: bool| {
            div()
                .px_2()
                .py_1()
                .text_sm()
                .text_color(if err {
                    theme.danger
                } else {
                    theme.muted_foreground
                })
                .child(text)
        };
        let error = self
            .note
            .clone()
            .or_else(|| self.list.as_ref().and_then(|l| l.result.clone().err()));
        let body = if jump {
            note(
                format!("Enter goes to {typed}; {MOD_KEY}+Enter picks it."),
                false,
            )
            .into_any_element()
        } else if let Some(e) = error {
            note(e, true).into_any_element()
        } else if self.list.is_none() {
            note("Loading…".into(), false).into_any_element()
        } else if items.is_empty() {
            let some_hidden = listed.is_some_and(|l| l.entries.iter().any(|e| e.hidden));
            note(
                format!(
                    "{}{}",
                    if typed.is_empty() {
                        "No folders here."
                    } else {
                        "No folder matches."
                    },
                    if !self.hidden && some_hidden {
                        " Hidden ones are off."
                    } else {
                        ""
                    }
                ),
                false,
            )
            .into_any_element()
        } else {
            let rows = items.into_iter().enumerate().map(|(i, e)| {
                let path = join_path(&here, &e.name);
                div()
                    .id(SharedString::from(format!("fp-it-{i}")))
                    .role(Role::ListItem)
                    .aria_label(if e.git {
                        format!("{} (git)", e.name)
                    } else {
                        e.name.clone()
                    })
                    .aria_selected(i == at)
                    .when(i == at, |d| d.aria_active_descendant())
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .cursor_pointer()
                    .when(i == at, |d| d.bg(theme.list_active))
                    .when(e.hidden, |d| d.text_color(theme.muted_foreground))
                    .child(div().text_color(theme.muted_foreground).child("▸"))
                    .child(div().flex_1().truncate().child(e.name.clone()))
                    .when(e.git, |d| {
                        d.child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .text_xs()
                                .border_1()
                                .border_color(theme.border)
                                .text_color(theme.muted_foreground)
                                .child("git"),
                        )
                    })
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.active != i {
                            this.active = i;
                            cx.notify();
                        }
                    }))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.go(path.clone(), window, cx)),
                    )
            });
            div()
                .id("fp-list")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(rows)
                .into_any_element()
        };

        let reveal = resolve_user_path(&here, &home_dir());
        div()
            .id("folder-picker")
            .a11y_dialog("Choose a folder")
            .w(px(560.))
            .h(px(480.))
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .capture_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Choose a folder"),
                    )
                    .child(
                        div()
                            .id("fp-back")
                            .a11y_button("Back")
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.secondary))
                            .child("✕")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(FolderPickerEvent::Close))),
                    ),
            )
            .child(crumb_row)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.query)))
                    .child(
                        button(
                            "fp-pin",
                            if is_pinned { "Pinned" } else { "Pin" },
                            is_pinned,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.emit(FolderPickerEvent::TogglePin(this.here()))
                        })),
                    )
                    .child(
                        Checkbox::new("fp-hidden")
                            .checked(self.hidden)
                            .label("Hidden")
                            .on_click(cx.listener(|this, on: &bool, _, cx| {
                                this.hidden = *on;
                                cx.notify();
                            })),
                    ),
            )
            .when(!quick.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .pb_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .children(quick),
                )
            })
            .child(div().flex_1().min_h_0().flex().flex_col().child(body).when(
                listed.is_some_and(|l| l.truncated),
                |d| {
                    d.child(note(
                        "Only the first folders are listed; filter to find others.".into(),
                        false,
                    ))
                },
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "↑↓ move · Enter open · ⌫ up · {MOD_KEY}+Enter pick · Esc back"
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        button("fp-files", "Open in Files", false, cx)
                            .on_click(move |_, _, cx| cx.open_with_system(&reveal)),
                    )
                    .child(button("fp-system", "System dialog…", false, cx).on_click(
                        cx.listener(|this, _, window, cx| this.system_dialog(window, cx)),
                    ))
                    .child(
                        button("fp-use", "Use this folder", true, cx)
                            .when(!ok, |d| d.opacity(0.5))
                            .when(ok, |d| {
                                d.on_click(cx.listener(|this, _, window, cx| {
                                    let here = this.here();
                                    this.pick(here, window, cx)
                                }))
                            }),
                    ),
            )
    }
}

/// A small bordered button; `primary` fills it with the accent.
fn button(id: &'static str, text: &'static str, primary: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .a11y_button(text)
        .px_2()
        .py_0p5()
        .rounded_md()
        .border_1()
        .text_sm()
        .cursor_pointer()
        .border_color(if primary { theme.primary } else { theme.border })
        .when(primary, |d| {
            d.bg(theme.primary).text_color(theme.primary_foreground)
        })
        .when(!primary, |d| d.hover(|d| d.bg(theme.secondary)))
        .child(text)
}

/// A dialog's folder field with "Choose…": while `picker` is set the dialog shows it instead
/// of its own body; the pick lands in the field.
pub struct Picking {
    pub picker: Option<Entity<FolderPicker>>,
    pinned: Vec<String>,
    recent: Vec<String>,
    default_cwd: String,
    _sub: Option<Subscription>,
}

impl Picking {
    pub fn new(config: &Config) -> Self {
        let home = home_dir().to_string_lossy().into_owned();
        Picking {
            picker: None,
            pinned: config.settings.pinned_folders.clone(),
            recent: config
                .recent_folders
                .iter()
                .map(|f| contract_home(f, &home))
                .collect(),
            default_cwd: config.settings.default_cwd.clone(),
            _sub: None,
        }
    }

    /// Opens the picker at the field's folder (or the default one). `slot` finds this
    /// `Picking` in the dialog; `pin` saves new pins.
    pub fn open<T: 'static>(
        folder: &Entity<InputState>,
        slot: fn(&mut T) -> &mut Picking,
        pin: fn(Vec<String>, &mut Context<T>),
        this: &mut T,
        window: &mut Window,
        cx: &mut Context<T>,
    ) {
        let p = slot(this);
        let typed = folder.read(cx).value().trim().to_string();
        let start = if typed.is_empty() {
            p.default_cwd.clone()
        } else {
            typed
        };
        let (recent, pinned) = (p.recent.clone(), p.pinned.clone());
        let picker = cx.new(|cx| FolderPicker::new(start, recent, pinned, window, cx));
        let field = folder.clone();
        let sub = cx.subscribe_in(&picker, window, move |this, picker, e, window, cx| {
            let done = |this: &mut T, window: &mut Window, cx: &mut Context<T>| {
                let p = slot(this);
                p.picker = None;
                p._sub = None;
                window.focus(&field.focus_handle(cx), cx);
                cx.notify();
            };
            match e {
                FolderPickerEvent::Pick(path) => {
                    field.update(cx, |f, cx| f.set_value(path.clone(), window, cx));
                    done(this, window, cx);
                }
                FolderPickerEvent::Close => done(this, window, cx),
                FolderPickerEvent::TogglePin(path) => {
                    let p = slot(this);
                    p.pinned = toggle_pin(&p.pinned, path);
                    let pins = p.pinned.clone();
                    picker.update(cx, |pk, cx| pk.set_pinned(pins.clone(), cx));
                    pin(pins, cx);
                }
            }
        });
        let p = slot(this);
        p.picker = Some(picker);
        p._sub = Some(sub);
        cx.notify();
    }
}
