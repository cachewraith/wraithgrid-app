//! The active theme as plain colors, for the GPUI app (no CSS engine there). The renderer
//! sets `theme_vars` as CSS variables and lets the browser work out `var()` and
//! `color-mix()`; here those same values are evaluated, so both apps draw the same colors
//! from one set of formulas (theme.rs).

use std::path::Path;

use crate::prefs::{resolve_theme, Accent, TerminalPalette, ThemeName, ThemePreference};
use crate::store::{list_themes, ThemeProblem};
use crate::term::{terminal_theme, TerminalTheme};
use crate::theme::{
    built_in_themes, pick_theme, theme_vars, with_accent, AppTheme, MonoFont, ThemeBorder,
    ThemeDensity, ThemeDepth, ThemeFile, ThemeMotion, ThemeRadius, ThemeVars, UiFont,
};

/// A color, each channel 0–1 (GPUI's `Rgba` layout).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`.
    pub fn from_hex(s: &str) -> Option<Rgba> {
        let h = s.strip_prefix('#')?;
        if !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let digit = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
        let pair = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        let [r, g, b, a] = match h.len() {
            3 => [digit(0)?, digit(1)?, digit(2)?, 255],
            4 => [digit(0)?, digit(1)?, digit(2)?, digit(3)?],
            6 => [pair(0)?, pair(2)?, pair(4)?, 255],
            8 => [pair(0)?, pair(2)?, pair(4)?, pair(6)?],
            _ => return None,
        };
        Some(Rgba {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: f32::from(a) / 255.0,
        })
    }

    fn bytes(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
    }

    /// `#rrggbb`, or `#rrggbbaa` when not opaque.
    pub fn to_hex(self) -> String {
        let [r, g, b, a] = self.bytes();
        if a == 255 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }

    /// `rgba(r, g, b, a)` with the alpha to three decimals.
    pub fn to_css(self) -> String {
        let [r, g, b, _] = self.bytes();
        let a = (f64::from(self.a.clamp(0.0, 1.0)) * 1000.0).round() / 1000.0;
        format!("rgba({r}, {g}, {b}, {a})")
    }

    /// `0xRRGGBBAA`, as `gpui::rgba` takes it.
    pub fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.bytes())
    }
}

/// Evaluates a CSS color as the renderer's values use them: `#hex`, `transparent`,
/// `rgb[a](…)`, `var(--x)` (looked up in `vars`) and `color-mix(in srgb, A p%, B)`.
pub fn eval_css_color(expr: &str, vars: &ThemeVars) -> Option<Rgba> {
    eval(expr, vars, 0)
}

fn eval(expr: &str, vars: &ThemeVars, depth: u32) -> Option<Rgba> {
    // `var()` chains are short; a loop in a theme's values must not hang.
    if depth > 16 {
        return None;
    }
    let e = expr.trim();
    if e == "transparent" {
        return Some(Rgba::TRANSPARENT);
    }
    if e.starts_with('#') {
        return Rgba::from_hex(e);
    }
    if let Some(name) = call(e, "var") {
        return eval(vars.get(name.trim())?, vars, depth + 1);
    }
    if let Some(args) = call(e, "rgba").or_else(|| call(e, "rgb")) {
        let n: Vec<f32> = args
            .split(',')
            .map(|x| x.trim().parse().ok())
            .collect::<Option<_>>()?;
        let (r, g, b, a) = match n[..] {
            [r, g, b] => (r, g, b, 1.0),
            [r, g, b, a] => (r, g, b, a),
            _ => return None,
        };
        return Some(Rgba {
            r: r / 255.0,
            g: g / 255.0,
            b: b / 255.0,
            a,
        });
    }
    let args = split_top(call(e, "color-mix")?);
    let [space, first, second] = &args[..] else {
        return None;
    };
    if space.trim() != "in srgb" {
        return None;
    }
    let (c1, p1) = mix_arg(first, vars, depth)?;
    let (c2, p2) = mix_arg(second, vars, depth)?;
    // Percentages as CSS fills them in: one given, the other is the rest.
    let (p1, p2) = match (p1, p2) {
        (None, None) => (0.5, 0.5),
        (Some(p), None) => (p, 1.0 - p),
        (None, Some(q)) => (1.0 - q, q),
        (Some(p), Some(q)) => (p, q),
    };
    let sum = p1 + p2;
    if sum <= 0.0 {
        return None;
    }
    Some(mix_srgb(c1, p1 / sum, c2, p2 / sum, sum.min(1.0)))
}

