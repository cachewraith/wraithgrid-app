//! Settings (`SettingsView.tsx`): the same tabs, rows and wording, editing the same fields of
//! config.json. Every change goes to the window as a field-level patch (`SettingsPatch`) it
//! applies to the live config, so a change made elsewhere meanwhile (zoom, pins, the sidebar's
//! width) is never put back. The view keeps a copy to draw from; the window refreshes it.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::a11y::A11y as _;
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::args::{format_args, parse_args};
use wraithgrid_core::config::{Config, Settings};
use wraithgrid_core::pane_env::Os;
use wraithgrid_core::paths::{
    config_file_path, contract_home, current_user_data_dir, home_dir, themes_root,
};
use wraithgrid_core::platform::{
    check_binary, find_on_path, list_shells, BinaryCheck, ShellOption,
};
use wraithgrid_core::shortcuts::{
    binding_from_press, binding_keys, binding_problem, bindings, default_binding, format_binding,
    label as shortcut_label, Press,
};
use wraithgrid_core::types::*;
use wraithgrid_mods::shared_config::{LinkOutcome, SharedReportEntry};
use wraithgrid_themes::{AppTheme, ThemeFile, ThemeName, ThemeProblem, UiColors};

use crate::about_view::{AboutEvent, AboutView};
use crate::machines_row::{MachinesEvent, MachinesRow};
use crate::theme_editor::{ThemeEditor, ThemeEditorEvent};
use crate::themes_gallery::{ThemesGallery, ThemesGalleryEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    General,
    Chat,
    Appearance,
    Agents,
    Advanced,
    Keyboard,
    About,
}

const TABS: &[(Tab, &str)] = &[
    (Tab::General, "General"),
    (Tab::Chat, "Chat"),
    (Tab::Appearance, "Appearance"),
    (Tab::Agents, "Agents & CLIs"),
    (Tab::Advanced, "Advanced"),
    (Tab::Keyboard, "Keyboard"),
    (Tab::About, "About"),
];

/// One change to the config, applied by the window to the live config (and by the view to its
/// copy). Only the fields it names are touched.
pub type SettingsPatch = Arc<dyn Fn(&mut Config) + Send + Sync>;

pub enum SettingsEvent {
    /// Apply this to the config and save it.
    Changed(SettingsPatch),
    /// Advanced → Machines added or removed one (H5).
    Machines(MachinesEvent),
    /// "Restart claude panes" after the shared instructions changed: the window restarts the
    /// agent panes that are running and tells the view how many (`set_restarted`).
    RestartClaudePanes,
    Close,
}

/// Text fields, each bound to one setting.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    DefaultCwd,
    ChatModel,
    ClaudePath,
    ShellPath,
    ShellArgs,
    CliPath(AgentCli),
}

/// A context menu in the Theme row: its actions (⋯), or one card's.
#[derive(Clone, PartialEq)]
enum ThemeMenu {
    Actions,
    Card(String),
}

/// How long typing pauses before a text field is saved (the Electron app's 400 ms).
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// claude's own aliases (from its model list); any other model name goes in the field below.
const FALLBACK_MODELS: &[(&str, &str)] =
    &[("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")];

/// Swatch color per accent, the same value as `--acc` in tokens.css.
fn accent_swatch(a: Accent) -> u32 {
    match a {
        Accent::Violet => 0x7c5cff,
        Accent::Blue => 0x3b82f6,
        Accent::Teal => 0x14b8a6,
        Accent::Amber => 0xf59e0b,
        Accent::Rose | Accent::Theme => 0xf43f5e,
    }
}

pub struct SettingsView {
    focus: FocusHandle,
    /// The tab list, so arrows, Home and End move between tabs.
    nav_focus: FocusHandle,
    tab: Tab,
    /// The config as last applied; drawn from, and refreshed by the window (`sync`).
    config: Config,
    inputs: Vec<(Field, Entity<InputState>)>,
    /// Text fields typed into and not saved yet: saved 400 ms after typing stops, on blur and
    /// on leaving the page.
    pending: Vec<(Field, SettingsPatch, Task<()>)>,
    /// A typed value the config can't take, shown under its field.
    errors: Vec<(Field, String)>,
    _subscriptions: Vec<Subscription>,
    machines: Entity<MachinesRow>,
    /// Customize… and Browse community… under Theme; the editor's subscription puts the
    /// chosen theme back if Settings closes while it is open.
    theme_editor: Option<(Entity<ThemeEditor>, Subscription)>,
    themes_gallery: Option<Entity<ThemesGallery>>,
    /// Built-in and the user's themes, and the files that could not be loaded.
    themes: Vec<AppTheme>,
    theme_problems: Vec<ThemeProblem>,
    theme_error: Option<String>,
    theme_menu: Option<(ThemeMenu, Point<Pixels>)>,
    confirm_delete: bool,
    /// The claude binary as found (None: still checking).
    claude: Option<BinaryCheck>,
    _claude_task: Option<Task<()>>,
    /// Shells found on this machine (None: still looking), and Custom… picked.
    shells: Option<Vec<ShellOption>>,
    shell_custom: bool,
    /// Shared instructions: switching, its error and what changed per account.
    shared_busy: bool,
    shared_error: Option<String>,
    shared_report: Option<Vec<SharedReportEntry>>,
    restarted: Option<usize>,
    installed_fonts: Vec<SharedString>,
    /// The shortcut whose next chord is being recorded (no key is an app shortcut meanwhile).
    recording: Option<ShortcutId>,
    shortcut_error: Option<(ShortcutId, String)>,
    about: Option<Entity<AboutView>>,
}

