//! The theme editor (`ThemeEditor.tsx`, #24), under Settings → Appearance → Theme: starts from
//! a theme, edits a draft that the whole app draws live, warns about hard-to-read pairs, and
//! saves the draft as a new theme file in ~/.wraithgrid/themes (checked again by the themes
//! crate, as main does). Leaving it puts the chosen theme back.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use wraithgrid_core::config::Settings;
use wraithgrid_core::paths::{home_dir, themes_root};
use wraithgrid_core::types::{Accent, ThemePreference};
use wraithgrid_themes::{
    all_themes, contrast_issues, drawn_theme, resolve_active_theme, resolve_draft, save_theme,
    theme_file_text, theme_slug, AppTheme, MonoFont, TerminalPalette, ThemeBorder, ThemeColor,
    ThemeDensity, ThemeDepth, ThemeFile, ThemeMotion, ThemeName, ThemeRadius, ThemeStyle, UiFont,
    CHAT_TEXT_MAX, CHAT_TEXT_MIN, THEMES_FILE_MAX,
};

use ThemeColor as C;

const COLOR_GROUPS: &[(&str, &[(ThemeColor, &str)])] = &[
    (
        "Surfaces",
        &[
            (C::Bg, "Background"),
            (C::Sidebar, "Sidebar"),
            (C::Panel, "Menus & cards"),
            (C::Raised, "Raised"),
            (C::Frame, "Window frame"),
            (C::Line, "Borders"),
            (C::LineStrong, "Strong borders"),
        ],
    ),
    (
        "Text",
        &[
            (C::Text, "Text"),
            (C::Muted, "Muted text"),
            (C::Faint, "Hints"),
        ],
    ),
    (
        "Accent",
        &[
            (C::Accent, "Accent"),
            (C::AccentText, "Accent text"),
            (C::AccentFill, "Accent buttons"),
            (C::OnAccent, "Text on accent"),
        ],
    ),
    (
        "Status",
        &[
            (C::Ok, "Success"),
            (C::Warn, "Warning"),
            (C::Err, "Error"),
            (C::Info, "Info"),
        ],
    ),
    (
        "Chat, code & terminal",
        &[
            (C::UserBubble, "Your messages"),
            (C::CodeBlock, "Code blocks"),
            (C::DiffAdd, "Added lines"),
            (C::DiffDel, "Removed lines"),
            (C::Terminal, "Terminal"),
        ],
    ),
];