/// `color-mix` in sRGB with premultiplied alpha, as browsers do it.
fn mix_srgb(a: Rgba, p: f32, b: Rgba, q: f32, alpha_scale: f32) -> Rgba {
    let alpha = a.a * p + b.a * q;
    if alpha <= 0.0 {
        return Rgba::TRANSPARENT;
    }
    let ch = |x: f32, y: f32| (x * a.a * p + y * b.a * q) / alpha;
    Rgba {
        r: ch(a.r, b.r),
        g: ch(a.g, b.g),
        b: ch(a.b, b.b),
        a: alpha * alpha_scale,
    }
}

/// `name(args)` → `args`.
fn call<'a>(e: &'a str, name: &str) -> Option<&'a str> {
    e.strip_prefix(name)?.strip_prefix('(')?.strip_suffix(')')
}

/// Splits on commas outside parentheses.
fn split_top(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// `color 40%` → the color and 0.4; `color` → the color and no percentage.
fn mix_arg(arg: &str, vars: &ThemeVars, depth: u32) -> Option<(Rgba, Option<f32>)> {
    let arg = arg.trim();
    if let Some((color, pct)) = arg.rsplit_once(' ') {
        if let Some(p) = pct.strip_suffix('%') {
            return Some((
                eval(color, vars, depth + 1)?,
                Some(p.parse::<f32>().ok()? / 100.0),
            ));
        }
    }
    Some((eval(arg, vars, depth + 1)?, None))
}

/// The settings that pick the theme (part of the config's `settings`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeSettings {
    pub preference: ThemePreference,
    /// The theme id drawn in dark mode, and in light mode.
    pub theme_dark: String,
    pub theme_light: String,
    pub accent: Accent,
    pub terminal_palette: TerminalPalette,
}

impl Default for ThemeSettings {
    /// The config's defaults.
    fn default() -> Self {
        Self {
            preference: ThemePreference::Dark,
            theme_dark: "wraith".into(),
            theme_light: "daylight".into(),
            accent: Accent::Theme,
            terminal_palette: TerminalPalette::Match,
        }
    }
}

/// Every UI color token (tokens.css), resolved. Field names follow the theme file's color
/// names; the CSS variable each stands for is in its doc.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiColors {
    /// `--bg`
    pub bg: Rgba,
    /// `--bg2`
    pub sidebar: Rgba,
    /// `--panel`
    pub panel: Rgba,
    /// `--raise`
    pub raised: Rgba,
    /// `--line`
    pub line: Rgba,
    /// `--line2`
    pub line_strong: Rgba,
    /// `--tx`
    pub text: Rgba,
    /// `--mu`
    pub muted: Rgba,
    /// `--fa`
    pub faint: Rgba,
    /// `--acc`
    pub accent: Rgba,
    /// `--acct`
    pub accent_text: Rgba,
    /// `--accf`: the primary button.
    pub accent_fill: Rgba,
    /// `--accf-h`
    pub accent_fill_hover: Rgba,
    /// `--on-acc`: text on the primary button.
    pub on_accent: Rgba,
    /// `--term`: the terminal pane's surround.
    pub terminal: Rgba,
    /// `--ok`: success.
    pub ok: Rgba,
    /// `--warn`
    pub warn: Rgba,
    /// `--err`: danger.
    pub err: Rgba,
    /// `--cy`: info.
    pub info: Rgba,
    /// `--sel`
    pub selection: Rgba,
    /// `--hover`
    pub hover: Rgba,
    /// `--frame`: the window frame.
    pub frame: Rgba,
    /// `--user-bg`: your chat messages.
    pub user_bubble: Rgba,
    /// `--code-bg`
    pub code_block: Rgba,
    /// `--addbg`
    pub diff_add: Rgba,
    /// `--delbg`
    pub diff_del: Rgba,
    /// `--selr`: a selected row's ring.
    pub selection_ring: Rgba,
    /// `--hover-line`
    pub hover_line: Rgba,
    /// `--scrim`: behind dialogs.
    pub scrim: Rgba,
    /// `--banner-bg`
    pub banner_bg: Rgba,
    /// `--banner-line`
    pub banner_line: Rgba,
    /// `--banner-title`
    pub banner_title: Rgba,
    /// `--logo-line`
    pub logo_line: Rgba,
    /// `--empty-line`
    pub empty_line: Rgba,
    /// `--mini-hover`
    pub mini_hover: Rgba,
    /// `--solid`: the solid button (near-white on dark, near-black on light).
    pub solid: Rgba,
    /// `--solid-h`
    pub solid_hover: Rgba,
    /// `--on-solid`
    pub on_solid: Rgba,
}