impl SettingsView {
    pub fn new(config: &Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let s = config.settings.clone();
        let mut fields: Vec<(Field, String, &str)> = vec![
            (Field::DefaultCwd, s.default_cwd.clone(), "~"),
            (
                Field::ChatModel,
                s.chat_model.clone().unwrap_or_default(),
                "Another model, e.g. claude-opus-4-1",
            ),
            (
                Field::ClaudePath,
                config.claude_path.clone(),
                "Override path (leave empty to use auto-detect)",
            ),
            (
                Field::ShellPath,
                s.shell_path.clone(),
                if cfg!(windows) {
                    "C:\\msys64\\usr\\bin\\zsh.exe or nu"
                } else {
                    "/usr/bin/nu or elvish"
                },
            ),
            (
                Field::ShellArgs,
                format_args(&s.shell_args),
                "Arguments (optional), e.g. --login -i",
            ),
        ];
        for cli in AgentCli::ALL.iter().filter(|c| **c != AgentCli::Claude) {
            let current = s
                .cli_paths
                .iter()
                .find(|(c, _)| c == cli)
                .map(|(_, p)| p.clone())
                .unwrap_or_default();
            fields.push((Field::CliPath(*cli), current, "on PATH"));
        }
        let mut inputs = Vec::new();
        let mut subs = Vec::new();
        for (field, value, placeholder) in fields {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            });
            subs.push(cx.subscribe_in(
                &input,
                window,
                move |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.field_changed(field, cx),
                    InputEvent::Blur | InputEvent::PressEnter { .. } => this.flush_field(field, cx),
                    InputEvent::Focus => {}
                },
            ));
            inputs.push((field, input));
        }
        let machines = cx.new(|cx| MachinesRow::new(config.machines.clone(), window, cx));
        subs.push(cx.subscribe(&machines, |_, _, e: &MachinesEvent, cx| {
            cx.emit(SettingsEvent::Machines(match e {
                MachinesEvent::Add(m) => MachinesEvent::Add(m.clone()),
                MachinesEvent::Remove(id) => MachinesEvent::Remove(id.clone()),
            }))
        }));
        let (themes, theme_problems) = wraithgrid_themes::all_themes(&themes_root(&home_dir()));
        let installed_fonts = cx
            .text_system()
            .all_font_names()
            .into_iter()
            .map(SharedString::from)
            .collect();
        let mut view = SettingsView {
            focus: cx.focus_handle(),
            nav_focus: cx.focus_handle(),
            tab: Tab::General,
            machines,
            config: config.clone(),
            inputs,
            pending: Vec::new(),
            errors: Vec::new(),
            _subscriptions: subs,
            theme_editor: None,
            themes_gallery: None,
            themes,
            theme_problems,
            theme_error: None,
            theme_menu: None,
            confirm_delete: false,
            claude: None,
            _claude_task: None,
            shells: None,
            shell_custom: false,
            shared_busy: false,
            shared_error: None,
            shared_report: None,
            restarted: None,
            installed_fonts,
            recording: None,
            shortcut_error: None,
            about: None,
        };
        view.detect_claude(cx);
        view.find_shells(cx);
        view
    }

    /// The config after the window applied a change (or anything else changed it).
    pub fn sync(&mut self, config: &Config, cx: &mut Context<Self>) {
        // A field still waiting to be saved keeps its typed value.
        let mut next = config.clone();
        for (_, patch, _) in &self.pending {
            patch(&mut next);
        }
        self.config = next;
        cx.notify();
    }

    /// Saves every field still waiting (before the page goes away).
    pub fn flush(&mut self, cx: &mut Context<Self>) {
        let fields: Vec<Field> = self.pending.iter().map(|(f, _, _)| *f).collect();
        for f in fields {
            self.flush_field(f, cx);
        }
    }

    /// The fields still waiting to be saved, for the window to apply before it drops the page
    /// (an event sent from a page being dropped may never arrive).
    // Called by the window before it closes Settings (shell.rs follow-up).
    #[allow(dead_code)]
    pub fn take_pending(&mut self) -> Vec<SettingsPatch> {
        self.pending.drain(..).map(|(_, patch, _)| patch).collect()
    }

    /// How many panes "Restart claude panes" restarted.
    pub fn set_restarted(&mut self, n: usize, cx: &mut Context<Self>) {
        self.restarted = Some(n);
        cx.notify();
    }

    /// The About tab's body; its "check on launch" switch is a setting like any other.
    pub fn set_about(&mut self, about: Entity<AboutView>, cx: &mut Context<Self>) {
        let sub = cx.subscribe(&about, |this, _, event: &AboutEvent, cx| match event {
            AboutEvent::CheckOnLaunch(on) => {
                let on = *on;
                this.update(cx, move |s| s.check_updates_on_launch = on);
            }
        });
        self._subscriptions.push(sub);
        self.about = Some(about);
    }

    /// Opens a tab by its label ("About").
    pub fn show_tab(&mut self, label: &str, cx: &mut Context<Self>) {
        if let Some((tab, _)) = TABS.iter().find(|(_, l)| *l == label) {
            self.tab = *tab;
            cx.notify();
        }
    }

    /// While a chord is being recorded the window must not act on app shortcuts.
    pub fn recording(&self) -> bool {
        self.recording.is_some()
    }

    /// `WRAITHGRID_OPEN=theme-editor|themes`: Appearance with the editor or gallery open.
    pub fn open_theme_tool(&mut self, editor: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Appearance;
        if editor {
            self.toggle_theme_editor(window, cx);
        } else {
            self.toggle_themes_gallery(window, cx);
        }
    }

    // ---- Saving ----------------------------------------------------------------------------

    /// Applies a change to the copy and sends it to the window.
    fn patch(
        &mut self,
        cx: &mut Context<Self>,
        change: impl Fn(&mut Config) + Send + Sync + 'static,
    ) {
        change(&mut self.config);
        cx.emit(SettingsEvent::Changed(Arc::new(change)));
        cx.notify();
    }

    fn update(
        &mut self,
        cx: &mut Context<Self>,
        change: impl Fn(&mut Settings) + Send + Sync + 'static,
    ) {
        self.patch(cx, move |c| change(&mut c.settings));
    }

    fn input(&self, field: Field) -> Entity<InputState> {
        self.inputs
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, i)| i.clone())
            .expect("input for field")
    }

    fn error(&self, field: Field) -> Option<String> {
        self.errors
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, e)| e.clone())
    }

    /// A field changed: checked now (a bad value shows why and waits), saved once typing stops.
    fn field_changed(&mut self, field: Field, cx: &mut Context<Self>) {
        let value = self.input(field).read(cx).value().to_string();
        self.errors.retain(|(f, _)| *f != field);
        self.pending.retain(|(f, _, _)| *f != field);
        match field_patch(field, &value) {
            Err(e) => self.errors.push((field, e)),
            Ok(patch) => {
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SAVE_DEBOUNCE).await;
                    let _ = this.update(cx, |this, cx| this.flush_field(field, cx));
                });
                self.pending.push((field, patch, task));
            }
        }
        cx.notify();
    }

    fn flush_field(&mut self, field: Field, cx: &mut Context<Self>) {
        let Some(i) = self.pending.iter().position(|(f, _, _)| *f == field) else {
            return;
        };
        let (_, patch, task) = self.pending.remove(i);
        // It may be the one running this.
        task.detach();
        patch(&mut self.config);
        cx.emit(SettingsEvent::Changed(patch));
        if field == Field::ClaudePath {
            self.detect_claude(cx);
        }
        cx.notify();
    }

    /// Sets a field's text (Browse…, a chip) and saves it at once.
    fn set_field(
        &mut self,
        field: Field,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input(field)
            .update(cx, |i, cx| i.set_value(value, window, cx));
        self.field_changed(field, cx);
        self.flush_field(field, cx);
    }

    /// A native file (or folder) dialog; the picked path, home as `~`, goes to `field`.
    fn browse(&mut self, field: Field, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let home = home_dir().to_string_lossy().into_owned();
            let path = contract_home(&path.to_string_lossy(), &home);
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_field(field, path, window, cx)
            });
        })
        .detach();
    }

    // ---- Agents & CLIs -----------------------------------------------------------------------

    /// `detectClaude`: the override or PATH, and whether `--version` answers.
    fn detect_claude(&mut self, cx: &mut Context<Self>) {
        self.claude = None;
        let path = self.config.claude_path.clone();
        let check = cx.background_spawn(async move {
            let env = crate::runtime::spawn_env();
            check_binary(
                "claude",
                &path,
                &home_dir(),
                &|k| env.get(k).cloned(),
                Os::current(),
            )
        });
        self._claude_task = Some(cx.spawn(async move |this, cx| {
            let found = check.await;
            let _ = this.update(cx, |this, cx| {
                this.claude = Some(found);
                cx.notify();
            });
        }));
    }

    fn find_shells(&mut self, cx: &mut Context<Self>) {
        let list = cx.background_spawn(async {
            let env = crate::runtime::spawn_env();
            let get = |k: &str| env.get(k).cloned();
            list_shells(
                Os::current(),
                &get,
                &|n| find_on_path(n, get, Os::current()),
                &|p| p.exists(),
                &|p| std::fs::read_to_string(p).ok(),
            )
        });
        cx.spawn(async move |this, cx| {
            let shells = list.await;
            let _ = this.update(cx, |this, cx| {
                this.shells = Some(shells);
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_shell(
        &mut self,
        path: String,
        args: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shell_custom = false;
        self.pending
            .retain(|(f, _, _)| *f != Field::ShellPath && *f != Field::ShellArgs);
        self.errors
            .retain(|(f, _)| *f != Field::ShellPath && *f != Field::ShellArgs);
        self.input(Field::ShellPath)
            .update(cx, |i, cx| i.set_value(path.clone(), window, cx));
        let text = format_args(&args);
        self.input(Field::ShellArgs)
            .update(cx, |i, cx| i.set_value(text, window, cx));
        self.update(cx, move |s| {
            s.shell_path = path.clone();
            s.shell_args = args.clone();
        });
    }

    /// Overall or per account: links every account, or takes the links out, then reports what
    /// changed in each (`setSharedMode`).
    fn choose_shared(&mut self, mode: SharedMode, cx: &mut Context<Self>) {
        if mode == self.config.settings.shared_mode || self.shared_busy {
            return;
        }
        self.update(cx, move |s| s.shared_mode = mode);
        self.shared_busy = true;
        self.shared_error = None;
        self.restarted = None;
        let config = self.config.clone();
        let task = cx.background_spawn(async move { crate::mods_glue::apply_shared_mode(&config) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.shared_busy = false;
                match result {
                    Ok(report) => this.shared_report = Some(report),
                    Err(e) => this.shared_error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ---- Themes ------------------------------------------------------------------------------

    /// The editor draws its draft over the whole app until it closes.
    fn toggle_theme_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme_editor.take().is_some() {
            crate::theme_editor::apply_settings(&self.config.settings, Some(window), cx);
            return cx.notify();
        }
        let editor = cx.new(|cx| ThemeEditor::new(&self.config.settings, window, cx));
        cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &ThemeEditorEvent, window, cx| {
                this.reload_themes();
                if let ThemeEditorEvent::Saved(t) = event {
                    this.choose_theme(t, cx);
                }
                this.theme_editor = None;
                crate::theme_editor::apply_settings(&this.config.settings, Some(window), cx);
                cx.notify();
            },
        )
        .detach();
        let restore = cx.on_release(|this, cx| {
            crate::theme_editor::apply_settings(&this.config.settings, None, cx)
        });
        self.theme_editor = Some((editor, restore));
        cx.notify();
    }

    fn toggle_themes_gallery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.themes_gallery.take().is_some() {
            return cx.notify();
        }
        let gallery = cx.new(|cx| ThemesGallery::new(window, cx));
        cx.subscribe_in(
            &gallery,
            window,
            |this, _, event: &ThemesGalleryEvent, window, cx| {
                this.reload_themes();
                match event {
                    ThemesGalleryEvent::Use(t) => this.choose_theme(t, cx),
                    ThemesGalleryEvent::Changed => {}
                    ThemesGalleryEvent::Close => this.themes_gallery = None,
                }
                // Theme files may have changed under the same settings.
                crate::theme_editor::apply_settings(&this.config.settings, Some(window), cx);
                cx.notify();
            },
        )
        .detach();
        self.themes_gallery = Some(gallery);
        cx.notify();
    }

    fn reload_themes(&mut self) {
        let (themes, problems) = wraithgrid_themes::all_themes(&themes_root(&home_dir()));
        self.themes = themes;
        self.theme_problems = problems;
    }

    /// `chooseTheme`: drawn now; also the theme for its mode when following the OS.
    fn choose_theme(&mut self, t: &AppTheme, cx: &mut Context<Self>) {
        let t = t.clone();
        self.update(cx, move |s| crate::theme_editor::choose_theme(s, &t));
    }

    fn import_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else {
                return;
            };
            let Some(src) = paths.into_iter().next() else {
                return;
            };
            let imported = cx
                .background_spawn(async move {
                    wraithgrid_themes::import_theme(&src, &themes_root(&home_dir()))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.reload_themes();
                match imported {
                    Ok(Ok(id)) => {
                        this.theme_error = None;
                        if let Some(t) = this.themes.iter().find(|t| t.id == id).cloned() {
                            this.choose_theme(&t, cx);
                        }
                        crate::theme_editor::apply_settings(
                            &this.config.settings,
                            Some(window),
                            cx,
                        );
                    }
                    Ok(Err(e)) => this.theme_error = Some(e),
                    Err(_) => this.theme_error = Some("Could not import the theme".into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Export…: a theme as a file anywhere, written as a theme file is.
    fn export_theme(&mut self, theme: ThemeFile, cx: &mut Context<Self>) {
        let text = wraithgrid_themes::theme_file_text(&theme);
        let name = format!("{}.json", wraithgrid_themes::theme_slug(&theme.name));
        let pick = cx.prompt_for_new_path(&home_dir(), Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = pick.await else {
                return;
            };
            let error = std::fs::write(&path, text)
                .err()
                .map(|_| "Could not export the theme".to_string());
            let _ = this.update(cx, |this, cx| {
                this.theme_error = error;
                cx.notify();
            });
        })
        .detach();
    }

    fn open_themes_folder(&mut self, cx: &mut Context<Self>) {
        let dir = themes_root(&home_dir());
        match std::fs::create_dir_all(&dir) {
            Ok(()) => {
                self.theme_error = None;
                cx.open_with_system(&dir);
            }
            Err(e) => self.theme_error = Some(format!("Could not open the themes folder: {e}")),
        }
        cx.notify();
    }

    fn delete_theme(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        match wraithgrid_themes::delete_theme(&id, &themes_root(&home_dir())) {
            Ok(_) => self.theme_error = None,
            Err(e) => self.theme_error = Some(e.to_string()),
        }
        self.reload_themes();
        crate::theme_editor::apply_settings(&self.config.settings, Some(window), cx);
        cx.notify();
    }

    // ---- Tabs --------------------------------------------------------------------------------

    fn general(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.config.settings.clone();
        let accounts: Vec<AnyElement> = self
            .config
            .accounts
            .clone()
            .into_iter()
            .map(|a| {
                let on = s.default_account_id.as_ref() == Some(&a.id);
                let color = crate::shell::parse_hex(&a.color).unwrap_or(rgb(0x7c5cff).into());
                let id = a.id.clone();
                chip(
                    SharedString::from(format!("defacc-{}", a.id)),
                    a.name.clone(),
                    on,
                    cx,
                )
                .flex()
                .items_center()
                .gap_2()
                .child(initial(&a.name, color, 16.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let id = id.clone();
                    this.update(cx, move |s| s.default_account_id = Some(id.clone()));
                }))
                .into_any_element()
            })
            .collect();
        vec![
            row("Default account", "Pre-selected in the New pane dialog.", cx)
                .child(if accounts.is_empty() {
                    hint("No accounts yet.", cx).into_any_element()
                } else {
                    chips(accounts).into_any_element()
                })
                .into_any_element(),
            row(
                "Default working directory",
                "Where the folder picker opens and Login panes start.",
                cx,
            )
            .child(self.field_row(Field::DefaultCwd, Some(true), cx))
            .into_any_element(),
            row(
                "When the window closes",
                "Agents can keep running in the background after the window closes, or after Wraithgrid quits. The next launch shows them again, screen and all.",
                cx,
            )
            .child(choice(
                "close",
                &[
                    (CloseMode::Ask, "Ask each time"),
                    (CloseMode::Keep, "Keep agents running"),
                    (CloseMode::Stop, "Stop agents and quit"),
                ],
                s.on_window_close,
                |s, v| s.on_window_close = v,
                cx,
            ))
            .child(hint(
                "Running agents keep using CPU, memory and API usage. With agents kept, the tray icon offers to open Wraithgrid or stop them all; no notifications arrive while the window is closed.",
                cx,
            ))
            .into_any_element(),
            row(
                "Notifications",
                "A desktop notification when a claude pane you aren't looking at finishes its work or asks for approval. Click it to jump to the pane.",
                cx,
            )
            .child(switch(
                "notify",
                "Notify when a pane finishes or needs approval",
                s.notify_panes,
                |s, v| s.notify_panes = v,
                cx,
            ))
            .into_any_element(),
        ]
    }

    fn chat(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.config.settings.clone();
        let effort: Vec<(Option<ChatEffort>, &str)> = std::iter::once((None, "Default"))
            .chain(ChatEffort::ALL.iter().map(|e| (Some(*e), effort_label(*e))))
            .collect();
        let models: Vec<(String, String)> = FALLBACK_MODELS
            .iter()
            .map(|(v, l)| (v.to_string(), l.to_string()))
            .collect();
        let mut model_chips: Vec<AnyElement> = vec![chip(
            "model-default".into(),
            "Default",
            s.chat_model.is_none(),
            cx,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.set_field(Field::ChatModel, String::new(), window, cx)
        }))
        .into_any_element()];
        for (value, label) in models {
            let on = s.chat_model.as_deref() == Some(value.as_str());
            model_chips.push(
                chip(SharedString::from(format!("model-{value}")), label, on, cx)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_field(Field::ChatModel, value.clone(), window, cx)
                    }))
                    .into_any_element(),
            );
        }
        let theme = cx.theme().clone();
        let modes = [
            ChatMode::Default,
            ChatMode::AcceptEdits,
            ChatMode::Plan,
            ChatMode::Auto,
            ChatMode::DontAsk,
        ];
        let mode_chips: Vec<AnyElement> = modes
            .iter()
            .map(|m| {
                let m = *m;
                let on = s.chat_mode.unwrap_or(ChatMode::Default) == m;
                chip(
                    SharedString::from(format!("mode-{}", m.as_str())),
                    mode_label(m),
                    on,
                    cx,
                )
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(mode_hint(m)).build(window, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update(cx, move |s| {
                        s.chat_mode = (m != ChatMode::Default).then_some(m)
                    })
                }))
                .into_any_element()
            })
            .collect();
        let off = s.auto_switch_mode == AutoSwitchMode::Off;
        let at_chips: Vec<AnyElement> = [80, 90, 95, 100]
            .iter()
            .map(|p| {
                let p = *p;
                chip(
                    SharedString::from(format!("switchat-{p}")),
                    format!("{p}%"),
                    s.auto_switch_at == p,
                    cx,
                )
                .when(off, |d| d.opacity(0.45).cursor_default())
                .when(!off, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        this.update(cx, move |s| s.auto_switch_at = p)
                    }))
                })
                .into_any_element()
            })
            .collect();
        vec![
            row(
                "Model for new chats",
                "Each chat can switch from the model button in its message box. Default follows the account's own setting.",
                cx,
            )
            .child(chips(model_chips))
            .child(self.field_row(Field::ChatModel, None, cx))
            .into_any_element(),
            row(
                "Effort for new chats",
                "How hard the model thinks before it answers. Higher is slower and uses more of your plan.",
                cx,
            )
            .child(choice("effort", &effort, s.chat_effort, |s, v| s.chat_effort = v, cx))
            .into_any_element(),
            row(
                "Mode for new chats",
                "How much a chat may do without asking. Each chat can switch from its mode button or with Shift+Tab.",
                cx,
            )
            .child(chips(mode_chips))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{} Full access is only offered per chat, never by default.",
                        mode_hint(s.chat_mode.unwrap_or(ChatMode::Default))
                    )),
            )
            .into_any_element(),
            row(
                "Switch account at limit",
                "When a Claude or Codex chat's account reaches its 5-hour or weekly limit, the chat can carry on as your next account of the same CLI that has room, with the whole conversation. A message the limit refused is sent again there.",
                cx,
            )
            .child(choice(
                "autoswitch",
                &[
                    (AutoSwitchMode::Ask, "Ask first"),
                    (AutoSwitchMode::Auto, "Automatically"),
                    (AutoSwitchMode::Off, "Off"),
                ],
                s.auto_switch_mode,
                |s, v| s.auto_switch_mode = v,
                cx,
            ))
            .child(hint(
                match s.auto_switch_mode {
                    AutoSwitchMode::Ask => "The chat stops and asks before it switches. Scheduled runs switch on their own.",
                    AutoSwitchMode::Auto => "The chat switches without asking and says so in the conversation.",
                    AutoSwitchMode::Off => "The chat stays on its account and shows the limit error.",
                },
                cx,
            ))
            .child(chips(at_chips))
            .into_any_element(),
        ]
    }

    fn appearance(&mut self, dark_os: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.config.settings.clone();
        let theme = cx.theme().clone();
        let ts = crate::theme::theme_settings(&s);
        let (picked, drawn) = wraithgrid_themes::drawn_theme(&self.themes, &ts, dark_os);
        let picked = picked.clone();
        let palettes: Vec<AnyElement> =
            TerminalPalette::ALL
                .iter()
                .map(|p| {
                    let p = *p;
                    let tp = wraithgrid_themes::TerminalPalette::parse(p.as_str())
                        .unwrap_or(wraithgrid_themes::TerminalPalette::Match);
                    let t = wraithgrid_themes::terminal_theme(tp, &drawn);
                    let on = s.terminal_palette == p;
                    let hex = |h: &str| crate::shell::parse_hex(h).unwrap_or(theme.foreground);
                    div()
                        .id(SharedString::from(format!("palette-{}", p.as_str())))
                        .role(Role::RadioButton)
                        .aria_label(wraithgrid_themes::palette_label(tp))
                        .aria_toggled(if on { Toggled::True } else { Toggled::False })
                        .w(px(132.))
                        .p_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .rounded_md()
                        .border_1()
                        .cursor_pointer()
                        .border_color(if on { theme.ring } else { theme.border })
                        .hover(|d| d.bg(theme.secondary))
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .bg(hex(&t.background))
                                .text_color(hex(&t.foreground))
                                .font_family(crate::terminal::mono(cx))
                                .text_xs()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .child(div().text_color(hex(&t.cursor)).child("> "))
                                        .child("claude"),
                                )
                                .child(div().flex().gap(px(3.)).children((1..=6).map(|i| {
                                    div().size(px(8.)).rounded_full().bg(hex(&t.ansi[i]))
                                }))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_center()
                                .child(wraithgrid_themes::palette_label(tp)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update(cx, move |s| s.terminal_palette = p)
                        }))
                        .into_any_element()
                })
                .collect();
        let term = wraithgrid_themes::terminal_theme(
            wraithgrid_themes::TerminalPalette::parse(s.terminal_palette.as_str())
                .unwrap_or(wraithgrid_themes::TerminalPalette::Match),
            &drawn,
        );
        let fonts: Vec<AnyElement> = TerminalFont::ALL
            .iter()
            .map(|f| {
                let f = *f;
                let on = s.font_family == f;
                chip(
                    SharedString::from(format!("font-{}", f.as_str())),
                    f.as_str(),
                    on,
                    cx,
                )
                .font_family(self.font_face(f.as_str(), cx))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.update(cx, move |s| s.font_family = f)),
                )
                .into_any_element()
            })
            .collect();
        let zoom = s.ui_zoom;
        let zoom_min = zoom <= UI_ZOOM_STEPS[0] + 1e-6;
        let zoom_max = zoom >= UI_ZOOM_STEPS[UI_ZOOM_STEPS.len() - 1] - 1e-6;
        let mac = cfg!(target_os = "macos");
        let mod_key = if mac { "⌘" } else { "Ctrl" };
        vec![
            self.theme_row(&picked, &drawn, dark_os, cx),
            row(
                "Interface size",
                "Everything at once: sidebar, chats and terminals.",
                cx,
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(stepper(
                        "zoom",
                        format!("{}%", (zoom * 100.0).round()),
                        (!zoom_min).then_some(|s: &mut Settings| s.ui_zoom = step_zoom(s.ui_zoom, -1)),
                        (!zoom_max).then_some(|s: &mut Settings| s.ui_zoom = step_zoom(s.ui_zoom, 1)),
                        cx,
                    ))
                    .when((zoom - 1.0).abs() > 1e-6, |d| {
                        d.child(
                            small_button("zoom-reset", "Reset", true, cx).on_click(cx.listener(
                                |this, _, _, cx| this.update(cx, |s| s.ui_zoom = 1.0),
                            )),
                        )
                    }),
            )
            .child(hint_owned(
                format!("Also {mod_key}+= / {mod_key}+-, and {mod_key}+0 to reset."),
                cx,
            ))
            .into_any_element(),
            row(
                "Accent color",
                "Buttons, selection, focus rings and the terminal cursor. Theme keeps the theme's own.",
                cx,
            )
            .child(self.accent_swatches(&picked, cx))
            .into_any_element(),
            row(
                "Terminal colors",
                "Match app uses the theme's own terminal colors. The others keep theirs in every theme.",
                cx,
            )
            .child(div().flex().flex_wrap().gap_2().children(palettes))
            .into_any_element(),
            row(
                "Terminal font",
                "Applies to every pane. Monospace fonts only; a font that is not installed falls back to your system monospace.",
                cx,
            )
            .child(chips(fonts))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_sm().text_color(theme.muted_foreground).child("Size"))
                    .child(stepper(
                        "fontsize",
                        format!("{} px", s.font_size),
                        (s.font_size > FONT_SIZE_MIN)
                            .then_some(|s: &mut Settings| s.font_size = (s.font_size - 1).max(FONT_SIZE_MIN)),
                        (s.font_size < FONT_SIZE_MAX)
                            .then_some(|s: &mut Settings| s.font_size = (s.font_size + 1).min(FONT_SIZE_MAX)),
                        cx,
                    )),
            )
            .child(font_preview(
                self.font_face(s.font_family.as_str(), cx),
                s.font_size as f32,
                &term,
                theme.foreground,
            ))
            .into_any_element(),
        ]
    }

    /// The family a font is drawn in: itself if installed, else the terminal's monospace.
    fn font_face(&self, family: &str, cx: &App) -> SharedString {
        if self.installed_fonts.iter().any(|f| f == family) {
            SharedString::from(family.to_string())
        } else {
            crate::terminal::mono(cx)
        }
    }

    fn accent_swatches(&self, picked: &AppTheme, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let current = self.config.settings.accent;
        let own = rgba_h(UiColors::from_vars(&wraithgrid_themes::theme_vars(&picked.theme)).accent);
        let swatch = |a: Accent, color: Hsla, cx: &mut Context<Self>| {
            let on = current == a;
            let label = if a == Accent::Theme {
                "Theme accent".to_string()
            } else {
                let n = a.as_str();
                format!("{}{}", n[..1].to_uppercase(), &n[1..])
            };
            div()
                .id(SharedString::from(format!("accent-{}", a.as_str())))
                .role(Role::RadioButton)
                .aria_label(label.clone())
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .size(px(26.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(color)
                .border_2()
                .border_color(if on {
                    theme.foreground
                } else {
                    transparent_black()
                })
                .text_xs()
                .text_color(white())
                .when(on, |d| d.child("✓"))
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| this.update(cx, move |s| s.accent = a)))
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(swatch(Accent::Theme, own, cx))
            .child(div().w(px(1.)).h(px(18.)).bg(theme.border))
            .children(
                Accent::ALL
                    .iter()
                    .filter(|a| **a != Accent::Theme)
                    .map(|a| swatch(*a, rgb(accent_swatch(*a)).into(), cx))
                    .collect::<Vec<_>>(),
            )
    }

    /// Theme: a card per theme drawn in its own colors, Import/Export/folder/reload, and the
    /// follow-the-OS switch (`ThemeRow`).
    fn theme_row(
        &mut self,
        picked: &AppTheme,
        drawn: &ThemeFile,
        dark_os: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let s = self.config.settings.clone();
        let theme = cx.theme().clone();
        let follow = s.theme == ThemePreference::System;
        let dark =
            wraithgrid_themes::pick_theme(&self.themes, &s.theme_dark, ThemeName::Dark).clone();
        let light =
            wraithgrid_themes::pick_theme(&self.themes, &s.theme_light, ThemeName::Light).clone();
        let drawn_id = picked.id.clone();
        let cards: Vec<AnyElement> = self
            .themes
            .clone()
            .into_iter()
            .map(|t| {
                let on = t.id == drawn_id;
                let slot = follow && !on && (t.id == dark.id || t.id == light.id);
                let sub = format!(
                    "{}{}{}",
                    if t.mode == ThemeName::Dark {
                        "Dark"
                    } else {
                        "Light"
                    },
                    if t.built_in { "" } else { " · yours" },
                    if slot {
                        if t.mode == ThemeName::Dark {
                            " · when the OS is dark"
                        } else {
                            " · when the OS is light"
                        }
                    } else {
                        ""
                    }
                );
                let title = match &t.author {
                    Some(a) => format!("{} by {a}", t.name),
                    None => t.name.clone(),
                };
                let (pick, menu_id) = (t.clone(), t.id.clone());
                div()
                    .id(SharedString::from(format!("theme-{}", t.id)))
                    .role(Role::RadioButton)
                    .aria_label(t.name.clone())
                    .aria_toggled(if on { Toggled::True } else { Toggled::False })
                    .w(px(150.))
                    .p_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .rounded_lg()
                    .border_1()
                    .cursor_pointer()
                    .border_color(if on { theme.ring } else { theme.border })
                    .when(on, |d| d.bg(theme.secondary))
                    .hover(|d| d.bg(theme.secondary))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(title.clone()).build(window, cx)
                    })
                    .child(theme_preview(&t.theme))
                    .child(
                        div()
                            .px_1()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_sm()
                            .child(div().flex_1().truncate().child(t.name.clone()))
                            .when(on, |d| d.child(div().text_color(theme.ring).child("✓"))),
                    )
                    .child(
                        div()
                            .px_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(sub),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_theme(&pick, cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            this.confirm_delete = false;
                            this.theme_menu = Some((ThemeMenu::Card(menu_id.clone()), e.position));
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .into_any_element()
            })
            .collect();
        let mode_now = if drawn.mode == ThemeName::Dark {
            ThemePreference::Dark
        } else {
            ThemePreference::Light
        };
        let note = if follow {
            format!(
                "Your desktop is {} now. Dark uses {}, light uses {}: pick a card of each kind to change them.",
                if dark_os { "dark" } else { "light" },
                dark.name,
                light.name
            )
        } else {
            format!("{} is on for the whole app.", picked.name)
        };
        let problems = self.theme_problems.clone();
        row(
            "Theme",
            "Five built in, or your own: a theme is a small JSON file of colors you can import and share.",
            cx,
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(
                    small_button("theme-browse", "Browse community…", true, cx)
                        .aria_expanded(self.themes_gallery.is_some())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_themes_gallery(window, cx)
                        })),
                )
                .child(
                    small_button("theme-customize", "Customize…", true, cx)
                        .aria_expanded(self.theme_editor.is_some())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_theme_editor(window, cx)
                        })),
                )
                .child(
                    small_button("theme-import", "Import theme…", true, cx).on_click(
                        cx.listener(|this, _, window, cx| this.import_theme(window, cx)),
                    ),
                )
                .child(
                    small_button("theme-more", "⋯", true, cx)
                        .aria_label("More theme actions")
                        .on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                            this.theme_menu = Some((ThemeMenu::Actions, e.position()));
                            cx.notify();
                        })),
                ),
        )
        .children(self.themes_gallery.clone())
        .children(self.theme_editor.as_ref().map(|(e, _)| e.clone()))
        .child(div().flex().flex_wrap().gap_2().children(cards))
        .child(
            Switch::new("theme-follow")
                .checked(follow)
                .label("Follow the OS light or dark setting")
                .on_click(cx.listener(move |this, on: &bool, _, cx| {
                    let next = if *on { ThemePreference::System } else { mode_now };
                    this.update(cx, move |s| s.theme = next);
                })),
        )
        .child(hint_owned(note, cx))
        .when_some(self.theme_error.clone(), |d, e| d.child(error_text(e, cx)))
        .when(!problems.is_empty(), |d| {
            d.child(
                div()
                    .id("theme-problems")
                    .role(Role::Status)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.warning)
                    .text_xs()
                    .child(if problems.len() == 1 {
                        "A theme file could not be loaded:".to_string()
                    } else {
                        format!("{} theme files could not be loaded:", problems.len())
                    })
                    .children(problems.into_iter().map(|p| {
                        div()
                            .flex()
                            .gap_1()
                            .child(div().font_family(crate::terminal::mono(cx)).child(p.file))
                            .child(format!("— {}", p.error))
                    })),
            )
        })
        .into_any_element()
    }

    /// The Theme row's menus, over a scrim that closes them.
    fn theme_menu_layer(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (menu, at) = self.theme_menu.clone()?;
        let theme = cx.theme().clone();
        let size = window.viewport_size();
        let item = |id: &'static str, label: String, danger: bool| {
            div()
                .id(id)
                .role(Role::MenuItem)
                .aria_label(label.clone())
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .when(danger, |d| d.text_color(theme.danger))
                .child(label)
        };
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.theme_menu = None;
            this.confirm_delete = false;
            cx.notify();
        };
        let ts = crate::theme::theme_settings(&self.config.settings);
        let dark_os = crate::theme::system_dark(window);
        let (picked, _) = wraithgrid_themes::drawn_theme(&self.themes, &ts, dark_os);
        let mut list = div()
            .id("theme-menu")
            .role(Role::Menu)
            .absolute()
            .left(at.x.min(size.width - px(250.)))
            .top(at.y.min(size.height - px(120.)))
            .w(px(240.))
            .p_1()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .text_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        match menu {
            ThemeMenu::Actions => {
                let file = picked.theme.clone();
                list =
                    list.aria_label("Theme actions")
                        .child(
                            item("tm-export", format!("Export {}…", picked.name), false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    close(this, cx);
                                    this.export_theme(file.clone(), cx);
                                }),
                            ),
                        )
                        .child(
                            item("tm-folder", "Open themes folder".into(), false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    close(this, cx);
                                    this.open_themes_folder(cx);
                                }),
                            ),
                        )
                        .child(item("tm-reload", "Reload themes".into(), false).on_click(
                            cx.listener(move |this, _, window, cx| {
                                close(this, cx);
                                this.reload_themes();
                                crate::theme_editor::apply_settings(
                                    &this.config.settings,
                                    Some(window),
                                    cx,
                                );
                            }),
                        ));
            }
            ThemeMenu::Card(id) => {
                let t = self.themes.iter().find(|t| t.id == id).cloned()?;
                let (use_t, file, del) = (t.clone(), t.theme.clone(), t.id.clone());
                list = list
                    .aria_label(format!("Theme {}", t.name))
                    .child(
                        item("tm-use", "Use this theme".into(), false).on_click(cx.listener(
                            move |this, _, _, cx| {
                                close(this, cx);
                                this.choose_theme(&use_t, cx);
                            },
                        )),
                    )
                    .child(
                        item(
                            "tm-card-export",
                            if t.built_in {
                                "Export as a starting point…".into()
                            } else {
                                "Export…".into()
                            },
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            close(this, cx);
                            this.export_theme(file.clone(), cx);
                        })),
                    )
                    .when(!t.built_in, |d| {
                        d.child(div().h(px(1.)).my_1().bg(theme.border)).child(
                            item(
                                "tm-delete",
                                if self.confirm_delete {
                                    "Click again to delete".into()
                                } else {
                                    "Delete theme".into()
                                },
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    if !this.confirm_delete {
                                        this.confirm_delete = true;
                                        return cx.notify();
                                    }
                                    close(this, cx);
                                    this.delete_theme(del.clone(), window, cx);
                                },
                            )),
                        )
                    });
            }
        }
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("theme-scrim")
                        .occlude()
                        .w(size.width)
                        .h(size.height)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| close(this, cx)),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, _, _, cx| close(this, cx)),
                        )
                        .child(list),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn agents(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let home = home_dir().to_string_lossy().into_owned();
        let status: AnyElement = match &self.claude {
            None => hint("Checking…", cx).into_any_element(),
            Some(c) if c.error.is_none() => {
                let path = c
                    .path
                    .as_ref()
                    .map(|p| contract_home(&p.to_string_lossy(), &home))
                    .unwrap_or_default();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(div().text_color(theme.success).child("✓"))
                    .child(if c.overridden {
                        "Override:"
                    } else {
                        "Auto-detected:"
                    })
                    .child(div().font_family(crate::terminal::mono(cx)).child(path))
                    .when_some(c.version.clone(), |d, v| {
                        d.child(div().text_xs().text_color(theme.muted_foreground).child(v))
                    })
                    .into_any_element()
            }
            Some(c) => {
                let error = c.error.clone().unwrap_or_default();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(div().text_color(theme.warning).child("⚠"))
                    .child(match &c.path {
                        Some(p) => div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .font_family(crate::terminal::mono(cx))
                                    .child(contract_home(&p.to_string_lossy(), &home)),
                            )
                            .child(error),
                        None => div().child(format!("{error}. Set its path below.")),
                    })
                    .into_any_element()
            }
        };
        let has_override = !self.input(Field::ClaudePath).read(cx).value().is_empty();
        let mut rows = vec![row(
            "claude binary",
            "Found on your PATH. Override it if you keep several versions.",
            cx,
        )
        .child(div().id("claude-status").role(Role::Status).child(status))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(self.field_row(Field::ClaudePath, Some(false), cx).flex_1())
                .child(
                    small_button("claude-reset", "Reset", has_override, cx).when(
                        has_override,
                        |d| {
                            d.on_click(cx.listener(|this, _, window, cx| {
                                this.set_field(Field::ClaudePath, String::new(), window, cx)
                            }))
                        },
                    ),
                ),
        )
        .into_any_element()];
        let mut paths = row(
            "Other agent CLIs",
            "Found on your PATH. Set a path to use a binary that is not on it.",
            cx,
        );
        for cli in AgentCli::ALL.iter().filter(|c| **c != AgentCli::Claude) {
            paths = paths.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(130.))
                            .text_sm()
                            .font_family(crate::terminal::mono(cx))
                            .child(cli.spec().bin),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(self.field_row(Field::CliPath(*cli), None, cx)),
                    ),
            );
        }
        rows.push(paths.into_any_element());
        rows.push(self.shared_row(cx));
        rows.push(self.shell_row(cx));
        rows
    }

    fn shared_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mode = self.config.settings.shared_mode;
        let busy = self.shared_busy;
        let home = home_dir().to_string_lossy().into_owned();
        let shared_chip = |value: SharedMode, label: &'static str, cx: &mut Context<Self>| {
            chip(
                SharedString::from(format!("shared-{}", value.as_str())),
                label,
                mode == value,
                cx,
            )
            .when(busy, |d| d.opacity(0.5).cursor_default())
            .when(!busy, |d| {
                d.on_click(cx.listener(move |this, _, _, cx| this.choose_shared(value, cx)))
            })
            .into_any_element()
        };
        // One line per account that changed, e.g. "work: linked CLAUDE.md, skills".
        let mut lines: Vec<(String, Vec<String>)> = Vec::new();
        for r in self.shared_report.iter().flatten() {
            let res = &r.result;
            let what = match res.outcome {
                LinkOutcome::AlreadyLinked => continue,
                LinkOutcome::Failed => format!(
                    "could not link {} ({})",
                    res.item,
                    res.error.as_deref().unwrap_or("")
                ),
                LinkOutcome::Linked => match &res.backup {
                    Some(b) => format!(
                        "linked {}, kept the old one as {}",
                        res.item,
                        contract_home(&b.to_string_lossy(), &home)
                    ),
                    None => format!("linked {}", res.item),
                },
                LinkOutcome::Restored => format!("restored its own {}", res.item),
                LinkOutcome::Unlinked => format!("unlinked {}", res.item),
            };
            match lines.iter_mut().find(|(a, _)| *a == r.account) {
                Some((_, whats)) => whats.push(what),
                None => lines.push((r.account.clone(), vec![what])),
            }
        }
        let a = shared_chip(SharedMode::Overall, "Overall (one per provider)", cx);
        let b = shared_chip(SharedMode::PerAccount, "Each account its own", cx);
        row(
            "Shared instructions and skills",
            "Overall gives each provider one brain: all claude accounts share one CLAUDE.md and skills, all codex accounts one AGENTS.md and skills, and so on. Logins and history always stay separate.",
            cx,
        )
        .child(chips(vec![a, b]))
        .when(busy, |d| d.child(hint("Updating the accounts…", cx)))
        .child(hint(
            "Kept in ~/.wraithgrid/shared/<provider>, first copied from that provider's own home (~/.claude, ~/.codex…), which is left as it is. claude: CLAUDE.md, settings, skills, plugins, agents, commands. codex: AGENTS.md, skills, prompts. Files an account already had are kept as backups.",
            cx,
        ))
        .when_some(self.shared_error.clone(), |d, e| d.child(error_text(e, cx)))
        .when(!lines.is_empty(), |d| {
            d.child(
                div()
                    .id("shared-report")
                    .role(Role::Status)
                    .p_2()
                    .rounded_md()
                    .bg(theme.secondary)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_xs()
                    .children(lines.into_iter().map(|(account, whats)| {
                        div()
                            .flex()
                            .gap_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{account}:")))
                            .child(whats.join("; "))
                    })),
            )
        })
        .when(self.shared_report.is_some(), |d| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(hint("Running claude panes read these files at start.", cx))
                    .child(
                        small_button("shared-restart", "Restart claude panes", true, cx).on_click(
                            cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::RestartClaudePanes)),
                        ),
                    )
                    .when_some(self.restarted, |d, n| {
                        d.child(hint_owned(format!("Restarted {n}."), cx))
                    }),
            )
        })
        .into_any_element()
    }

    fn shell_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let home = home_dir().to_string_lossy().into_owned();
        let s = self.config.settings.clone();
        let found = self.shells.clone().unwrap_or_default();
        let known = found.iter().any(|o| o.path == s.shell_path);
        #[derive(PartialEq)]
        enum Mode {
            Auto,
            Known(String),
            Custom,
        }
        let mode = if s.shell_path.is_empty() && !self.shell_custom {
            Mode::Auto
        } else if known && !self.shell_custom {
            Mode::Known(s.shell_path.clone())
        } else {
            Mode::Custom
        };
        let option = |id: String, name: String, sub: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .role(Role::RadioButton)
                .aria_label(name.clone())
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .w(px(220.))
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .cursor_pointer()
                .border_color(if on { theme.ring } else { theme.border })
                .when(on, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .child(div().text_sm().child(name))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(sub),
                        ),
                )
                .when(on, |d| d.child(div().text_color(theme.ring).child("✓")))
        };
        let mut options: Vec<AnyElement> = vec![option(
            "shell-auto".into(),
            "Automatic".into(),
            if cfg!(windows) {
                "PowerShell 7, Windows PowerShell, then cmd".into()
            } else {
                "Your login shell ($SHELL)".into()
            },
            mode == Mode::Auto,
        )
        .on_click(
            cx.listener(|this, _, window, cx| this.pick_shell(String::new(), vec![], window, cx)),
        )
        .into_any_element()];
        for (i, o) in found.into_iter().enumerate() {
            let sub = [contract_home(&o.path, &home), format_args(&o.args)]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let on = mode == Mode::Known(o.path.clone());
            options.push(
                option(format!("shell-{i}"), o.name.clone(), sub, on)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_shell(o.path.clone(), o.args.clone(), window, cx)
                    }))
                    .into_any_element(),
            );
        }
        options.push(
            option(
                "shell-custom".into(),
                "Custom…".into(),
                "Any shell: a path or a name on PATH".into(),
                mode == Mode::Custom,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.shell_custom = true;
                cx.notify();
            }))
            .into_any_element(),
        );
        row(
            "Shell for plain panes",
            "What Open a plain shell runs. claude panes don't use it: claude starts directly and gets a clean terminal environment whichever shell you use.",
            cx,
        )
        .child(div().flex().flex_wrap().gap_2().children(options))
        .when(self.shells.is_none(), |d| d.child(hint("Looking for installed shells…", cx)))
        .when(mode == Mode::Custom, |d| {
            d.child(self.field_row(Field::ShellPath, Some(false), cx))
                .child(self.field_row(Field::ShellArgs, None, cx))
        })
        .child(hint("Applies to shell panes started from now on.", cx))
        .into_any_element()
    }

    fn advanced(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.config.settings.clone();
        vec![
            row(
                "Command line",
                "Lets the wraithgrid command, and agents in your panes, list panes, read their screens, type into them, wait for them and open new ones. Only your user can connect; nothing listens on the network.",
                cx,
            )
            .child(switch(
                "cli",
                "Allow wraithgrid commands",
                s.cli_access,
                |s, v| s.cli_access = v,
                cx,
            ))
            .child(hint(
                "wraithgrid list, read <pane>, send <pane> <text>, wait <pane> --until idle, new, split <pane>, stop. Run wraithgrid help for details.",
                cx,
            ))
            .into_any_element(),
            row(
                "Machines",
                "Run a workspace's panes on another computer over ssh: right-click the workspace, then Run on. Its agents keep running there when you disconnect or quit. The other computer needs Wraithgrid installed (wraithgrid on PATH) and ssh with a key or agent.",
                cx,
            )
            .child(self.machines.clone())
            .into_any_element(),
        ]
    }

    /// The machines after the window applied a change.
    pub fn set_machines(
        &mut self,
        machines: Vec<wraithgrid_core::config::Machine>,
        cx: &mut Context<Self>,
    ) {
        self.config.machines = machines.clone();
        self.machines
            .update(cx, |m, cx| m.set_machines(machines, cx));
    }

    /// One rebindable shortcut per line: its keys, Change (records the next chord) and Reset.
    fn keyboard(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mac = cfg!(target_os = "macos");
        let theme = cx.theme().clone();
        let shortcuts = self.config.settings.shortcuts.clone();
        let items: Vec<AnyElement> = bindings(&shortcuts)
            .into_iter()
            .map(|(id, b)| {
                let recording = self.recording == Some(id);
                let is_default = !shortcuts.iter().any(|(s, _)| *s == id);
                let error = self
                    .shortcut_error
                    .as_ref()
                    .filter(|(e, _)| *e == id)
                    .map(|(_, m)| m.clone());
                let keys = if recording {
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Press the new keys… (Esc cancels)")
                } else {
                    crate::shortcuts_sheet::key_caps(&binding_keys(&b, mac), cx)
                };
                div()
                    .flex()
                    .flex_col()
                    .py_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().text_sm().child(shortcut_label(id)))
                            .child(keys)
                            .child(
                                small_button(
                                    format!("sc-change-{}", id.as_str()),
                                    if recording { "Cancel" } else { "Change" },
                                    true,
                                    cx,
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.shortcut_error = None;
                                        this.recording = if recording { None } else { Some(id) };
                                        window.focus(&this.focus, cx);
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                small_button(
                                    format!("sc-reset-{}", id.as_str()),
                                    "Reset",
                                    !is_default,
                                    cx,
                                )
                                .when(!is_default, |d| {
                                    d.on_click(cx.listener(move |this, _, _, cx| {
                                        this.shortcut_error = None;
                                        this.store_shortcut(id, None, cx);
                                    }))
                                }),
                            ),
                    )
                    .when_some(error, |d, e| {
                        d.child(div().pt_1().text_xs().text_color(theme.danger).child(e))
                    })
                    .into_any_element()
            })
            .collect();
        vec![row(
            "Keyboard shortcuts",
            if mac {
                "Pick another chord for an app shortcut. It needs Shift or Alt with ⌘, so the terminal keeps its own keys."
            } else {
                "Pick another chord for an app shortcut. It needs Shift or Alt with Ctrl, so the terminal keeps its own keys."
            },
            cx,
        )
        .children(items)
        .into_any_element()]
    }

    /// While recording, the next chord is the binding; it never reaches the app or a pane.
    fn record(&mut self, id: ShortcutId, e: &KeyDownEvent, cx: &mut Context<Self>) {
        if e.keystroke.key == "escape" {
            self.recording = None;
            return cx.notify();
        }
        let m = e.keystroke.modifiers;
        let press = Press {
            key: &e.keystroke.key,
            ctrl: m.control,
            shift: m.shift,
            alt: m.alt,
            meta: m.platform,
        };
        let Some(b) = binding_from_press(press, cfg!(target_os = "macos")) else {
            return;
        };
        self.recording = None;
        let problem = binding_problem(&b, id, &bindings(&self.config.settings.shortcuts));
        self.shortcut_error = problem.clone().map(|p| (id, p));
        if problem.is_none() {
            let value = (b != default_binding(id)).then(|| format_binding(&b));
            self.store_shortcut(id, value, cx);
        }
        cx.notify();
    }

    /// Saves a binding in `settings.shortcuts`, or drops it (None) to use the default.
    fn store_shortcut(&mut self, id: ShortcutId, value: Option<String>, cx: &mut Context<Self>) {
        self.update(cx, move |s| {
            s.shortcuts.retain(|(i, _)| *i != id);
            if let Some(v) = value.clone() {
                s.shortcuts.push((id, v));
            }
        });
    }

    /// A text field with its error under it; `browse`: a Browse… button (Some(true) picks a
    /// folder, Some(false) a file).
    fn field_row(&self, field: Field, browse: Option<bool>, cx: &mut Context<Self>) -> Div {
        let error = self.error(field);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.input(field))))
                    .when_some(browse, |d, folder| {
                        d.child(
                            small_button(
                                SharedString::from(format!("browse-{}", field_key(field))),
                                "Browse…",
                                true,
                                cx,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| this.browse(field, folder, window, cx),
                            )),
                        )
                    }),
            )
            .when_some(error, |d, e| d.child(error_text(e, cx)))
    }

    fn nav_key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let at = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        let n = TABS.len();
        let next = match e.keystroke.key.as_str() {
            "down" | "right" => (at + 1) % n,
            "up" | "left" => (at + n - 1) % n,
            "home" => 0,
            "end" => n - 1,
            _ => return,
        };
        self.tab = TABS[next].0;
        cx.stop_propagation();
        cx.notify();
    }
}