/// `^#(?:[0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$`, any case.
fn is_hex(s: &str) -> bool {
    s.strip_prefix('#')
        .is_some_and(|h| matches!(h.len(), 3 | 6 | 8) && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn label(o: &str) -> &str {
    match o {
        "small" => "Small",
        "medium" => "Medium",
        "large" => "Large",
        "thin" => "Thin",
        "thick" => "Thick",
        "flat" => "Flat",
        "soft" => "Soft",
        "deep" => "Deep",
        "compact" => "Compact",
        "comfortable" => "Comfortable",
        "spacious" => "Spacious",
        "on" => "On",
        "reduced" => "Reduced",
        "off" => "Off",
        o => o,
    }
}

/// `chooseTheme`: the theme for its mode, with its own accent; that mode too unless the app
/// follows the OS.
pub fn choose_theme(s: &mut Settings, t: &AppTheme) {
    let dark = t.mode == ThemeName::Dark;
    if dark {
        s.theme_dark = t.id.clone();
    } else {
        s.theme_light = t.id.clone();
    }
    s.accent = Accent::Theme;
    if s.theme != ThemePreference::System {
        s.theme = if dark {
            ThemePreference::Dark
        } else {
            ThemePreference::Light
        };
    }
}

fn os_dark(window: Option<&Window>, cx: &App) -> bool {
    let a = window.map_or_else(|| cx.window_appearance(), |w| w.appearance());
    matches!(a, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// Draws the theme the settings pick (again): after the editor, or once theme files changed.
pub fn apply_settings(s: &Settings, window: Option<&mut Window>, cx: &mut App) {
    let dark = os_dark(window.as_deref(), cx);
    let themes_dir = themes_root(&home_dir());
    let r = resolve_active_theme(&crate::theme::theme_settings(s), dark, &themes_dir);
    crate::theme::apply(&r, window, cx);
}

/// `fileOf`: a theme's file part under another name.
fn file_of(t: &ThemeFile, name: &str) -> ThemeFile {
    ThemeFile {
        name: name.to_owned(),
        ..t.clone()
    }
}

/// Keys whose values differ between two JSON objects (a missing key is its own value; a
/// missing object has no keys).
fn differing(a: Value, b: Value) -> usize {
    let keys = |v| match v {
        Value::Object(m) => m,
        _ => Default::default(),
    };
    let (a, b) = (keys(a), keys(b));
    let mut n = a.iter().filter(|(k, v)| b.get(*k) != Some(*v)).count();
    n += b.keys().filter(|k| !a.contains_key(*k)).count();
    n
}

/// A color as `#rrggbb`.
fn hex_of(c: Hsla) -> String {
    let c = c.to_rgb();
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b))
}

fn hsla_of(hex: &str) -> Option<Hsla> {
    let c = wraithgrid_themes::Rgba::from_hex(hex)?;
    Some(
        Rgba {
            r: c.r,
            g: c.g,
            b: c.b,
            a: 1.0,
        }
        .into(),
    )
}

/// A Select row: its value and its label.
#[derive(Clone)]
pub struct Choice {
    value: SharedString,
    label: SharedString,
}

impl Choice {
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Choice {
            value: value.into(),
            label: label.into(),
        }
    }
}

impl SelectItem for Choice {
    type Value = SharedString;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &SharedString {
        &self.value
    }
}

fn select(
    items: Vec<Choice>,
    value: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<SelectState<Vec<Choice>>> {
    let at = items.iter().position(|c| c.value == value);
    cx.new(|cx| SelectState::new(items, at.map(|i| IndexPath::default().row(i)), window, cx))
}

fn names(all: impl Iterator<Item = &'static str>) -> Vec<Choice> {
    all.map(|n| Choice::new(n, n)).collect()
}

pub enum ThemeEditorEvent {
    /// Saved as this new theme: choose it.
    Saved(Box<AppTheme>),
    Close,
}

/// One editable color: the swatch's picker and the hex field.
struct ColorRow {
    color: ThemeColor,
    picker: Entity<ColorPickerState>,
    hex: Entity<InputState>,
}

pub struct ThemeEditor {
    themes: Vec<AppTheme>,
    base: AppTheme,
    draft: ThemeFile,
    history: Vec<ThemeFile>,
    error: Option<String>,
    palette: TerminalPalette,
    start: Entity<SelectState<Vec<Choice>>>,
    name: Entity<InputState>,
    font: Entity<SelectState<Vec<Choice>>>,
    mono: Entity<SelectState<Vec<Choice>>>,
    chat: Entity<SliderState>,
    colors: Vec<ColorRow>,
    _subscriptions: Vec<Subscription>,
}

impl ThemeEditor {
    /// Starts from the theme the settings draw now.
    pub fn new(settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (themes, _) = all_themes(&themes_root(&home_dir()));
        let ts = crate::theme::theme_settings(settings);
        let base = drawn_theme(&themes, &ts, os_dark(Some(window), cx))
            .0
            .clone();
        let draft = file_of(&base, &format!("{} custom", base.name));
        let st = draft.style.clone().unwrap_or_default();
        let mut subs = Vec::new();

        let start_items = themes
            .iter()
            .map(|t| Choice {
                value: t.id.clone().into(),
                label: format!("{} ({})", t.name, t.mode).into(),
            })
            .collect();
        let start = select(start_items, &base.id, window, cx);
        subs.push(cx.subscribe_in(
            &start,
            window,
            |this, _, e: &SelectEvent<Vec<Choice>>, window, cx| {
                let SelectEvent::Confirm(Some(id)) = e else {
                    return;
                };
                let Some(t) = this.themes.iter().find(|t| t.id == id.as_ref()).cloned() else {
                    return;
                };
                let next = file_of(&t, &this.draft.name);
                this.base = t;
                this.change(next, window, cx);
            },
        ));

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Theme name")
                .default_value(draft.name.clone())
        });
        subs.push(
            cx.subscribe_in(&name, window, |this, input, e: &InputEvent, window, cx| {
                if let InputEvent::Change = e {
                    let v: String = input.read(cx).value().chars().take(40).collect();
                    if v != this.draft.name {
                        let next = ThemeFile {
                            name: v,
                            ..this.draft.clone()
                        };
                        this.change(next, window, cx);
                    }
                }
            }),
        );

        let font = select(
            names(UiFont::ALL.iter().map(|f| f.as_str())),
            st.font.unwrap_or(UiFont::Geist).as_str(),
            window,
            cx,
        );
        subs.push(cx.subscribe_in(
            &font,
            window,
            |this, _, e: &SelectEvent<Vec<Choice>>, window, cx| {
                let SelectEvent::Confirm(Some(v)) = e else {
                    return;
                };
                let font = UiFont::parse(v);
                this.set_style(|s| s.font = font, window, cx);
            },
        ));
        let mono = select(
            names(MonoFont::ALL.iter().map(|f| f.as_str())),
            st.mono_font.unwrap_or(MonoFont::JetBrainsMono).as_str(),
            window,
            cx,
        );
        subs.push(cx.subscribe_in(
            &mono,
            window,
            |this, _, e: &SelectEvent<Vec<Choice>>, window, cx| {
                let SelectEvent::Confirm(Some(v)) = e else {
                    return;
                };
                let mono = MonoFont::parse(v);
                this.set_style(|s| s.mono_font = mono, window, cx);
            },
        ));

        let chat = cx.new(|_| {
            SliderState::new()
                .min(f32::from(CHAT_TEXT_MIN))
                .max(f32::from(CHAT_TEXT_MAX))
                .step(1.)
                .default_value(f32::from(st.chat_text_size.unwrap_or(15)))
        });
        subs.push(
            cx.subscribe_in(&chat, window, |this, _, e: &SliderEvent, window, cx| {
                if let SliderEvent::Change(v) = e {
                    let size = v.end().round() as u8;
                    let now = this.draft.style.as_ref().and_then(|s| s.chat_text_size);
                    if now != Some(size) {
                        this.set_style(|s| s.chat_text_size = Some(size), window, cx);
                    }
                }
            }),
        );

        let mut colors = Vec::new();
        for (_, keys) in COLOR_GROUPS {
            for (color, _) in *keys {
                let color = *color;
                let value = draft.colors.get(color).unwrap_or("").to_owned();
                let picker = cx.new(|cx| {
                    let p = ColorPickerState::new(window, cx);
                    match hsla_of(&value) {
                        Some(c) => p.default_value(c),
                        None => p,
                    }
                });
                subs.push(cx.subscribe_in(
                    &picker,
                    window,
                    move |this, _, e: &ColorPickerEvent, window, cx| {
                        let ColorPickerEvent::Change(Some(c)) = e else {
                            return;
                        };
                        // The picker sets the color; an alpha the hex had stays.
                        let alpha = this
                            .draft
                            .colors
                            .get(color)
                            .and_then(|v| v.get(7..))
                            .unwrap_or("")
                            .to_owned();
                        this.set_color(color, Some(format!("{}{alpha}", hex_of(*c))), window, cx);
                    },
                ));
                let required = color.required();
                let hex = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(if required { "#000000" } else { "auto" })
                        .default_value(value)
                });
                // Only a valid color passes (empty leaves it to the theme).
                subs.push(cx.subscribe_in(
                    &hex,
                    window,
                    move |this, input, e: &InputEvent, window, cx| {
                        if let InputEvent::Change = e {
                            let v = input.read(cx).value().trim().to_string();
                            if is_hex(&v) {
                                this.set_color(color, Some(v), window, cx);
                            } else if v.is_empty() && !required {
                                this.set_color(color, None, window, cx);
                            } else {
                                cx.notify();
                            }
                        }
                    },
                ));
                colors.push(ColorRow { color, picker, hex });
            }
        }

        let editor = ThemeEditor {
            themes,
            base,
            draft,
            history: Vec::new(),
            error: None,
            palette: ts.terminal_palette,
            start,
            name,
            font,
            mono,
            chat,
            colors,
            _subscriptions: subs,
        };
        // The whole app is the preview.
        editor.draw(window, cx);
        editor
    }

    fn draw(&self, window: &mut Window, cx: &mut App) {
        crate::theme::apply(&resolve_draft(&self.draft, self.palette), Some(window), cx);
    }

    fn change(&mut self, next: ThemeFile, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.len() >= 50 {
            self.history.remove(0);
        }
        self.history.push(std::mem::replace(&mut self.draft, next));
        self.error = None;
        self.drafted(window, cx);
    }

    /// The draft changed: draw it, and show it in every field typed into elsewhere.
    fn drafted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draw(window, cx);
        let d = &self.draft;
        if self.name.read(cx).value().as_ref() != d.name {
            let name = d.name.clone();
            self.name.update(cx, |i, cx| i.set_value(name, window, cx));
        }
        for row in &self.colors {
            let v = d.colors.get(row.color).unwrap_or("").to_owned();
            if self.hex_of_input(row, cx) != v {
                let v = v.clone();
                row.hex.update(cx, |i, cx| i.set_value(v, window, cx));
            }
            let shown = row.picker.read(cx).value().map(hex_of);
            if let Some(c) = hsla_of(&v).filter(|c| shown != Some(hex_of(*c))) {
                row.picker.update(cx, |p, cx| p.set_value(c, window, cx));
            }
        }
        let st = d.style.clone().unwrap_or_default();
        let base_id: SharedString = self.base.id.clone().into();
        self.start
            .update(cx, |s, cx| s.set_selected_value(&base_id, window, cx));
        let font: SharedString = st.font.unwrap_or(UiFont::Geist).as_str().into();
        self.font
            .update(cx, |s, cx| s.set_selected_value(&font, window, cx));
        let mono: SharedString = st
            .mono_font
            .unwrap_or(MonoFont::JetBrainsMono)
            .as_str()
            .into();
        self.mono
            .update(cx, |s, cx| s.set_selected_value(&mono, window, cx));
        let size = f32::from(st.chat_text_size.unwrap_or(15));
        if self.chat.read(cx).value().end() != size {
            self.chat.update(cx, |s, cx| s.set_value(size, window, cx));
        }
        cx.notify();
    }

    fn hex_of_input(&self, row: &ColorRow, cx: &App) -> String {
        row.hex.read(cx).value().trim().to_string()
    }

    fn set_color(
        &mut self,
        c: ThemeColor,
        v: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut next = self.draft.clone();
        match v {
            Some(v) => next.colors.set(c, v.to_lowercase()),
            None => {
                // Only optional colors are left out; required ones always hold a value.
                let mut map = serde_json::to_value(&next.colors).unwrap_or_default();
                if let Some(m) = map.as_object_mut() {
                    m.remove(c.key());
                }
                if let Ok(colors) = serde_json::from_value(map) {
                    next.colors = colors;
                }
            }
        }
        if next != self.draft {
            self.change(next, window, cx);
        }
    }

    fn set_style(
        &mut self,
        patch: impl FnOnce(&mut ThemeStyle),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut next = self.draft.clone();
        patch(next.style.get_or_insert_with(Default::default));
        if next != self.draft {
            self.change(next, window, cx);
        }
    }

    /// Colors and style keys that differ from the theme it started from.
    fn changes(&self) -> usize {
        let b = &self.base.theme;
        let d = &self.draft;
        let json = |v| serde_json::to_value(v).unwrap_or_default();
        differing(json(&b.colors), json(&d.colors))
            + differing(
                serde_json::to_value(&b.style).unwrap_or_default(),
                serde_json::to_value(&d.style).unwrap_or_default(),
            )
    }

    fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prev) = self.history.pop() {
            self.draft = prev;
            self.error = None;
            self.drafted(window, cx);
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let dir = themes_root(&home_dir());
        let r = serde_json::to_value(&self.draft)
            .map_err(anyhow::Error::from)
            .and_then(|v| save_theme(&v, &dir));
        match r {
            Ok(Ok(id)) => {
                eprintln!("[wraithgrid] saved theme {id}");
                cx.emit(ThemeEditorEvent::Saved(Box::new(AppTheme {
                    id,
                    built_in: false,
                    theme: self.draft.clone(),
                })));
            }
            Ok(Err(e)) => self.error = Some(e),
            Err(_) => self.error = Some("Could not save the theme".into()),
        }
        cx.notify();
    }

    /// Export…: the draft as a file anywhere, written as a theme file is.
    fn export(&mut self, cx: &mut Context<Self>) {
        let text = theme_file_text(&self.draft);
        let name = format!("{}.json", theme_slug(&self.draft.name));
        let pick = cx.prompt_for_new_path(&home_dir(), Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = pick.await else {
                return;
            };
            let error = if text.encode_utf16().count() > THEMES_FILE_MAX {
                Some("Theme is too large.".to_string())
            } else {
                std::fs::write(&path, text)
                    .err()
                    .map(|_| "Could not export the theme".to_string())
            };
            let _ = this.update(cx, |this, cx| {
                this.error = error;
                cx.notify();
            });
        })
        .detach();
    }
}