impl UiColors {
    /// Resolves every color variable of a theme's CSS variables.
    pub fn from_vars(v: &ThemeVars) -> UiColors {
        let c = |name: &str| {
            v.get(name)
                .and_then(|e| eval_css_color(e, v))
                .unwrap_or(Rgba::TRANSPARENT)
        };
        UiColors {
            bg: c("--bg"),
            sidebar: c("--bg2"),
            panel: c("--panel"),
            raised: c("--raise"),
            line: c("--line"),
            line_strong: c("--line2"),
            text: c("--tx"),
            muted: c("--mu"),
            faint: c("--fa"),
            accent: c("--acc"),
            accent_text: c("--acct"),
            accent_fill: c("--accf"),
            accent_fill_hover: c("--accf-h"),
            on_accent: c("--on-acc"),
            terminal: c("--term"),
            ok: c("--ok"),
            warn: c("--warn"),
            err: c("--err"),
            info: c("--cy"),
            selection: c("--sel"),
            hover: c("--hover"),
            frame: c("--frame"),
            user_bubble: c("--user-bg"),
            code_block: c("--code-bg"),
            diff_add: c("--addbg"),
            diff_del: c("--delbg"),
            selection_ring: c("--selr"),
            hover_line: c("--hover-line"),
            scrim: c("--scrim"),
            banner_bg: c("--banner-bg"),
            banner_line: c("--banner-line"),
            banner_title: c("--banner-title"),
            logo_line: c("--logo-line"),
            empty_line: c("--empty-line"),
            mini_hover: c("--mini-hover"),
            solid: c("--solid"),
            solid_hover: c("--solid-h"),
            on_solid: c("--on-solid"),
        }
    }
}

/// The terminal's colors, resolved.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedTerminal {
    pub background: Rgba,
    pub foreground: Rgba,
    pub cursor: Rgba,
    /// The character under a block cursor.
    pub cursor_accent: Rgba,
    pub selection: Rgba,
    pub scrollbar: Rgba,
    pub scrollbar_hover: Rgba,
    pub scrollbar_active: Rgba,
    /// black, red, green, yellow, blue, magenta, cyan, white, then the bright eight.
    pub ansi: [Rgba; 16],
}

impl ResolvedTerminal {
    pub fn from_theme(t: &TerminalTheme) -> ResolvedTerminal {
        let none = ThemeVars::default();
        let c = |s: &str| eval_css_color(s, &none).unwrap_or(Rgba::TRANSPARENT);
        ResolvedTerminal {
            background: c(&t.background),
            foreground: c(&t.foreground),
            cursor: c(&t.cursor),
            cursor_accent: c(&t.cursor_accent),
            selection: c(&t.selection_background),
            scrollbar: c(&t.scrollbar_slider_background),
            scrollbar_hover: c(&t.scrollbar_slider_hover_background),
            scrollbar_active: c(&t.scrollbar_slider_active_background),
            ansi: std::array::from_fn(|i| c(&t.ansi[i])),
        }
    }
}

/// A theme's style, as values.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedStyle {
    pub font: UiFont,
    pub mono_font: MonoFont,
    /// Chat message text, px (14.5 by default).
    pub chat_text_size: f32,
    pub radius: ThemeRadius,
    /// The scale on every rounded corner (`--rs`).
    pub radius_scale: f32,
    pub border: ThemeBorder,
    /// px (`--bw`).
    pub border_width: f32,
    pub density: ThemeDensity,
    /// The spacing scale (`--dens`).
    pub density_scale: f32,
    pub depth: ThemeDepth,
    pub motion: ThemeMotion,
    /// The faint accent glow over the window frame, when the theme has one: the color at
    /// the top-left corner, fading to transparent.
    pub frame_glow: Option<Rgba>,
}