/// What a typed value saves, or why it can't be saved.
fn field_patch(field: Field, value: &str) -> Result<SettingsPatch, String> {
    let v = value.trim().to_string();
    if v.chars().count() > 4096 {
        return Err("Too long: 4096 characters at most.".into());
    }
    Ok(match field {
        Field::DefaultCwd if v.is_empty() => {
            return Err("Type a folder, or pick one with Browse….".into())
        }
        Field::DefaultCwd => Arc::new(move |c| c.settings.default_cwd = v.clone()),
        Field::ChatModel if v.is_empty() => Arc::new(|c| c.settings.chat_model = None),
        Field::ChatModel if wraithgrid_core::config::is_chat_model(&v) => {
            Arc::new(move |c| c.settings.chat_model = Some(v.clone()))
        }
        Field::ChatModel => {
            return Err(
                "A model name is letters, digits and . _ - [ ] only, up to 100 characters.".into(),
            )
        }
        Field::ClaudePath => Arc::new(move |c| c.claude_path = v.clone()),
        Field::ShellPath => Arc::new(move |c| c.settings.shell_path = v.clone()),
        Field::ShellArgs => {
            let args = parse_args(value);
            Arc::new(move |c| c.settings.shell_args = args.clone())
        }
        Field::CliPath(cli) => Arc::new(move |c| {
            c.settings.cli_paths.retain(|(x, _)| *x != cli);
            if !v.is_empty() {
                c.settings.cli_paths.push((cli, v.clone()));
            }
        }),
    })
}

