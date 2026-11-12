//! Window chrome per desktop, ported from `src/main/platform.ts` (`detectDesktop`). GPUI's
//! own answer isn't enough: Hyprland grants server-side decorations and then draws none,
//! so the env decides, as in the Electron app.

/// How the window frame is drawn (same meanings as `WindowChrome` in platform.ts).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Chrome {
    /// Our title bar with min/max/close (GNOME, KDE, Xfce, …).
    Custom,
    /// Windows: our buttons, marked as caption areas so snap layouts still work.
    Overlay,
    /// A tiling compositor manages size: only close shows.
    Tiling,
    /// macOS's own traffic lights inset at the left.
    Mac,
}

const TILING_DESKTOPS: &[&str] = &[
    "hyprland",
    "sway",
    "i3",
    "river",
    "niri",
    "bspwm",
    "qtile",
    "dwm",
    "awesome",
    "xmonad",
    "herbstluftwm",
    "leftwm",
    "miracle-wm",
];

/// Same minimum on every desktop: a half tile on a 1920 px screen (DECISIONS 2026-10-21).
pub const MIN_WIDTH: f32 = 640.;
pub const MIN_HEIGHT: f32 = 420.;

pub fn detect(os: &str, env: impl Fn(&str) -> Option<String>) -> Chrome {
    match os {
        "windows" => return Chrome::Overlay,
        "macos" => return Chrome::Mac,
        _ => {}
    }
    let sockets = [
        "HYPRLAND_INSTANCE_SIGNATURE",
        "SWAYSOCK",
        "I3SOCK",
        "NIRI_SOCKET",
    ];
    let tiling_socket = sockets
        .iter()
        .any(|name| env(name).is_some_and(|v| !v.is_empty()));
    let tiling_name = [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .iter()
    .filter_map(|name| env(name))
    .flat_map(|value| {
        value
            .to_lowercase()
            .split([':', ';'])
            .map(String::from)
            .collect::<Vec<_>>()
    })
    .any(|name| TILING_DESKTOPS.contains(&name.as_str()));
    if tiling_socket || tiling_name {
        Chrome::Tiling
    } else {
        Chrome::Custom
    }
}

pub fn current() -> Chrome {
    detect(std::env::consts::OS, |name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn platforms() {
        assert_eq!(detect("windows", env(&[])), Chrome::Overlay);
        assert_eq!(detect("macos", env(&[])), Chrome::Mac);
        assert_eq!(
            detect("linux", env(&[("XDG_CURRENT_DESKTOP", "GNOME")])),
            Chrome::Custom
        );
    }

    #[test]
    fn tiling_by_socket_or_name() {
        assert_eq!(
            detect("linux", env(&[("HYPRLAND_INSTANCE_SIGNATURE", "x")])),
            Chrome::Tiling
        );
        assert_eq!(
            detect("linux", env(&[("XDG_CURRENT_DESKTOP", "KDE:sway")])),
            Chrome::Tiling
        );
        assert_eq!(
            detect("linux", env(&[("DESKTOP_SESSION", "Niri")])),
            Chrome::Tiling
        );
        assert_eq!(detect("linux", env(&[("SWAYSOCK", "")])), Chrome::Custom);
    }
}