impl EventEmitter<ThemeEditorEvent> for ThemeEditor {}

/// A small bordered button; `primary` fills it with the accent.
fn button(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    primary: bool,
    enabled: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .text_sm()
        .border_color(if primary { theme.primary } else { theme.border })
        .when(primary, |d| {
            d.bg(theme.primary).text_color(theme.primary_foreground)
        })
        .when(enabled, |d| {
            d.cursor_pointer().hover(|d| {
                if primary {
                    d.bg(theme.primary_hover)
                } else {
                    d.bg(theme.secondary)
                }
            })
        })
        .when(!enabled, |d| d.opacity(0.5))
        .child(text.into())
}

/// A fieldset: a bordered group under a small title.
fn group(title: &'static str, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .p_2()
        .px_3()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(title),
        )
}

/// Pill buttons, one of them on (`Seg`).
fn seg<T: Copy + PartialEq + 'static>(
    key: &'static str,
    options: &[T],
    value: T,
    name: fn(T) -> &'static str,
    set: fn(&mut ThemeStyle, T),
    cx: &mut Context<ThemeEditor>,
) -> Div {
    let theme = cx.theme().clone();
    div()
        .flex()
        .flex_wrap()
        .gap_1()
        .children(options.iter().map(|o| {
            let (o, on) = (*o, *o == value);
            div()
                .id(SharedString::from(format!("ted-{key}-{}", name(o))))
                .px(px(10.))
                .py(px(3.))
                .rounded_full()
                .border_1()
                .text_sm()
                .cursor_pointer()
                .border_color(if on { theme.ring } else { theme.input })
                .text_color(if on {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .when(on, |d| d.bg(theme.ring.opacity(0.12)))
                .hover(|d| d.bg(theme.secondary))
                .child(label(name(o)).to_string())
                .on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.set_style(|s| set(s, o), window, cx)
                    }),
                )
        }))
}