fn field_key(field: Field) -> String {
    match field {
        Field::DefaultCwd => "cwd".into(),
        Field::ChatModel => "model".into(),
        Field::ClaudePath => "claude".into(),
        Field::ShellPath => "shell".into(),
        Field::ShellArgs => "shellargs".into(),
        Field::CliPath(c) => format!("cli-{}", c.as_str()),
    }
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let dark_os = crate::theme::system_dark(window);
        let rows = match self.tab {
            Tab::General => self.general(cx),
            Tab::Chat => self.chat(cx),
            Tab::Appearance => self.appearance(dark_os, cx),
            Tab::Agents => self.agents(cx),
            Tab::Advanced => self.advanced(cx),
            Tab::Keyboard => self.keyboard(cx),
            Tab::About => self
                .about
                .iter()
                .map(|a| a.clone().into_any_element())
                .collect(),
        };
        let menu = self.theme_menu_layer(window, cx);
        let tabs = TABS.iter().map(|(tab, label)| {
            let tab = *tab;
            div()
                .id(SharedString::from(format!("tab-{label}")))
                .role(Role::Tab)
                .aria_label(*label)
                .aria_selected(self.tab == tab)
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(self.tab == tab, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
                .child(*label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.tab = tab;
                    window.focus(&this.nav_focus, cx);
                    cx.notify();
                }))
        });
        let title = TABS
            .iter()
            .find(|(t, _)| *t == self.tab)
            .map(|(_, l)| *l)
            .unwrap_or("");
        let config_path = config_file_path(&current_user_data_dir())
            .to_string_lossy()
            .into_owned();
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .bg(theme.background)
            .capture_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if let Some(id) = this.recording {
                    cx.stop_propagation();
                    this.record(id, e, cx);
                }
            }))
            .child(
                div()
                    .id("settings-nav")
                    .track_focus(&self.nav_focus)
                    .role(Role::TabList)
                    .aria_label("Settings sections")
                    .w(px(200.))
                    .h_full()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .border_r_1()
                    .border_color(theme.border)
                    .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| this.nav_key(e, cx)))
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .children(tabs)
                    .child(div().flex_1())
                    .child(
                        div()
                            .px_3()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Saved to")
                            .child(
                                div()
                                    .font_family(crate::terminal::mono(cx))
                                    .truncate()
                                    .child(config_path),
                            ),
                    )
                    .child(
                        div()
                            .id("settings-close")
                            .a11y_button("Back to panes")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.secondary))
                            .child("← Back to panes")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.flush(cx);
                                cx.emit(SettingsEvent::Close)
                            })),
                    ),
            )
            .child(
                div()
                    .id("settings-body")
                    .role(Role::TabPanel)
                    .aria_label(title)
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .overflow_y_scroll()
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .max_w(px(if self.tab == Tab::About { 1080. } else { 820. }))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .children(rows),
            )
            .children(menu)
    }
}

