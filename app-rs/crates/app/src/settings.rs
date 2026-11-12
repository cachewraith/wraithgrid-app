//! Settings (`SettingsView.tsx`): the same tabs, rows and wording, editing the same fields of
//! config.json. The view keeps a working copy and sends every change to the window, which saves
//! it like any other config change.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::args::{format_args, parse_args};
use wraithgrid_core::config::{Config, Settings};
use wraithgrid_core::shortcuts::{bindings, label as shortcut_label, Binding};
use wraithgrid_core::types::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    General,
    Chat,
    Appearance,
    Agents,
    Advanced,
    Keyboard,
}

const TABS: &[(Tab, &str)] = &[
    (Tab::General, "General"),
    (Tab::Chat, "Chat"),
    (Tab::Appearance, "Appearance"),
    (Tab::Agents, "Agents & CLIs"),
    (Tab::Advanced, "Advanced"),
    (Tab::Keyboard, "Keyboard"),
];

pub enum SettingsEvent {
    /// The new settings, and the claude binary override (Agents & CLIs).
    Changed(Box<Settings>, String),
    Close,
}

/// Text fields, each bound to one setting.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    DefaultCwd,
    ChatModel,
    ThemeDark,
    ThemeLight,
    ClaudePath,
    ShellPath,
    ShellArgs,
    CliPath(AgentCli),
}

