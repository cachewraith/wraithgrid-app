//! The app's look from the user's theme (`src/renderer/app/theme.ts`): the settings and the
//! OS light/dark mode pick a theme (built-in or from ~/.wraithgrid/themes) through the
//! `wraithgrid-themes` crate; its colors are laid onto gpui-component's theme, and its terminal
//! palette onto the terminal panes.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use wraithgrid_core::config::Config;
use wraithgrid_core::paths::home_dir;
use wraithgrid_themes::{resolve_active_theme, ResolvedTheme, Rgba as ThemeRgba, ThemeSettings};
use wraithgrid_themes::{Accent, TerminalPalette, ThemeName, ThemePreference};

fn hsla(c: ThemeRgba) -> Hsla {
    Rgba {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
    .into()
}

pub fn system_dark(window: &Window) -> bool {
    matches!(
        window.appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// The theme the settings and OS mode pick, resolved to colors.
pub fn resolve(config: &Config, system_dark: bool) -> ResolvedTheme {
    let s = &config.settings;
    let settings = ThemeSettings {
        preference: ThemePreference::parse(s.theme.as_str()).unwrap_or(ThemePreference::Dark),
        theme_dark: s.theme_dark.clone(),
        theme_light: s.theme_light.clone(),
        accent: Accent::parse(s.accent.as_str()).unwrap_or(Accent::Theme),
        terminal_palette: TerminalPalette::parse(s.terminal_palette.as_str())
            .unwrap_or(TerminalPalette::Match),
    };
    resolve_active_theme(
        &settings,
        system_dark,
        &wraithgrid_core::paths::themes_root(&home_dir()),
    )
}

/// What the terminal panes draw with: the theme's (or the fixed palette's) colors.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalColors {
    pub ansi: [Hsla; 16],
    pub foreground: Hsla,
    pub background: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
}

impl Global for TerminalColors {}

/// Lays the resolved theme onto the components and the terminals.
pub fn apply(r: &ResolvedTheme, window: Option<&mut Window>, cx: &mut App) {
    Theme::change(
        if r.mode == ThemeName::Light {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        },
        window,
        cx,
    );
    let c = &r.colors;
    let t = &mut Theme::global_mut(cx).colors;
    t.background = hsla(c.bg);
    t.foreground = hsla(c.text);
    t.border = hsla(c.line);
    t.input = hsla(c.line_strong);
    t.ring = hsla(c.accent);
    t.caret = hsla(c.accent);
    t.selection = hsla(c.selection);
    t.muted = hsla(c.raised);
    t.muted_foreground = hsla(c.muted);
    t.primary = hsla(c.accent_fill);
    t.primary_hover = hsla(c.accent_fill_hover);
    t.primary_active = hsla(c.accent_fill_hover);
    t.primary_foreground = hsla(c.on_accent);
    t.secondary = hsla(c.hover);
    t.secondary_hover = hsla(c.hover);
    t.secondary_active = hsla(c.selection);
    t.secondary_foreground = hsla(c.text);
    t.accent = hsla(c.hover);
    t.accent_foreground = hsla(c.text);
    t.sidebar = hsla(c.sidebar);
    t.sidebar_foreground = hsla(c.text);
    t.sidebar_border = hsla(c.line);
    t.sidebar_accent = hsla(c.hover);
    t.sidebar_accent_foreground = hsla(c.text);
    t.title_bar = hsla(c.sidebar);
    t.title_bar_border = hsla(c.line);
    t.popover = hsla(c.panel);
    t.popover_foreground = hsla(c.text);
    t.danger = hsla(c.err);
    t.success = hsla(c.ok);
    t.warning = hsla(c.warn);
    t.info = hsla(c.info);
    t.link = hsla(c.accent_text);
    t.list_hover = hsla(c.hover);
    t.list_active = hsla(c.selection);
    t.scrollbar_thumb = hsla(c.line_strong);
    t.window_border = hsla(c.frame);
    let term = &r.terminal;
    cx.set_global(TerminalColors {
        ansi: term.ansi.map(hsla),
        foreground: hsla(term.foreground),
        background: hsla(term.background),
        cursor: hsla(term.cursor),
        selection: hsla(term.selection),
    });
    cx.refresh_windows();
}