// ---- Row building blocks -------------------------------------------------------------------

fn rgba_h(c: wraithgrid_themes::Rgba) -> Hsla {
    Rgba {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
    .into()
}

/// A small window drawn in a theme's own colors: sidebar, a chat bubble, the accent.
fn theme_preview(t: &ThemeFile) -> Div {
    let c = UiColors::from_vars(&wraithgrid_themes::theme_vars(t));
    let line = |color: Hsla, w: f32| div().h(px(4.)).w(px(w)).rounded_full().bg(color);
    div()
        .h(px(70.))
        .w_full()
        .rounded_md()
        .overflow_hidden()
        .flex()
        .border_1()
        .border_color(rgba_h(c.line))
        .child(
            div()
                .w(px(38.))
                .h_full()
                .bg(rgba_h(c.sidebar))
                .p(px(6.))
                .flex()
                .flex_col()
                .gap(px(5.))
                .child(line(rgba_h(c.accent), 24.))
                .child(line(rgba_h(c.faint), 22.))
                .child(line(rgba_h(c.faint), 14.)),
        )
        .child(
            div()
                .flex_1()
                .h_full()
                .bg(rgba_h(c.bg))
                .p(px(7.))
                .flex()
                .flex_col()
                .gap(px(5.))
                .child(
                    div().flex().justify_end().child(
                        div()
                            .w(px(44.))
                            .h(px(11.))
                            .rounded_md()
                            .bg(rgba_h(c.user_bubble)),
                    ),
                )
                .child(line(rgba_h(c.muted), 60.))
                .child(line(rgba_h(c.muted), 36.))
                .child(
                    div()
                        .w(px(24.))
                        .h(px(9.))
                        .rounded_sm()
                        .bg(rgba_h(c.accent_fill)),
                ),
        )
}

/// A few lines of a claude session in the terminal font and colors (`.prev`).
fn font_preview(
    family: SharedString,
    size: f32,
    t: &wraithgrid_themes::TerminalTheme,
    fallback: Hsla,
) -> Stateful<Div> {
    let hex = |h: &str| crate::shell::parse_hex(h).unwrap_or(fallback);
    let (fg, dim, acc, ok) = (
        hex(&t.foreground),
        hex(&t.ansi[8]),
        hex(&t.cursor),
        hex(&t.ansi[2]),
    );
    let span = |text: &'static str, color: Hsla| div().text_color(color).child(text);
    let line = || div().flex().whitespace_nowrap();
    div()
        .id("font-preview")
        .role(Role::Group)
        .aria_label("Font preview")
        .p_3()
        .rounded_md()
        .bg(hex(&t.background))
        .font_family(family)
        .text_size(px(size))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(
            line()
                .child(span("> ", acc))
                .child(span("fix the failing auth test", fg)),
        )
        .child(
            line()
                .child(span("● ", ok))
                .child(span("Bash", fg))
                .child(span("(pnpm test auth)", dim)),
        )
        .child(
            line()
                .child(span("  ⎿  ", dim))
                .child(span("PASS", ok))
                .child(span(" src/auth/auth.controller.spec.ts", dim)),
        )
        .child(line().child(span(
            "  13 + import { Throttle } from '@nestjs/throttler';",
            ok,
        )))
        .child(line().child(span("  0O 1lI {}[] => !== ─│╭╮ ✻ ⎿", dim)))
}

/// The letter avatar used for accounts (`Initial`).
pub(crate) fn initial(name: &str, color: Hsla, size: f32) -> Div {
    let letter = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.6))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(white())
        .child(letter)
}