impl Render for ThemeEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let issues = contrast_issues(&self.draft);
        let changes = self.changes();
        let st = self.draft.style.clone().unwrap_or_default();
        let field = |text: &'static str| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        };

        let header = div()
            .flex()
            .flex_wrap()
            .items_end()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(240.))
                    .child(field("Start from"))
                    .child(Select::new(&self.start).small()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .flex_1()
                    .min_w(px(160.))
                    .child(field("Name"))
                    .child(Input::new(&self.name).small()),
            )
            .child(
                div()
                    .ml_auto()
                    .pb_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(match changes {
                        0 => "No changes yet".to_string(),
                        1 => "1 change".to_string(),
                        n => format!("{n} changes"),
                    }),
            );

        let warnings = (!issues.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .px(px(10.))
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.warning.opacity(0.4))
                .bg(theme.warning.opacity(0.08))
                .text_sm()
                .children(issues.iter().map(|i| {
                    let (fg, fix) = (i.fg, i.fix.clone());
                    div()
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .child(
                                    div()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(theme.warning)
                                        .child("Hard to read:"),
                                )
                                .child(format!(
                                    "{} is {} : 1 (needs {} : 1).",
                                    i.label, i.ratio, i.need
                                )),
                        )
                        .child(
                            button(
                                SharedString::from(format!("ted-fix-{}", fg.key())),
                                format!("Use {}", i.fix),
                                false,
                                true,
                                cx,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.set_color(fg, Some(fix.clone()), window, cx)
                                },
                            )),
                        )
                }))
        });

        let mut rows = self.colors.iter();
        let color_groups = COLOR_GROUPS
            .iter()
            .map(|(title, keys)| {
                group(title, cx).children(keys.iter().filter_map(|(_, name)| {
                    let row = rows.next()?;
                    let set = self.draft.colors.get(row.color).is_some();
                    let text = self.hex_of_input(row, cx);
                    let bad = !text.is_empty() && !is_hex(&text);
                    Some(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child(
                                div()
                                    .when(!set, |d| d.opacity(0.35))
                                    .child(ColorPicker::new(&row.picker).small()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .child(*name),
                            )
                            .child(
                                div()
                                    .w(px(108.))
                                    .font_family(crate::terminal::mono(cx))
                                    .when(bad, |d| {
                                        d.rounded_md().border_1().border_color(theme.danger)
                                    })
                                    .child(Input::new(&row.hex).small()),
                            ),
                    )
                }))
            })
            .collect::<Vec<_>>();

        let row = |label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(div().w(px(72.)).flex_none().child(label))
        };
        let glow = st.frame_gradient == Some(true);
        let style = div()
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                group("Type", cx)
                    .child(row("Font").child(div().flex_1().child(Select::new(&self.font).small())))
                    .child(
                        row("Code font")
                            .child(div().flex_1().child(Select::new(&self.mono).small())),
                    )
                    .child(
                        row("Chat text")
                            .child(div().flex_1().child(Slider::new(&self.chat)))
                            .child(div().font_family(crate::terminal::mono(cx)).child(
                                match st.chat_text_size {
                                    Some(n) => format!("{n}px"),
                                    None => "14.5px".into(),
                                },
                            )),
                    ),
            )
            .child(
                group("Shape & depth", cx)
                    .child(seg(
                        "radius",
                        ThemeRadius::ALL,
                        st.radius.unwrap_or(ThemeRadius::Medium),
                        ThemeRadius::as_str,
                        |s, v| s.radius = Some(v),
                        cx,
                    ))
                    .child(seg(
                        "border",
                        ThemeBorder::ALL,
                        st.border.unwrap_or(ThemeBorder::Thin),
                        ThemeBorder::as_str,
                        |s, v| s.border = Some(v),
                        cx,
                    ))
                    .child(seg(
                        "depth",
                        ThemeDepth::ALL,
                        st.depth.unwrap_or(ThemeDepth::Soft),
                        ThemeDepth::as_str,
                        |s, v| s.depth = Some(v),
                        cx,
                    ))
                    .child(
                        Checkbox::new("ted-glow")
                            .checked(glow)
                            .label("Accent glow on the window frame")
                            .on_click(cx.listener(|this, on: &bool, window, cx| {
                                let on = *on;
                                this.set_style(|s| s.frame_gradient = Some(on), window, cx)
                            })),
                    ),
            )
            .child(group("Density", cx).child(seg(
                "density",
                ThemeDensity::ALL,
                st.density.unwrap_or(ThemeDensity::Comfortable),
                ThemeDensity::as_str,
                |s, v| s.density = Some(v),
                cx,
            )))
            .child(
                group("Motion", cx)
                    .child(seg(
                        "motion",
                        ThemeMotion::ALL,
                        st.motion.unwrap_or(ThemeMotion::On),
                        ThemeMotion::as_str,
                        |s, v| s.motion = Some(v),
                        cx,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Your system's reduce-motion setting always wins."),
                    ),
            );

        let can_undo = !self.history.is_empty();
        let can_save = !self.draft.name.trim().is_empty();
        let footer = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .pt_2p5()
            .border_t_1()
            .border_color(theme.border)
            .child(
                button("ted-undo", "Undo", false, can_undo, cx).when(can_undo, |d| {
                    d.on_click(cx.listener(|this, _, window, cx| this.undo(window, cx)))
                }),
            )
            .child(
                button(
                    "ted-reset",
                    format!("Reset to {}", self.base.name),
                    false,
                    changes > 0,
                    cx,
                )
                .when(changes > 0, |d| {
                    d.on_click(cx.listener(|this, _, window, cx| {
                        let next = file_of(&this.base, &this.draft.name);
                        this.change(next, window, cx)
                    }))
                }),
            )
            .child(div().flex_1())
            .child(
                button("ted-cancel", "Cancel", false, true, cx)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ThemeEditorEvent::Close))),
            )
            .child(
                button("ted-export", "Export…", false, true, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.export(cx))),
            )
            .child(
                button("ted-save", "Save as new theme", true, can_save, cx).when(can_save, |d| {
                    d.on_click(cx.listener(|this, _, _, cx| this.save(cx)))
                }),
            );

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p(px(14.))
            .rounded_xl()
            .border_1()
            .border_color(theme.input)
            .bg(theme.popover)
            .child(header)
            .children(warnings)
            .child(
                div()
                    .flex()
                    .gap(px(14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_2p5()
                            .children(color_groups),
                    )
                    .child(div().w(px(300.)).flex_none().child(style)),
            )
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_xs().text_color(theme.warning).child(e))
            })
            .child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::{choose_theme, differing, file_of, is_hex, Accent, AppTheme, ThemeName};
    use super::{ThemePreference, Value};

    #[test]
    fn hex_check_matches_the_editor() {
        for ok in ["#abc", "#ABCDEF", "#11223344"] {
            assert!(is_hex(ok), "{ok}");
        }
        for bad in ["", "#", "abc", "#abcd", "#12345", "#ggg"] {
            assert!(!is_hex(bad), "{bad}");
        }
    }

    #[test]
    fn choose_theme_sets_the_mode_slot_and_accent() {
        let mut s = wraithgrid_core::config::default_config().settings;
        s.accent = Accent::Rose;
        let t = AppTheme {
            id: "ocean".into(),
            built_in: false,
            theme: file_of(&wraithgrid_themes::built_in_themes()[0], "Ocean"),
        };
        choose_theme(&mut s, &t);
        assert_eq!(s.accent, Accent::Theme);
        match t.mode {
            ThemeName::Dark => assert_eq!(s.theme_dark, "ocean"),
            ThemeName::Light => assert_eq!(s.theme_light, "ocean"),
        }
        s.theme = ThemePreference::System;
        choose_theme(&mut s, &t);
        assert_eq!(s.theme, ThemePreference::System);
    }

    #[test]
    fn changes_count_colors_and_style_keys() {
        let a = serde_json::json!({ "bg": "#000", "text": "#fff" });
        let b = serde_json::json!({ "bg": "#111", "accent": "#f00", "text": "#fff" });
        assert_eq!(differing(a, b), 2);
        assert_eq!(differing(Value::Null, Value::Null), 0);
        let style = serde_json::json!({ "radius": "large" });
        assert_eq!(differing(Value::Null, style), 1);
    }
}
