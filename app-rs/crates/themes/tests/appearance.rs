//! The theme parts of tests/unit/appearance.test.ts (resolveTheme, terminalTheme), and the
//! resolved colors the GPUI app draws with.

use std::fs;

use wraithgrid_themes::{
    built_in_themes, eval_css_color, palette_label, resolve_active_theme, resolve_theme,
    resolve_ui_theme, terminal_font_stack, terminal_theme, theme_vars, themes_root, with_accent,
    Accent, Rgba, TerminalPalette, ThemeName, ThemePreference, ThemeSettings, ACCENTS,
    TERMINAL_PALETTES,
};

fn built(id: &str) -> &'static wraithgrid_themes::AppTheme {
    built_in_themes().iter().find(|t| t.id == id).unwrap()
}

#[test]
fn resolve_theme_follows_the_os_only_for_system() {
    assert_eq!(
        resolve_theme(ThemePreference::System, true),
        ThemeName::Dark
    );
    assert_eq!(
        resolve_theme(ThemePreference::System, false),
        ThemeName::Light
    );
    assert_eq!(resolve_theme(ThemePreference::Dark, false), ThemeName::Dark);
    assert_eq!(
        resolve_theme(ThemePreference::Light, true),
        ThemeName::Light
    );
}

#[test]
fn terminal_theme_is_the_same_for_the_same_inputs() {
    let wraith = built("wraith");
    assert_eq!(
        terminal_theme(TerminalPalette::Match, wraith),
        terminal_theme(TerminalPalette::Match, wraith)
    );
    assert_eq!(
        terminal_theme(TerminalPalette::Nord, wraith),
        terminal_theme(TerminalPalette::Nord, built("daylight"))
    );
}

#[test]
fn match_uses_the_themes_own_terminal_colors() {
    for theme in built_in_themes() {
        let t = terminal_theme(TerminalPalette::Match, theme);
        let own = theme.terminal.as_ref().unwrap();
        assert_eq!(Some(t.background.as_str()), own.background.as_deref());
        assert_eq!(Some(t.cursor.as_str()), own.cursor.as_deref());
        assert_eq!(Some(t.ansi[1].as_str()), own.red.as_deref());
    }
}

#[test]
fn an_accent_tints_only_cursor_and_selection() {
    let wraith = built("wraith");
    let own = terminal_theme(TerminalPalette::Match, wraith);
    let mut amber = terminal_theme(TerminalPalette::Match, &with_accent(wraith, Accent::Amber));
    assert_ne!(amber.cursor, own.cursor);
    assert_ne!(amber.selection_background, own.selection_background);
    amber.cursor = own.cursor.clone();
    amber.selection_background = own.selection_background.clone();
    amber.scrollbar_slider_active_background = own.scrollbar_slider_active_background.clone();
    assert_eq!(amber, own);
}