fn row(title: &'static str, explain: &'static str, cx: &App) -> Stateful<Div> {
    div()
        .id(title)
        .role(Role::Group)
        .aria_label(title)
        .aria_description(explain)
        .flex()
        .flex_col()
        .gap_2()
        .pb_4()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(div().font_weight(FontWeight::MEDIUM).child(title))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(explain),
        )
}

fn hint(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn hint_owned(text: String, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn error_text(text: String, cx: &App) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("err-{}", text.len())))
        .role(Role::Alert)
        .aria_label(text.clone())
        .text_xs()
        .text_color(cx.theme().danger)
        .child(text)
}

fn chips(children: Vec<AnyElement>) -> Div {
    div().flex().flex_wrap().gap_2().children(children)
}

fn chip(id: SharedString, label: impl Into<SharedString>, on: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let label = label.into();
    div()
        .id(id)
        .a11y_toggle(label.clone(), on)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .text_sm()
        .cursor_pointer()
        .border_color(if on { theme.ring } else { theme.border })
        .when(on, |d| d.bg(theme.secondary))
        .hover(|d| d.bg(theme.secondary))
        .child(label)
}

/// A plain bordered button; disabled ones are dimmed and take no clicks (callers attach
/// `on_click` only when enabled).
fn small_button(
    id: impl Into<SharedString>,
    label: &'static str,
    enabled: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().clone();
    div()
        .id(id.into())
        .a11y_button(label)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .text_sm()
        .flex_shrink_0()
        .when(enabled, |d| {
            d.cursor_pointer().hover(|d| d.bg(theme.secondary))
        })
        .when(!enabled, |d| d.opacity(0.45))
        .child(label)
}