impl ResolvedStyle {
    fn of(t: &ThemeFile, colors: &UiColors) -> ResolvedStyle {
        let s = t.style.clone().unwrap_or_default();
        let radius = s.radius.unwrap_or(ThemeRadius::Medium);
        let border = s.border.unwrap_or(ThemeBorder::Thin);
        let density = s.density.unwrap_or(ThemeDensity::Comfortable);
        let glow = if t.mode == ThemeName::Dark {
            0.10
        } else {
            0.07
        };
        ResolvedStyle {
            font: s.font.unwrap_or(UiFont::Geist),
            mono_font: s.mono_font.unwrap_or(MonoFont::JetBrainsMono),
            chat_text_size: s.chat_text_size.map_or(14.5, f32::from),
            radius,
            radius_scale: radius.scale() as f32,
            border,
            border_width: border.px() as f32,
            density,
            density_scale: density.scale() as f32,
            depth: s.depth.unwrap_or(ThemeDepth::Soft),
            motion: s.motion.unwrap_or(ThemeMotion::On),
            frame_glow: (s.frame_gradient == Some(true)).then_some(Rgba {
                a: colors.accent.a * glow,
                ..colors.accent
            }),
        }
    }
}

/// The theme the app draws, all worked out.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTheme {
    /// The picked theme's id and name.
    pub id: String,
    pub name: String,
    pub built_in: bool,
    pub mode: ThemeName,
    /// The theme with the accent laid over it (what the renderer calls `drawn`).
    pub drawn: ThemeFile,
    pub colors: UiColors,
    pub style: ResolvedStyle,
    /// The terminal palette from the settings (`match`: the theme's own). A fixed palette
    /// also colors the pane around the terminal: use `terminal.background` there.
    pub terminal: ResolvedTerminal,
    /// The same, as strings.
    pub terminal_strings: TerminalTheme,
}

/// The built-in themes, then the user's from `themes_dir`, and the files that failed.
pub fn all_themes(themes_dir: &Path) -> (Vec<AppTheme>, Vec<ThemeProblem>) {
    let list = list_themes(themes_dir);
    let mut themes = built_in_themes().to_vec();
    themes.extend(list.themes);
    (themes, list.problems)
}

/// The picked theme for the settings and OS mode, and it with the accent laid over it.
pub fn drawn_theme<'a>(
    themes: &'a [AppTheme],
    settings: &ThemeSettings,
    system_dark: bool,
) -> (&'a AppTheme, ThemeFile) {
    let mode = resolve_theme(settings.preference, system_dark);
    let id = match mode {
        ThemeName::Dark => &settings.theme_dark,
        ThemeName::Light => &settings.theme_light,
    };
    let t = pick_theme(themes, id, mode);
    (t, with_accent(t, settings.accent))
}

/// The active theme from a list of themes (built-ins plus the user's; see `all_themes`).
pub fn resolve_ui_theme(
    themes: &[AppTheme],
    settings: &ThemeSettings,
    system_dark: bool,
) -> ResolvedTheme {
    let (picked, drawn) = drawn_theme(themes, settings, system_dark);
    resolved(
        &picked.id,
        picked.built_in,
        drawn,
        settings.terminal_palette,
    )
}

/// A theme file drawn as it is, no accent laid over it: the theme editor's draft.
pub fn resolve_draft(draft: &ThemeFile, palette: TerminalPalette) -> ResolvedTheme {
    resolved("", false, draft.clone(), palette)
}

fn resolved(id: &str, built_in: bool, drawn: ThemeFile, palette: TerminalPalette) -> ResolvedTheme {
    let colors = UiColors::from_vars(&theme_vars(&drawn));
    let term = terminal_theme(palette, &drawn);
    ResolvedTheme {
        id: id.to_owned(),
        // `with_accent` keeps the name.
        name: drawn.name.clone(),
        built_in,
        mode: drawn.mode,
        style: ResolvedStyle::of(&drawn, &colors),
        colors,
        terminal: ResolvedTerminal::from_theme(&term),
        terminal_strings: term,
        drawn,
    }
}

/// The active theme for the settings: reads the user's themes from `themes_dir`
/// (~/.wraithgrid/themes, see `themes_root`) and resolves the one to draw.
pub fn resolve_active_theme(
    settings: &ThemeSettings,
    system_dark: bool,
    themes_dir: &Path,
) -> ResolvedTheme {
    let (themes, _) = all_themes(themes_dir);
    resolve_ui_theme(&themes, settings, system_dark)
}