pub struct SettingsView {
    focus: FocusHandle,
    tab: Tab,
    settings: Settings,
    claude_path: String,
    accounts: Vec<(String, String)>,
    inputs: Vec<(Field, Entity<InputState>)>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(config: &Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let s = config.settings.clone();
        let mut fields: Vec<(Field, String, &str)> = vec![
            (Field::DefaultCwd, s.default_cwd.clone(), "~"),
            (
                Field::ChatModel,
                s.chat_model.clone().unwrap_or_default(),
                "The CLI's default",
            ),
            (Field::ThemeDark, s.theme_dark.clone(), "wraith"),
            (Field::ThemeLight, s.theme_light.clone(), "daylight"),
            (
                Field::ClaudePath,
                config.claude_path.clone(),
                "Found on PATH",
            ),
            (Field::ShellPath, s.shell_path.clone(), "Automatic"),
            (
                Field::ShellArgs,
                format_args(&s.shell_args),
                "e.g. --login -i",
            ),
        ];
        for cli in AgentCli::ALL.iter().filter(|c| **c != AgentCli::Claude) {
            let current = s
                .cli_paths
                .iter()
                .find(|(c, _)| c == cli)
                .map(|(_, p)| p.clone())
                .unwrap_or_default();
            fields.push((Field::CliPath(*cli), current, "Found on PATH"));
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
                move |this, input, event: &InputEvent, _, cx| {
                    if let InputEvent::Change = event {
                        let value = input.read(cx).value().to_string();
                        this.set_field(field, value, cx);
                    }
                },
            ));
            inputs.push((field, input));
        }
        SettingsView {
            focus: cx.focus_handle(),
            tab: Tab::General,
            settings: s,
            claude_path: config.claude_path.clone(),
            accounts: config
                .accounts
                .iter()
                .map(|a| (a.id.clone(), a.name.clone()))
                .collect(),
            inputs,
            _subscriptions: subs,
        }
    }

    fn input(&self, field: Field) -> Entity<InputState> {
        self.inputs
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, i)| i.clone())
            .expect("input for field")
    }

    /// Text fields are saved only when valid for the config schema; a bad value waits.
    fn set_field(&mut self, field: Field, value: String, cx: &mut Context<Self>) {
        let v = value.trim().to_string();
        let s = &mut self.settings;
        match field {
            Field::DefaultCwd if !v.is_empty() => s.default_cwd = v,
            Field::ChatModel if v.is_empty() => s.chat_model = None,
            Field::ChatModel if wraithgrid_core::config::is_chat_model(&v) => {
                s.chat_model = Some(v)
            }
            Field::ThemeDark if wraithgrid_core::config::is_id(&v) => s.theme_dark = v,
            Field::ThemeLight if wraithgrid_core::config::is_id(&v) => s.theme_light = v,
            Field::ClaudePath => self.claude_path = v,
            Field::ShellPath => s.shell_path = v,
            Field::ShellArgs => s.shell_args = parse_args(&value),
            Field::CliPath(cli) => {
                s.cli_paths.retain(|(c, _)| *c != cli);
                if !v.is_empty() {
                    s.cli_paths.push((cli, v));
                }
            }
            _ => return,
        }
        self.changed(cx);
    }

    fn update(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings);
        self.changed(cx);
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(SettingsEvent::Changed(
            Box::new(self.settings.clone()),
            self.claude_path.clone(),
        ));
        cx.notify();
    }

    fn general(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.settings.clone();
        let accounts: Vec<AnyElement> = self
            .accounts
            .clone()
            .into_iter()
            .map(|(id, name)| {
                let on = s.default_account_id.as_ref() == Some(&id);
                chip(SharedString::from(format!("defacc-{id}")), name, on, cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id.clone();
                        this.update(cx, |s| s.default_account_id = Some(id));
                    }))
                    .into_any_element()
            })
            .collect();
        vec![
            row("Default account", "Pre-selected in the New pane dialog.", cx)
                .child(if accounts.is_empty() { hint("No accounts yet.", cx).into_any_element() } else { chips(accounts).into_any_element() })
                .into_any_element(),
            row("Default working directory", "Where the folder picker opens and Login panes start.", cx)
                .child(Input::new(&self.input(Field::DefaultCwd)))
                .into_any_element(),
            row(
                "When the window closes",
                "Agents can keep running in the background after the window closes, or after Wraithgrid quits. The next launch shows them again, screen and all.",
                cx,
            )
            .child(choice(
                "close",
                &[(CloseMode::Ask, "Ask each time"), (CloseMode::Keep, "Keep agents running"), (CloseMode::Stop, "Stop agents and quit")],
                s.on_window_close,
                |s, v| s.on_window_close = v,
                cx,
            ))
            .child(hint("Running agents keep using CPU, memory and API usage.", cx))
            .into_any_element(),
            row("Notifications", "Tell me when a pane I'm not looking at finishes or needs approval.", cx)
                .child(toggle("notify", s.notify_panes, |s, v| s.notify_panes = v, cx))
                .into_any_element(),
        ]
    }

    fn chat(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.settings.clone();
        let effort: Vec<(Option<ChatEffort>, &str)> = std::iter::once((None, "Default"))
            .chain(ChatEffort::ALL.iter().map(|e| (Some(*e), effort_label(*e))))
            .collect();
        let modes: Vec<(Option<ChatMode>, &str)> = std::iter::once((None, "Default"))
            .chain(
                ChatMode::ALL
                    .iter()
                    .filter(|m| **m != ChatMode::Default)
                    .map(|m| (Some(*m), mode_label(*m))),
            )
            .collect();
        let models = ["opus", "sonnet", "haiku"];
        let model_chips: Vec<AnyElement> = models
            .iter()
            .map(|m| {
                let on = s.chat_model.as_deref() == Some(*m);
                let model = m.to_string();
                chip(
                    SharedString::from(format!("model-{m}")),
                    m.to_string(),
                    on,
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    let model = model.clone();
                    this.input(Field::ChatModel)
                        .update(cx, |i, cx| i.set_value(model, window, cx));
                }))
                .into_any_element()
            })
            .collect();
        vec![
            row("Model for new chats", "A CLI alias or full model name; empty uses the account's default.", cx)
                .child(Input::new(&self.input(Field::ChatModel)))
                .child(chips(model_chips))
                .into_any_element(),
            row("Effort for new chats", "How long the model thinks before it answers.", cx)
                .child(choice("effort", &effort, s.chat_effort, |s, v| s.chat_effort = v, cx))
                .into_any_element(),
            row("Mode for new chats", "What a new chat may do without asking.", cx)
                .child(choice("mode", &modes, s.chat_mode, |s, v| s.chat_mode = v, cx))
                .into_any_element(),
            row(
                "Switch account at limit",
                "When a Claude or Codex chat's account reaches its 5-hour or weekly limit, the chat can carry on as your next account of the same CLI that has room, with the whole conversation. A message the limit refused is sent again there.",
                cx,
            )
            .child(choice(
                "autoswitch",
                &[(AutoSwitchMode::Ask, "Ask first"), (AutoSwitchMode::Auto, "Automatically"), (AutoSwitchMode::Off, "Off")],
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
            .when(s.auto_switch_mode != AutoSwitchMode::Off, |d| {
                d.child(choice("switchat", &[(80, "80%"), (90, "90%"), (95, "95%"), (100, "100%")], s.auto_switch_at, |s, v| s.auto_switch_at = v, cx))
            })
            .into_any_element(),
        ]
    }

    fn appearance(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.settings.clone();
        let accents: Vec<(Accent, &str)> = Accent::ALL
            .iter()
            .map(|a| {
                (
                    *a,
                    if *a == Accent::Theme {
                        "Theme's own"
                    } else {
                        a.as_str()
                    },
                )
            })
            .collect();
        let palettes: Vec<(TerminalPalette, &str)> = TerminalPalette::ALL
            .iter()
            .map(|p| (*p, palette_label(*p)))
            .collect();
        let fonts: Vec<(TerminalFont, &str)> =
            TerminalFont::ALL.iter().map(|f| (*f, f.as_str())).collect();
        vec![
            row(
                "Theme",
                "Dark, light, or follow the system; each mode draws its own theme.",
                cx,
            )
            .child(choice(
                "theme",
                &[
                    (ThemePreference::System, "System"),
                    (ThemePreference::Dark, "Dark"),
                    (ThemePreference::Light, "Light"),
                ],
                s.theme,
                |s, v| s.theme = v,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.input(Field::ThemeDark))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.input(Field::ThemeLight))),
                    ),
            )
            .child(hint("Dark theme, then light theme, by id.", cx))
            .into_any_element(),
            row(
                "Interface size",
                "The whole app's zoom, like a browser (Ctrl+= / Ctrl+- / Ctrl+0).",
                cx,
            )
            .child(stepper(
                "zoom",
                format!("{}%", (s.ui_zoom * 100.0).round()),
                |s| s.ui_zoom = step_zoom(s.ui_zoom, -1),
                |s| s.ui_zoom = step_zoom(s.ui_zoom, 1),
                cx,
            ))
            .into_any_element(),
            row(
                "Accent color",
                "Buttons, focus rings and the selected items.",
                cx,
            )
            .child(choice(
                "accent",
                &accents,
                s.accent,
                |s, v| s.accent = v,
                cx,
            ))
            .into_any_element(),
            row(
                "Terminal colors",
                "Match follows the app theme; the rest are fixed schemes.",
                cx,
            )
            .child(choice(
                "palette",
                &palettes,
                s.terminal_palette,
                |s, v| s.terminal_palette = v,
                cx,
            ))
            .into_any_element(),
            row("Terminal font", "The font and size of terminal panes.", cx)
                .child(choice(
                    "font",
                    &fonts,
                    s.font_family,
                    |s, v| s.font_family = v,
                    cx,
                ))
                .child(stepper(
                    "fontsize",
                    format!("{} px", s.font_size),
                    |s| s.font_size = (s.font_size - 1).max(FONT_SIZE_MIN),
                    |s| s.font_size = (s.font_size + 1).min(FONT_SIZE_MAX),
                    cx,
                ))
                .into_any_element(),
        ]
    }

    fn agents(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.settings.clone();
        let mut rows = vec![row(
            "Claude binary",
            "The claude command panes run. Empty: found on PATH.",
            cx,
        )
        .child(Input::new(&self.input(Field::ClaudePath)))
        .into_any_element()];
        let mut paths = row(
            "Other agent CLIs",
            "Where each CLI's binary is. Empty: found on PATH.",
            cx,
        );
        for cli in AgentCli::ALL.iter().filter(|c| **c != AgentCli::Claude) {
            paths = paths.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(130.)).text_sm().child(cli.spec().label))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.input(Field::CliPath(*cli)))),
                    ),
            );
        }
        rows.push(paths.into_any_element());
        rows.push(
            row(
                "Shared instructions and skills",
                "Overall: each provider's accounts share one set of instructions, skills and mods. Per account: each keeps its own.",
                cx,
            )
            .child(choice("shared", &[(SharedMode::Overall, "Overall"), (SharedMode::PerAccount, "Per account")], s.shared_mode, |s, v| s.shared_mode = v, cx))
            .into_any_element(),
        );
        rows.push(
            row(
                "Shell for plain panes",
                "A path or a name on PATH, with its arguments. Empty: automatic.",
                cx,
            )
            .child(Input::new(&self.input(Field::ShellPath)))
            .child(Input::new(&self.input(Field::ShellArgs)))
            .into_any_element(),
        );
        rows
    }

    fn advanced(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.settings.clone();
        vec![row(
            "Command line",
            "Lets the wraithgrid command, and agents in your panes, list panes, read their screens, type into them, wait for them and open new ones. Only your user can connect; nothing listens on the network.",
            cx,
        )
        .child(toggle("cli", s.cli_access, |s, v| s.cli_access = v, cx))
        .into_any_element()]
    }

    fn keyboard(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mac = cfg!(target_os = "macos");
        let rows = bindings(&self.settings.shortcuts)
            .into_iter()
            .map(|(id, b)| {
                div()
                    .flex()
                    .justify_between()
                    .py_1()
                    .child(shortcut_label(id))
                    .child(
                        div()
                            .font_family(crate::terminal::mono(cx))
                            .text_color(cx.theme().muted_foreground)
                            .child(chord(&b, mac)),
                    )
            });
        let fixed = [
            (
                "Move focus between panes",
                if mac {
                    "⌘ ⌥ Arrows"
                } else {
                    "Ctrl Alt Arrows"
                },
            ),
            (
                "Switch to project 1–9",
                if mac {
                    "⌘ ⇧ 1–9"
                } else {
                    "Ctrl Shift 1–9"
                },
            ),
            (
                "Zoom the app in / out / reset",
                if mac {
                    "⌘ = / ⌘ - / ⌘ 0"
                } else {
                    "Ctrl = / Ctrl - / Ctrl 0"
                },
            ),
            (
                "Copy / paste in a terminal",
                if mac {
                    "⌘ C / ⌘ V"
                } else {
                    "Ctrl Shift C / Ctrl Shift V"
                },
            ),
        ]
        .into_iter()
        .map(|(what, keys)| {
            div().flex().justify_between().py_1().child(what).child(
                div()
                    .font_family(crate::terminal::mono(cx))
                    .text_color(cx.theme().muted_foreground)
                    .child(keys),
            )
        });
        vec![row(
            "Keyboard shortcuts",
            "App shortcuts always take Shift or Alt, so plain chords stay with the terminal.",
            cx,
        )
        .children(rows)
        .children(fixed)
        .into_any_element()]
    }
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = match self.tab {
            Tab::General => self.general(cx),
            Tab::Chat => self.chat(cx),
            Tab::Appearance => self.appearance(cx),
            Tab::Agents => self.agents(cx),
            Tab::Advanced => self.advanced(cx),
            Tab::Keyboard => self.keyboard(cx),
        };
        let tabs = TABS.iter().map(|(tab, label)| {
            let tab = *tab;
            div()
                .id(SharedString::from(format!("tab-{label}")))
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(self.tab == tab, |d| d.bg(theme.secondary))
                .hover(|d| d.bg(theme.secondary))
                .child(*label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = tab;
                    cx.notify();
                }))
        });
        let title = TABS
            .iter()
            .find(|(t, _)| *t == self.tab)
            .map(|(_, l)| *l)
            .unwrap_or("");
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .bg(theme.background)
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(SettingsEvent::Close);
                }
            }))
            .child(
                div()
                    .w(px(200.))
                    .h_full()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .border_r_1()
                    .border_color(theme.border)
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
                            .id("settings-close")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.secondary))
                            .child("← Back to panes")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                    ),
            )
            .child(
                div()
                    .id("settings-body")
                    .flex_1()
                    .h_full()
                    .overflow_y_scroll()
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .max_w(px(820.))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .children(rows),
            )
    }
}