/// One of several values: a row of chips, the current one marked.
fn choice<T: Copy + PartialEq + Send + Sync + 'static>(
    key: &'static str,
    options: &[(T, &str)],
    current: T,
    set: fn(&mut Settings, T),
    cx: &mut Context<SettingsView>,
) -> Div {
    let items = options
        .iter()
        .enumerate()
        .map(|(i, (value, label))| {
            let value = *value;
            chip(
                SharedString::from(format!("{key}-{i}")),
                label.to_string(),
                value == current,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.update(cx, move |s| set(s, value))))
            .into_any_element()
        })
        .collect();
    chips(items)
}

/// An on/off setting: a switch with its label.
fn switch(
    key: &'static str,
    label: &'static str,
    on: bool,
    set: fn(&mut Settings, bool),
    cx: &mut Context<SettingsView>,
) -> Switch {
    Switch::new(key)
        .checked(on)
        .label(label)
        .on_click(cx.listener(move |this, v: &bool, _, cx| {
            let v = *v;
            this.update(cx, move |s| set(s, v))
        }))
}

/// − value +; a side is None at its limit (drawn disabled).
fn stepper(
    key: &'static str,
    value: String,
    down: Option<fn(&mut Settings)>,
    up: Option<fn(&mut Settings)>,
    cx: &mut Context<SettingsView>,
) -> Div {
    let side = |id: String,
                label: &'static str,
                step: Option<fn(&mut Settings)>,
                cx: &mut Context<SettingsView>| {
        small_button(id, label, step.is_some(), cx).when_some(step, |d, step| {
            d.on_click(cx.listener(move |this, _, _, cx| this.update(cx, step)))
        })
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(side(format!("{key}-down"), "−", down, cx))
        .child(div().w(px(70.)).text_center().child(value))
        .child(side(format!("{key}-up"), "+", up, cx))
}

fn effort_label(e: ChatEffort) -> &'static str {
    match e {
        ChatEffort::Low => "Low",
        ChatEffort::Medium => "Medium",
        ChatEffort::High => "High",
        ChatEffort::Xhigh => "Extra high",
        ChatEffort::Max => "Max",
    }
}

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

fn mode_hint(m: ChatMode) -> &'static str {
    match m {
        ChatMode::Default => "Asks before it edits files or runs commands.",
        ChatMode::AcceptEdits => "Edits files on its own; still asks before commands.",
        ChatMode::Plan => "Reads and plans only; changes nothing.",
        ChatMode::Auto => "Decides on its own what is safe to run.",
        ChatMode::DontAsk => "Refuses anything that is not already allowed.",
        ChatMode::BypassPermissions => "Runs everything without asking.",
    }
}
