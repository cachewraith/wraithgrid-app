//! The theme settings the config holds (from src/shared/types.ts): light or dark, the
//! accent, and the terminal palette, with the rule that picks the mode actually drawn.

str_enum! {
    /// The theme actually drawn.
    pub enum ThemeName {
        Dark = "dark",
        Light = "light",
    }
}

str_enum! {
    /// What the user picked; `system` follows the OS light/dark setting.
    pub enum ThemePreference {
        Dark = "dark",
        Light = "light",
        System = "system",
    }
}

/// The mode drawn for a preference: `system` follows the OS.
pub fn resolve_theme(pref: ThemePreference, system_dark: bool) -> ThemeName {
    match pref {
        ThemePreference::Dark => ThemeName::Dark,
        ThemePreference::Light => ThemeName::Light,
        ThemePreference::System if system_dark => ThemeName::Dark,
        ThemePreference::System => ThemeName::Light,
    }
}

str_enum! {
    /// The accent setting. `theme` keeps the theme's own accent; the rest are presets.
    pub enum Accent {
        Theme = "theme",
        Violet = "violet",
        Blue = "blue",
        Teal = "teal",
        Amber = "amber",
        Rose = "rose",
    }
}

/// The accent presets (the TS `ACCENTS`; `theme` is not one of them).
pub const ACCENTS: [Accent; 5] = [
    Accent::Violet,
    Accent::Blue,
    Accent::Teal,
    Accent::Amber,
    Accent::Rose,
];

str_enum! {
    /// `match` follows the app theme and accent; the rest are fixed terminal color schemes.
    pub enum TerminalPalette {
        Match = "match",
        Dracula = "dracula",
        Nord = "nord",
        TokyoNight = "tokyo-night",
        GruvboxDark = "gruvbox-dark",
        SolarizedDark = "solarized-dark",
        SolarizedLight = "solarized-light",
    }
}

/// Every terminal palette, in the settings' order.
pub const TERMINAL_PALETTES: [TerminalPalette; 7] = [
    TerminalPalette::Match,
    TerminalPalette::Dracula,
    TerminalPalette::Nord,
    TerminalPalette::TokyoNight,
    TerminalPalette::GruvboxDark,
    TerminalPalette::SolarizedDark,
    TerminalPalette::SolarizedLight,
];