// ---- Row building blocks -------------------------------------------------------------------

fn row(title: &'static str, explain: &'static str, cx: &App) -> Div {
    div()
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

fn chips(children: Vec<AnyElement>) -> Div {
    div().flex().flex_wrap().gap_2().children(children)
}

fn chip(id: SharedString, label: impl Into<SharedString>, on: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .text_sm()
        .cursor_pointer()
        .border_color(if on { theme.ring } else { theme.border })
        .when(on, |d| d.bg(theme.secondary))
        .hover(|d| d.bg(theme.secondary))
        .child(label.into())
}

/// One of several values: a row of chips, the current one marked.
fn choice<T: Copy + PartialEq + 'static>(
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
            .on_click(cx.listener(move |this, _, _, cx| this.update(cx, |s| set(s, value))))
            .into_any_element()
        })
        .collect();
    chips(items)
}

fn toggle(
    key: &'static str,
    on: bool,
    set: fn(&mut Settings, bool),
    cx: &mut Context<SettingsView>,
) -> Div {
    choice(key, &[(true, "On"), (false, "Off")], on, set, cx)
}

fn stepper(
    key: &'static str,
    value: String,
    down: fn(&mut Settings),
    up: fn(&mut Settings),
    cx: &mut Context<SettingsView>,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            chip(SharedString::from(format!("{key}-down")), "−", false, cx)
                .on_click(cx.listener(move |this, _, _, cx| this.update(cx, down))),
        )
        .child(div().w(px(70.)).text_center().child(value))
        .child(
            chip(SharedString::from(format!("{key}-up")), "+", false, cx)
                .on_click(cx.listener(move |this, _, _, cx| this.update(cx, up))),
        )
}

fn chord(b: &Binding, mac: bool) -> String {
    let key = match b.code.as_str() {
        "Slash" => "/".to_string(),
        code => code
            .trim_start_matches("Key")
            .trim_start_matches("Digit")
            .to_string(),
    };
    let mut parts = vec![if mac { "⌘" } else { "Ctrl" }];
    if b.shift {
        parts.push(if mac { "⇧" } else { "Shift" });
    }
    if b.alt {
        parts.push(if mac { "⌥" } else { "Alt" });
    }
    format!("{} {key}", parts.join(" "))
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

fn palette_label(p: TerminalPalette) -> &'static str {
    match p {
        TerminalPalette::Match => "Match theme",
        TerminalPalette::Dracula => "Dracula",
        TerminalPalette::Nord => "Nord",
        TerminalPalette::TokyoNight => "Tokyo Night",
        TerminalPalette::GruvboxDark => "Gruvbox dark",
        TerminalPalette::SolarizedDark => "Solarized dark",
        TerminalPalette::SolarizedLight => "Solarized light",
    }
}