#[test]
fn defines_all_16_ansi_colors_and_a_label_for_every_palette() {
    let hex = |s: &str| {
        s.len() == 7
            && s.starts_with('#')
            && s[1..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    for p in TERMINAL_PALETTES {
        assert!(!palette_label(p).is_empty());
        for theme in built_in_themes() {
            for accent in ACCENTS {
                let t = terminal_theme(p, &with_accent(theme, accent));
                for c in &t.ansi {
                    assert!(hex(c), "{p} {c}");
                }
            }
        }
    }
    assert_eq!(palette_label(TerminalPalette::Match), "Match app");
    assert_eq!(palette_label(TerminalPalette::GruvboxDark), "Gruvbox");
}

#[test]
fn selection_and_scrollbars_are_rgba_as_xterm_took_them() {
    let t = terminal_theme(TerminalPalette::Match, built("wraith"));
    // #7c5cff61: 0x61 / 255 = 0.38.
    assert_eq!(t.selection_background, "rgba(124, 92, 255, 0.38)");
    assert_eq!(t.scrollbar_slider_background, "rgba(236, 236, 236, 0.2)");
    assert_eq!(
        terminal_font_stack("Fira Code"),
        "'Fira Code', 'JetBrains Mono', ui-monospace, monospace"
    );
}

// ---- resolved colors for the GPUI app ----------------------------------------------------

fn hex(c: Rgba) -> String {
    c.to_hex()
}

#[test]
fn css_colors_evaluate_as_a_browser_mixes_them() {
    let t = parse(
        r##"{"name":"x","mode":"dark","colors":{"bg":"#000000","text":"#ffffff","accent":"#ff0000"}}"##,
    );
    let v = theme_vars(&t);
    // 89% black + 11% white.
    assert_eq!(hex(eval_css_color("var(--line)", &v).unwrap()), "#1c1c1c");
    // Mixed with transparent: the color at 10% alpha (premultiplied).
    let add = eval_css_color("var(--addbg)", &v).unwrap();
    assert_eq!(add.to_css(), "rgba(74, 222, 128, 0.1)");
    assert_eq!(
        eval_css_color("rgba(0, 0, 0, 0.55)", &v).unwrap().to_css(),
        "rgba(0, 0, 0, 0.55)"
    );
    assert_eq!(eval_css_color("var(--nope)", &v), None);
}

fn parse(s: &str) -> wraithgrid_themes::ThemeFile {
    wraithgrid_themes::parse_theme_file(&serde_json::from_str(s).unwrap()).unwrap()
}

#[test]
fn a_built_in_resolves_to_its_own_colors() {
    let r = resolve_ui_theme(built_in_themes(), &ThemeSettings::default(), false);
    assert_eq!((r.id.as_str(), r.mode), ("wraith", ThemeName::Dark));
    let c = &r.colors;
    assert_eq!(hex(c.bg), "#1c1c1c");
    assert_eq!(hex(c.sidebar), "#171717");
    assert_eq!(hex(c.accent_fill), "#6a4cf0");
    assert_eq!(hex(c.err), "#fb7185");
    assert_eq!(c.diff_add.to_css(), "rgba(74, 222, 128, 0.122)");
    assert_eq!(c.scrim.to_css(), "rgba(0, 0, 0, 0.55)");
    // The tokens.css first-paint values follow from the same formulas.
    assert_eq!(hex(c.selection_ring), "#3a3a3a");
    assert_eq!(hex(c.solid), "#ececec");
    assert_eq!(hex(c.on_solid), "#171717");
    assert_eq!(hex(c.mini_hover), "#473982");
    assert_eq!(hex(r.terminal.background), "#1c1c1c");
    assert_eq!(hex(r.terminal.ansi[1]), "#fb7185");
    assert_eq!(r.style.radius_scale, 1.2);
    assert_eq!(r.style.frame_glow, None);
}

#[test]
fn settings_pick_the_mode_theme_accent_and_palette() {
    let s = ThemeSettings {
        preference: ThemePreference::System,
        theme_dark: "clay".into(),
        theme_light: "paper".into(),
        accent: Accent::Teal,
        terminal_palette: TerminalPalette::Nord,
    };
    let light = resolve_ui_theme(built_in_themes(), &s, false);
    assert_eq!((light.id.as_str(), light.mode), ("paper", ThemeName::Light));
    assert_eq!(hex(light.colors.accent), "#14b8a6");
    assert_eq!(hex(light.colors.accent_text), "#0f766e");
    assert_eq!(hex(light.terminal.background), "#2e3440");
    assert_eq!(light.style.border_width, 1.5);
    let dark = resolve_ui_theme(built_in_themes(), &s, true);
    assert_eq!(dark.id, "clay");
    assert_eq!(hex(dark.colors.accent_text), "#5eead4");
    // Clay's frame glow: the accent at 10%.
    let glow = dark.style.frame_glow.unwrap();
    assert!((glow.a - 0.1).abs() < 1e-6);
    // The bubble is mixed again from the teal accent and the raised color.
    assert_ne!(hex(dark.colors.user_bubble), "#433129");
}

#[test]
fn a_user_theme_from_the_folder_is_drawn_with_mixed_colors() {
    let home = tempfile::tempdir().unwrap();
    let dir = themes_root(home.path());
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("ocean.json"),
        r##"{"name":"Ocean","mode":"light","colors":{"bg":"#ffffff","text":"#000000","accent":"#0000ff"}}"##,
    )
    .unwrap();
    let s = ThemeSettings {
        preference: ThemePreference::Light,
        theme_light: "ocean".into(),
        ..ThemeSettings::default()
    };
    let r = resolve_active_theme(&s, true, &dir);
    assert_eq!((r.id.as_str(), r.built_in), ("ocean", false));
    // 95% white + 5% black.
    assert_eq!(hex(r.colors.panel), "#f2f2f2");
    // Light status colors come from Daylight.
    assert_eq!(hex(r.colors.ok), "#15803d");
    // 78% accent + 22% black.
    assert_eq!(hex(r.colors.accent_text), "#0000c7");
    assert_eq!(hex(r.terminal.background), "#ffffff");
    assert_eq!(r.terminal.selection.to_css(), "rgba(0, 0, 255, 0.22)");
    // A missing folder or theme falls back to the built-in default.
    let gone = resolve_active_theme(&s, true, &home.path().join("nope"));
    assert_eq!(gone.id, "daylight");
}

#[test]
fn rgba_round_trips() {
    let c = Rgba::from_hex("#7c5cff61").unwrap();
    assert_eq!(c.to_hex(), "#7c5cff61");
    assert_eq!(c.to_u32(), 0x7c5cff61);
    assert_eq!(Rgba::from_hex("#fff").unwrap().to_hex(), "#ffffff");
    assert_eq!(Rgba::from_hex("red"), None);
}
