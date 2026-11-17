//! Electron's stroke icons (`icons.tsx`, 16×16 at 1.6 stroke), drawn as SVG images in any
//! colour; shared by the sidebar, the top row, pane headers and menus.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::*;

/// CSS px that follow the app zoom.
pub(super) fn z(v: f32) -> Rems {
    rems(v / 16.0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Glyph {
    Plus,
    Close,
    Minimize,
    Maximize,
    Sidebar,
    Compose,
    Search,
    Clock,
    Gauge,
    ChevronRight,
    ChevronDown,
    ChevronUp,
    UserPlus,
    Users,
    Mods,
    Settings,
    Rename,
    Trash,
    Monitor,
    Check,
    Play,
    Stop,
    More,
    Back,
    Preset1,
    Preset2,
    Preset3,
    Preset4,
    Zoom,
    Unzoom,
    Keyboard,
    Warn,
    Error,
    Restart,
    Branch,
    Diff,
}

impl Glyph {
    fn body(self) -> &'static str {
        match self {
            Glyph::Plus => r#"<path d="M8 3v10M3 8h10"/>"#,
            Glyph::Close => r#"<path d="M4.5 4.5l7 7M11.5 4.5l-7 7"/>"#,
            Glyph::Minimize => r#"<path d="M4 8h8"/>"#,
            Glyph::Maximize => r#"<rect x="4" y="4" width="8" height="8" rx="1"/>"#,
            Glyph::Sidebar => {
                r#"<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M6 3v10"/>"#
            }
            Glyph::Compose => {
                r#"<path d="M13 9v3.5a1.5 1.5 0 0 1-1.5 1.5h-8A1.5 1.5 0 0 1 2 12.5v-8A1.5 1.5 0 0 1 3.5 3H7"/><path d="M11.5 2.5l2 2L8 10H6V8z"/>"#
            }
            Glyph::Search => r#"<circle cx="7" cy="7" r="4"/><path d="M10 10l3.5 3.5"/>"#,
            Glyph::Clock => r#"<circle cx="8" cy="8" r="6"/><path d="M8 4.5V8l2 1.5"/>"#,
            Glyph::Gauge => {
                r#"<path d="M2.8 11.5a5.5 5.5 0 1 1 10.4 0"/><path d="M8 9.2l2.6-2.6"/>"#
            }
            Glyph::ChevronRight => r#"<path d="M6 4l4 4-4 4"/>"#,
            Glyph::ChevronDown => r#"<g transform="rotate(90 8 8)"><path d="M6 4l4 4-4 4"/></g>"#,
            Glyph::ChevronUp => r#"<g transform="rotate(-90 8 8)"><path d="M6 4l4 4-4 4"/></g>"#,
            Glyph::UserPlus => {
                r#"<circle cx="7" cy="5.5" r="2.5"/><path d="M2.5 13c.6-2.4 2.3-3.5 4.5-3.5 1 0 1.9.2 2.6.7M12 9.5v4M10 11.5h4"/>"#
            }
            Glyph::Users => {
                r#"<circle cx="6" cy="5.5" r="2.3"/><path d="M1.8 13c.4-2.3 2.1-3.6 4.2-3.6s3.8 1.3 4.2 3.6M10.5 3.4a2.2 2.2 0 0 1 0 4.2M12 9.6c1.3.5 2 1.6 2.2 3.4"/>"#
            }
            Glyph::Mods => r#"<path d="M6 2v3M10 2v3M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0zM8 11v3"/>"#,
            Glyph::Settings => {
                r#"<path d="M2.5 4.5h11M2.5 8h11M2.5 11.5h11"/><circle cx="5.5" cy="4.5" r="1.5" fill="{bg}"/><circle cx="10.5" cy="8" r="1.5" fill="{bg}"/><circle cx="6.5" cy="11.5" r="1.5" fill="{bg}"/>"#
            }
            Glyph::Rename => r#"<path d="M10.5 3l2.5 2.5L6 12.5H3.5V10z"/>"#,
            Glyph::Trash => r#"<path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5h5.8l.6-8.5"/>"#,
            Glyph::Monitor => {
                r#"<rect x="2" y="2.5" width="12" height="8.5" rx="1.5"/><path d="M6 13.5h4M8 11v2.5"/>"#
            }
            Glyph::Check => r#"<path d="M3.5 8.5l3 3 6-7"/>"#,
            Glyph::Play => r#"<path d="M5.5 3.8v8.4L12 8z"/>"#,
            Glyph::Stop => r#"<rect x="4.5" y="4.5" width="7" height="7" rx="1.5"/>"#,
            Glyph::More => r#"<path d="M3.5 8h.01M8 8h.01M12.5 8h.01" stroke-width="2.4"/>"#,
            Glyph::Back => r#"<path d="M10 3.5L5.5 8l4.5 4.5"/>"#,
            Glyph::Preset1 => r#"<rect x="2" y="3" width="12" height="10" rx="1.5"/>"#,
            Glyph::Preset2 => {
                r#"<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M8 3v10"/>"#
            }
            Glyph::Preset3 => {
                r#"<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M6 3v10M10 3v10"/>"#
            }
            Glyph::Preset4 => {
                r#"<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M8 3v10M2 8h12"/>"#
            }
            Glyph::Zoom => r#"<path d="M3 6V3h3M13 6V3h-3M3 10v3h3M13 10v3h-3"/>"#,
            Glyph::Unzoom => r#"<path d="M6 3v3H3M10 3v3h3M6 13v-3H3M10 13v-3h3"/>"#,
            Glyph::Keyboard => {
                r#"<rect x="1.5" y="4" width="13" height="8" rx="1.5"/><path d="M4 7h1M7 7h1M10 7h2M4.5 9.5h7"/>"#
            }
            Glyph::Warn => r#"<path d="M8 2.5l6 11H2zM8 7v3M8 11.8v.2"/>"#,
            Glyph::Error => r#"<circle cx="8" cy="8" r="5.5"/><path d="M8 5v3.5M8 10.8v.2"/>"#,
            Glyph::Restart => r#"<path d="M13 8a5 5 0 1 1-1.5-3.5M13 3v2.5h-2.5"/>"#,
            Glyph::Branch => {
                r#"<circle cx="5" cy="3.5" r="1.5"/><circle cx="5" cy="12.5" r="1.5"/><circle cx="11" cy="5" r="1.5"/><path d="M5 5v6M11 6.5c0 2.5-2 3.5-6 4"/>"#
            }
            Glyph::Diff => {
                r#"<rect x="2.5" y="1.5" width="11" height="13" rx="2"/><path d="M8 4v4M6 6h4M6 11h4"/>"#
            }
        }
    }
}

thread_local! {
    static IMAGES: RefCell<HashMap<String, Arc<Image>>> = RefCell::new(HashMap::new());
}

/// One SVG as an image, made once per text (rasterized at 64 px so it stays sharp).
pub(super) fn svg_image(svg: String) -> Arc<Image> {
    IMAGES.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(i) = m.get(&svg) {
            return i.clone();
        }
        let image = Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            svg.clone().into_bytes(),
        ));
        m.insert(svg, image.clone());
        image
    })
}

pub(super) fn hex(c: Hsla) -> (String, f32) {
    let c = Rgba::from(c);
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b)), c.a)
}

/// The icon at `size` CSS px in `color`; `bg` fills the Settings knobs.
pub(super) fn glyph(g: Glyph, size: f32, color: Hsla, bg: Hsla) -> Img {
    let (c, a) = hex(color);
    let (bg, _) = hex(bg);
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 16 16" fill="none" stroke="{c}" stroke-opacity="{a:.3}" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
        g.body().replace("{bg}", &bg)
    );
    img(svg_image(svg)).size(z(size)).flex_none()
}

/// A small icon (`.i.s`, 14 px).
pub(super) fn small(g: Glyph, color: Hsla, bg: Hsla) -> Img {
    glyph(g, 14., color, bg)
}
