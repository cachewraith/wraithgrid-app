//! The string enums of the domain model (`src/shared/types.ts`, the enums of
//! `src/shared/chat.ts` the config stores) and the constants the schema checks against.

/// A closed set of strings as an enum: `as_str`, `parse`, `ALL`, and serde as the string.
macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some($name::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                $name::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("unknown {}: {s}", stringify!($name))))
            }
        }
    };
}
pub(crate) use str_enum;

str_enum!(
    /// What the user picked; `system` follows the OS light/dark setting.
    ThemePreference { Dark = "dark", Light = "light", System = "system" }
);

str_enum!(Accent { Theme = "theme", Violet = "violet", Blue = "blue", Teal = "teal", Amber = "amber", Rose = "rose" });

str_enum!(
    /// `match` follows the app theme and accent; the rest are fixed terminal color schemes.
    TerminalPalette {
        Match = "match",
        Dracula = "dracula",
        Nord = "nord",
        TokyoNight = "tokyo-night",
        GruvboxDark = "gruvbox-dark",
        SolarizedDark = "solarized-dark",
        SolarizedLight = "solarized-light",
    }
);

str_enum!(TerminalFont {
    JetBrainsMono = "JetBrains Mono",
    FiraCode = "Fira Code",
    IbmPlexMono = "IBM Plex Mono",
    SourceCodePro = "Source Code Pro",
});

str_enum!(SharedMode { Overall = "overall", PerAccount = "per-account" });

str_enum!(
    /// How an agent pane is shown: a chat over the CLI's headless mode, or its own terminal UI.
    PaneView { Chat = "chat", Terminal = "terminal", Preview = "preview" }
);

str_enum!(
    /// What closing the window does while agents run: ask, stop them, or keep them running.
    CloseMode { Ask = "ask", Stop = "stop", Keep = "keep" }
);

str_enum!(
    /// At an account's limit (#61, #161): ask before switching, switch on its own, or stay.
    AutoSwitchMode { Ask = "ask", Auto = "auto", Off = "off" }
);

str_enum!(ChatMode {
    Default = "default",
    AcceptEdits = "acceptEdits",
    Plan = "plan",
    Auto = "auto",
    DontAsk = "dontAsk",
    BypassPermissions = "bypassPermissions",
});

str_enum!(ChatEffort { Low = "low", Medium = "medium", High = "high", Xhigh = "xhigh", Max = "max" });

str_enum!(SplitDir { Row = "row", Col = "col" });

str_enum!(ShortcutId {
    NewPane = "newPane",
    ClosePane = "closePane",
    ToggleZoom = "toggleZoom",
    Palette = "palette",
    Diff = "diff",
    Shortcuts = "shortcuts",
});

pub const FONT_SIZE_MIN: i64 = 10;
pub const FONT_SIZE_MAX: i64 = 20;
pub const FONT_SIZE_DEFAULT: i64 = 13;
pub const SIDEBAR_WIDTH_MIN: i64 = 180;
pub const SIDEBAR_WIDTH_MAX: i64 = 480;
pub const SIDEBAR_WIDTH_DEFAULT: i64 = 236;
pub const DIFF_WIDTH_MIN: i64 = 280;
pub const DIFF_WIDTH_MAX: i64 = 1000;
pub const DIFF_WIDTH_DEFAULT: i64 = 440;
pub const MOD_DIRS_MAX: usize = 16;
pub const TEAM_MAX: usize = 8;
pub const HISTORY_MAX: usize = 100;
pub const SCHEDULES_MAX: usize = 30;
pub const SCHEDULE_RUNS_MAX: usize = 20;

/// App zoom (Ctrl+= / Ctrl+- / Ctrl+0), the steps a browser uses.
pub const UI_ZOOM_STEPS: [f64; 10] = [0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

/// The next zoom step up (+1) or down (-1) from `z`; 0 resets to 100%.
pub fn step_zoom(z: f64, delta: i8) -> f64 {
    match delta {
        0 => 1.0,
        d if d > 0 => *UI_ZOOM_STEPS
            .iter()
            .find(|&&s| s > z + 0.001)
            .unwrap_or(&2.0),
        _ => *UI_ZOOM_STEPS
            .iter()
            .rev()
            .find(|&&s| s < z - 0.001)
            .unwrap_or(&0.67),
    }
}

pub const ACCOUNT_COLORS: [(&str, &str); 4] = [
    ("violet", "#7c5cff"),
    ("teal", "#2dd4bf"),
    ("amber", "#f5a524"),
    ("rose", "#f43f5e"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip() {
        assert_eq!(
            TerminalPalette::parse("tokyo-night"),
            Some(TerminalPalette::TokyoNight)
        );
        assert_eq!(ChatMode::BypassPermissions.as_str(), "bypassPermissions");
        assert_eq!(serde_json::to_string(&PaneView::Chat).unwrap(), "\"chat\"");
        assert!(serde_json::from_str::<PaneView>("\"nope\"").is_err());
    }

    #[test]
    fn zoom_steps() {
        assert_eq!(step_zoom(1.0, 1), 1.1);
        assert_eq!(step_zoom(1.0, -1), 0.9);
        assert_eq!(step_zoom(2.0, 1), 2.0);
        assert_eq!(step_zoom(0.67, -1), 0.67);
        assert_eq!(step_zoom(1.3, 0), 1.0);
    }
}
