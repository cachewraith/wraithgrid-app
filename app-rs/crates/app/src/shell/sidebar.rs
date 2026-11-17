//! The sidebar (`Sidebar.tsx`): New chat / Search / Scheduled, the Usage block (#75), projects
//! as dropdowns of their chats (#67, #87) with their right-click menu (rename inline, icon,
//! where it runs, delete), and the footer's account switcher (#49). Collapsed it is an icon
//! rail; below 900 px the rail opens the full sidebar as a drawer. The edge drags to resize and
//! double-click resets (#88). Also project icons and their picker (#80, `ProjectIcon`,
//! `ProjectIconPicker`), provider marks (`ProviderLogo`, `AccountIcon`) and the menus
//! (`ContextMenu`). Icon sets are bundled (`assets/`), never read from config or the network.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::config::{Pane, Workspace};
use wraithgrid_core::paths::{home_dir, resolve_user_path};
use wraithgrid_core::shortcuts::{bindings, Binding};
use wraithgrid_core::types::{
    AutoSwitchMode, ShortcutId, SIDEBAR_WIDTH_DEFAULT, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN,
};

use super::icons::{glyph, hex, svg_image, z, Glyph};
use super::menu::MenuItem;
use super::{pane_label, Shell};
use crate::a11y::A11y as _;
use crate::team_section::team_pane_ids;

/// Below this window width (CSS px, so zoom counts) the sidebar folds to its rail.
const NARROW_PX: f32 = 900.0;
const RAIL_W: f32 = 52.0;
const DEFAULT_ICON: &str = "ms:folder";
/// Enough icons to browse; typing finds the rest.
const PICKER_MAX: usize = 140;

/// The sidebar's own state, kept on the shell.
#[derive(Default)]
pub(super) struct SidebarState {
    menu: Option<Menu>,
    rename: Option<Rename>,
    picker: Option<Picker>,
    /// The full sidebar over the content in a narrow window.
    drawer: bool,
    /// The width while the edge is dragged (CSS px); saved on release.
    drag_width: Option<f32>,
    /// The focused chat's project last frame: when it changes, its dropdown opens.
    focus_ws: Option<String>,
    /// `view:project`: the drawer closes when it changes.
    place: String,
    /// When each pane was last seen working (`activeAt`, ms).
    active_at: HashMap<String, i64>,
    /// Where each project row was drawn, for its menu opened from the keyboard.
    row_bounds: std::rc::Rc<std::cell::RefCell<HashMap<String, Bounds<Pixels>>>>,
}

struct Menu {
    kind: MenuKind,
    at: Point<Pixels>,
    /// The two-step delete was clicked once.
    confirm: bool,
}

#[derive(PartialEq)]
enum MenuKind {
    Project(String),
    Accounts,
}

struct Rename {
    ws: String,
    input: Entity<InputState>,
    _sub: Subscription,
}

struct Picker {
    ws: String,
    general: bool,
    query: Entity<InputState>,
    suggested: Vec<String>,
    _sub: Subscription,
}

/// What a menu item does.
#[derive(Clone)]
enum Act {
    NewChat(String),
    Preview(String),
    StartTeam(String),
    StopTeam(String),
    Rename(String),
    Icon(String),
    RunOn(String, Option<String>),
    Delete(String),
    UseAccount(String),
    Accounts,
    Usage,
    Mods,
    Settings,
}

/// The edge being dragged.
#[derive(Clone)]
struct SideResize;

impl Render for SideResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The theme's colors under the CSS names the Electron styles use.
#[derive(Clone, Copy)]
pub(super) struct Pal {
    pub tx: Hsla,
    pub mu: Hsla,
    pub fa: Hsla,
    pub line: Hsla,
    pub line2: Hsla,
    pub sel: Hsla,
    pub hover: Hsla,
    pub raise: Hsla,
    pub ok: Hsla,
    pub warn: Hsla,
    pub err: Hsla,
    pub acc: Hsla,
    pub panel: Hsla,
    pub side: Hsla,
    pub bg: Hsla,
    /// The accent fill of a primary button, its hover, and the text on it.
    pub acc_fill: Hsla,
    pub acc_fill_hover: Hsla,
    pub on_acc: Hsla,
}

pub(super) fn pal(cx: &App) -> Pal {
    let t = cx.theme();
    Pal {
        tx: t.foreground,
        mu: t.muted_foreground,
        fa: t.muted_foreground.opacity(0.72),
        line: t.border,
        line2: t.input,
        sel: t.list_active,
        hover: t.secondary,
        raise: t.muted,
        ok: t.success,
        warn: t.warning,
        err: t.danger,
        acc: t.ring,
        panel: t.popover,
        side: t.sidebar,
        bg: t.background,
        acc_fill: t.primary,
        acc_fill_hover: t.primary_hover,
        on_acc: t.primary_foreground,
    }
}

// ---- Icons ------------------------------------------------------------------------------

struct SvgIcon {
    body: String,
    width: f64,
    height: f64,
}

fn svg_icon(i: &serde_json::Value, width: f64, height: f64) -> Option<SvgIcon> {
    Some(SvgIcon {
        body: i["body"].as_str()?.into(),
        width: i["width"].as_f64().unwrap_or(width),
        height: i["height"].as_f64().unwrap_or(height),
    })
}

/// Iconify-style sets as `set:name` → icon.
fn parse_icons(json: &str) -> HashMap<String, SvgIcon> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let mut out = HashMap::new();
    for (set, s) in v.as_object().into_iter().flatten() {
        let w = s["width"].as_f64().unwrap_or(16.0);
        let h = s["height"].as_f64().unwrap_or(16.0);
        for (name, i) in s["icons"].as_object().into_iter().flatten() {
            if let Some(icon) = svg_icon(i, w, h) {
                out.insert(format!("{set}:{name}"), icon);
            }
        }
    }
    out
}

/// The bundled sets (#80): `dev` devicon logos, `ms` Material Symbols, `mit` Material Icon
/// Theme (MIT, its `folder-*` icons left out as the picker hides them).
fn project_icons() -> &'static HashMap<String, SvgIcon> {
    static ICONS: OnceLock<HashMap<String, SvgIcon>> = OnceLock::new();
    ICONS.get_or_init(|| parse_icons(include_str!("../../assets/project-icons.json")))
}

/// Offline provider marks (`provider-logos.json`, docs/PROVIDER-LOGOS.md).
fn provider_logos() -> &'static HashMap<String, SvgIcon> {
    static LOGOS: OnceLock<HashMap<String, SvgIcon>> = OnceLock::new();
    LOGOS.get_or_init(|| {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../assets/provider-logos.json"))
                .unwrap_or_default();
        v.as_object()
            .into_iter()
            .flatten()
            .filter_map(|(cli, logo)| Some((cli.clone(), svg_icon(logo, 16.0, 16.0)?)))
            .collect()
    })
}

/// `PROJECT_ICON_ID`: `mit|dev|ms` and a lower-case name.
fn valid_icon_id(id: &str) -> bool {
    let Some((set, name)) = id.split_once(':') else {
        return false;
    };
    matches!(set, "mit" | "dev" | "ms")
        && (1..=48).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The icon to draw for a saved id: unknown or empty ones are the folder.
fn icon_or_folder(id: &str) -> Option<&'static SvgIcon> {
    let icons = project_icons();
    valid_icon_id(id)
        .then(|| icons.get(id))
        .flatten()
        .or_else(|| icons.get(DEFAULT_ICON))
}

fn svg_of(icon: &SvgIcon, color: Hsla) -> String {
    let (c, _) = hex(color);
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="64" height="64" viewBox="0 0 {} {}">{}</svg>"#,
        icon.width,
        icon.height,
        icon.body.replace("currentColor", &c)
    )
}

/// A project's icon in its own colours (`ProjectIcon`); the folder when it has none.
fn project_icon(id: &str, size: f32, color: Hsla) -> AnyElement {
    match icon_or_folder(id) {
        Some(icon) => img(svg_image(svg_of(icon, color)))
            .size(z(size))
            .flex_none()
            .into_any_element(),
        None => div().size(z(size)).flex_none().into_any_element(),
    }
}

/// `iconLabel`: `mit:react-ts` → `react ts`.
fn icon_label(id: &str) -> String {
    id.split_once(':').map_or(id, |(_, n)| n).replace('-', " ")
}

/// A name's first letter in a quiet circle (`Initial`).
fn initial(name: &str, size: f32, p: &Pal) -> Div {
    let letter = name
        .trim()
        .chars()
        .next()
        .map_or("?".to_string(), |c| c.to_uppercase().to_string());
    div()
        .size(z(size))
        .flex_none()
        .rounded_full()
        .bg(p.raise)
        .border_1()
        .border_color(p.line)
        .flex()
        .items_center()
        .justify_center()
        .text_size(z((size * 0.48).round()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.mu)
        .child(letter)
}

/// A CLI's mark (`ProviderLogo`), else the name's letter.
pub(super) fn provider_logo(cli: &str, name: &str, size: f32, p: &Pal) -> AnyElement {
    match provider_logos().get(cli) {
        Some(logo) => img(svg_image(svg_of(logo, p.tx)))
            .size(z(size))
            .flex_none()
            .into_any_element(),
        None => initial(name, size, p).into_any_element(),
    }
}

/// The letter with the CLI's mark on its corner (`AccountAvatar`).
fn avatar(cli: &str, name: &str, size: f32, p: &Pal) -> Div {
    div()
        .relative()
        .flex_none()
        .child(initial(name, size, p))
        .child(
            div()
                .absolute()
                .right(z(-3.))
                .bottom(z(-3.))
                .child(provider_logo(cli, name, 12., p)),
        )
}

// ---- Shortcut hints ---------------------------------------------------------------------

/// `chord`: `Ctrl+Shift+N`, or `⌘⇧N` on a Mac.
fn chord(parts: &[&str]) -> String {
    let mac = cfg!(target_os = "macos");
    let parts: Vec<&str> = parts
        .iter()
        .map(|k| match (*k, mac) {
            ("mod", true) => "⌘",
            ("shift", true) => "⇧",
            ("alt", true) => "⌥",
            ("mod", false) => "Ctrl",
            ("shift", false) => "Shift",
            ("alt", false) => "Alt",
            (k, _) => k,
        })
        .collect();
    parts.join(if mac { "" } else { "+" })
}

/// `bindingHint`: a rebindable shortcut as its keys.
pub(super) fn binding_hint(b: &Binding) -> String {
    let key = match b.code.as_str() {
        "Slash" => "/".to_string(),
        code => code
            .trim_start_matches("Key")
            .trim_start_matches("Digit")
            .to_string(),
    };
    let mut parts = vec!["mod"];
    if b.shift {
        parts.push("shift");
    }
    if b.alt {
        parts.push("alt");
    }
    parts.push(&key);
    chord(&parts)
}

// ---- Small pure helpers -----------------------------------------------------------------

/// `ago`: `now`, `5m`, `3h`, `2d`, `4w`.
fn ago(then: i64, now: i64) -> String {
    let s = ((now - then) / 1000).max(0);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=604_799 => format!("{}d", s / 86_400),
        _ => format!("{}w", s / 604_800),
    }
}

/// When a window resets: the time today, else the weekday (`13:40`, `Mon`).
fn reset_label(at: i64, now: i64) -> String {
    use chrono::TimeZone;
    let Some(t) = chrono::Local.timestamp_millis_opt(at).single() else {
        return String::new();
    };
    if at - now < 20 * 3_600_000 {
        t.format("%H:%M").to_string()
    } else {
        t.format("%a").to_string()
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A project's chat count, else its History, else none (`WorkspaceRow`).
fn chat_count(w: &Workspace) -> String {
    match (w.panes.len(), w.history.len()) {
        (0, 0) => "No chats yet".into(),
        (0, h) => format!("{h} in history"),
        (n, _) => plural(n, "chat", "chats"),
    }
}

/// The last part of a folder path.
fn folder_name(cwd: &str) -> String {
    cwd.split(['/', '\\'])
        .rfind(|s| !s.is_empty())
        .unwrap_or(cwd)
        .to_string()
}

const MARKER_FILES: [&str; 14] = [
    "composer.json",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "package.json",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "pubspec.yaml",
    "Gemfile",
    "Dockerfile",
    "compose.yaml",
    "docker-compose.yml",
];

/// package.json dependencies → icon, first match wins (frameworks before their base).
const NODE_DEPS: [(&str, &str); 10] = [
    ("next", "dev:nextjs"),
    ("nuxt", "mit:nuxt"),
    ("@angular/core", "mit:angular"),
    ("svelte", "mit:svelte"),
    ("astro", "mit:astro"),
    ("electron", "dev:electron"),
    ("@tauri-apps/api", "mit:tauri"),
    ("vue", "mit:vue"),
    ("react", "mit:react"),
    ("vite", "mit:vite"),
];

/// Icons that fit a project, best first, from its marker files' text (`suggestProjectIcons`;
/// a file too big to read is present with no text).
fn suggest_icons(files: &HashMap<&str, String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |id: &str| {
        if !out.iter().any(|o| o == id) {
            out.push(id.into());
        }
    };
    let has = |f: &str| files.contains_key(f);
    let text = |f: &str| files.get(f).map_or("", String::as_str);
    if has("composer.json") {
        add(if text("composer.json").contains("laravel/framework") {
            "mit:laravel"
        } else {
            "mit:php"
        });
    }
    let jvm = format!(
        "{}{}{}",
        text("pom.xml"),
        text("build.gradle"),
        text("build.gradle.kts")
    );
    if jvm.contains("spring-boot") || jvm.contains("org.springframework") {
        add("dev:spring");
    }
    if text("build.gradle.kts").contains("kotlin") || text("build.gradle").contains("kotlin") {
        add("mit:kotlin");
    }
    if has("pom.xml") || has("build.gradle") || has("build.gradle.kts") {
        add("mit:java");
    }
    if has("package.json") {
        let pkg: serde_json::Value = serde_json::from_str(text("package.json")).unwrap_or_default();
        let dep = |name: &str| {
            ["devDependencies", "dependencies"]
                .iter()
                .any(|k| pkg[k].as_object().is_some_and(|o| o.contains_key(name)))
        };
        for (name, id) in NODE_DEPS {
            if dep(name) {
                add(id);
            }
        }
        add(if dep("typescript") {
            "mit:typescript"
        } else {
            "mit:nodejs"
        });
    }
    if has("Cargo.toml") {
        add("mit:rust");
    }
    if has("go.mod") {
        add("mit:go");
    }
    let py = format!("{}{}", text("pyproject.toml"), text("requirements.txt")).to_lowercase();
    if py.contains("django") {
        add("mit:django");
    }
    if has("pyproject.toml") || has("requirements.txt") {
        add("mit:python");
    }
    if has("pubspec.yaml") {
        add("dev:flutter");
    }
    if text("Gemfile").contains("rails") {
        add("mit:ruby");
    }
    if has("Dockerfile") || has("compose.yaml") || has("docker-compose.yml") {
        add("mit:docker");
    }
    out
}

/// The project folder's top-level marker files, small ones read (main's `projectSuggestIcons`).
fn read_markers(dir: &Path) -> HashMap<&'static str, String> {
    MARKER_FILES
        .iter()
        .filter_map(|f| {
            let p = dir.join(f);
            let meta = std::fs::metadata(&p).ok().filter(|m| m.is_file())?;
            let text = if meta.len() <= 256 * 1024 {
                std::fs::read_to_string(&p).unwrap_or_default()
            } else {
                String::new()
            };
            Some((*f, text))
        })
        .collect()
}

/// The picker's icons for a tab, filtered (`iconNames`, sorted by name).
fn picker_icons(general: bool, query: &str) -> Vec<String> {
    let q = query
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    let mut ids: Vec<&String> = project_icons()
        .keys()
        .filter(|id| {
            if general {
                id.starts_with("ms:")
            } else {
                id.starts_with("dev:") || id.starts_with("mit:")
            }
        })
        .collect();
    ids.sort_by(|a, b| {
        let n = |s: &str| s.split_once(':').map_or("", |(_, n)| n).to_string();
        n(a).cmp(&n(b)).then(a.cmp(b))
    });
    ids.into_iter()
        .filter(|id| q.is_empty() || id.split_once(':').is_some_and(|(_, n)| n.contains(&q)))
        .take(PICKER_MAX)
        .cloned()
        .collect()
}

// ---- The sidebar ------------------------------------------------------------------------

impl Shell {
    /// `view` in the Electron store: what the main area shows.
    pub(super) fn side_view(&self) -> &'static str {
        if self.settings_view.is_some() {
            "settings"
        } else if self.accounts_view.is_some() {
            "accounts"
        } else if self.usage_view.is_some() {
            "usage"
        } else if self.mods_view.is_some() {
            "mods"
        } else if self.scheduled_view.is_some() {
            "scheduled"
        } else if self.project_view.is_some() {
            "project"
        } else {
            "grid"
        }
    }

    /// Back to the panes (`setView('grid')`).
    pub(super) fn side_show_grid(&mut self) {
        self.settings_view = None;
        self.accounts_view = None;
        self.usage_view = None;
        self.mods_view = None;
        self.scheduled_view = None;
        self.project_view = None;
    }

    /// Per frame: who was working, the focused chat's project opening its dropdown, and the
    /// drawer closing once you go somewhere.
    fn side_sync(&mut self, cx: &mut Context<Self>) {
        let now = wraithgrid_usage::now_ms();
        let busy: Vec<String> = self
            .config()
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .filter(|p| {
                matches!(
                    self.pane_status(&p.id),
                    Some("running" | "background" | "approval")
                )
            })
            .map(|p| p.id.clone())
            .collect();
        for id in busy {
            self.sidebar.active_at.insert(id, now);
        }
        let focus_ws = self.focused.as_ref().and_then(|f| {
            self.config()
                .workspaces
                .iter()
                .find(|w| w.panes.iter().any(|p| &p.id == f))
                .map(|w| w.id.clone())
        });
        if focus_ws != self.sidebar.focus_ws {
            self.sidebar.focus_ws = focus_ws.clone();
            if let Some(ws) = focus_ws {
                if !self.config().settings.sidebar_open_projects.contains(&ws) {
                    self.update(cx, |c| c.settings.sidebar_open_projects.push(ws));
                }
            }
        }
        let place = format!("{}:{}", self.side_view(), self.config().active_workspace);
        if place != self.sidebar.place {
            self.sidebar.place = place;
            self.sidebar.drawer = false;
        }
    }

    fn toggle_sidebar(&mut self, narrow: bool, cx: &mut Context<Self>) {
        if narrow {
            self.sidebar.drawer = !self.sidebar.drawer;
            cx.notify();
        } else {
            self.update(cx, |c| {
                c.settings.sidebar_collapsed = !c.settings.sidebar_collapsed
            });
        }
    }

    /// Opens or closes a project's chat list, saved in `sidebarOpenProjects`.
    fn set_project_open(&mut self, id: &str, open: bool, cx: &mut Context<Self>) {
        let ids: HashSet<String> = self
            .config()
            .workspaces
            .iter()
            .map(|w| w.id.clone())
            .collect();
        let mut kept: Vec<String> = self
            .config()
            .settings
            .sidebar_open_projects
            .iter()
            .filter(|x| *x != id && ids.contains(*x))
            .cloned()
            .collect();
        if open {
            kept.push(id.into());
        }
        self.update(cx, |c| c.settings.sidebar_open_projects = kept);
    }

    fn side_new_chat(&mut self, ws: String, window: &mut Window, cx: &mut Context<Self>) {
        self.select_workspace(ws, cx);
        self.open_new_pane(window, cx);
    }

    /// The sidebar, its resize edge, the narrow window's drawer and its menus and dialogs.
    pub(super) fn render_sidebar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.side_sync(cx);
        let p = pal(cx);
        let zoom = self.config().settings.ui_zoom as f32;
        let viewport = window.viewport_size();
        let narrow = f32::from(viewport.width) / zoom < NARROW_PX;
        if !narrow {
            self.sidebar.drawer = false;
        }
        let collapsed = narrow || self.config().settings.sidebar_collapsed;
        let width = self
            .sidebar
            .drag_width
            .unwrap_or(self.config().settings.sidebar_width as f32);
        let panel = self.side_panel(collapsed, width, narrow, window, cx);
        let drawer = (narrow && self.sidebar.drawer)
            .then(|| self.side_panel(false, width, narrow, window, cx));
        let menu = self.side_menu_layer(viewport, window, cx);
        let picker = self.icon_picker_layer(viewport, cx);
        let dragging = self.sidebar.drag_width.is_some();
        let top = self.frame_top(window);
        div()
            .id("sidebar")
            .relative()
            .h_full()
            .flex_shrink_0()
            .on_drag_move(
                cx.listener(move |this, e: &DragMoveEvent<SideResize>, _, cx| {
                    let w = f32::from(e.event.position.x - e.bounds.left()) / zoom;
                    this.sidebar.drag_width = Some(
                        w.clamp(SIDEBAR_WIDTH_MIN as f32, SIDEBAR_WIDTH_MAX as f32)
                            .round(),
                    );
                    cx.notify();
                }),
            )
            .child(panel)
            .when(!collapsed, |d| {
                let commit = |this: &mut Shell, cx: &mut Context<Shell>| {
                    if let Some(w) = this.sidebar.drag_width.take() {
                        this.update(cx, |c| c.settings.sidebar_width = w as i64);
                    }
                };
                d.child(
                    div()
                        .id("side-resize")
                        .group("side-resize")
                        // The gap between the sidebar and main cards is the handle (#88).
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(z(-super::PANEL_GAP))
                        .w(z(super::PANEL_GAP))
                        .cursor(CursorStyle::ResizeLeftRight)
                        .tooltip(|window, cx| {
                            Tooltip::new("Drag to resize, double-click to reset").build(window, cx)
                        })
                        .child(
                            div()
                                .absolute()
                                .top(px(18.))
                                .bottom(px(18.))
                                .left(z(super::PANEL_GAP / 2. - 1.))
                                .w(px(2.))
                                .rounded(px(1.))
                                .bg(p.acc)
                                .opacity(if dragging { 0.6 } else { 0. })
                                .group_hover("side-resize", |s| s.opacity(0.6)),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                if e.click_count == 2 {
                                    this.sidebar.drag_width = None;
                                    this.update(cx, |c| {
                                        c.settings.sidebar_width = SIDEBAR_WIDTH_DEFAULT
                                    });
                                }
                            }),
                        )
                        .on_drag(SideResize, |r, _, _, cx| cx.new(|_| r.clone()))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| commit(this, cx)),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| commit(this, cx)),
                        ),
                )
            })
            .when_some(drawer, |d, drawer| {
                d.child(
                    deferred(
                        anchored().position(point(px(0.), px(0.))).child(
                            div()
                                .id("side-drawer-scrim")
                                .occlude()
                                .w(viewport.width)
                                .h(viewport.height)
                                .bg(hsla(0., 0., 0., 0.3))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.sidebar.drawer = false;
                                        cx.notify();
                                    }),
                                )
                                .child(
                                    div()
                                        .id("side-drawer")
                                        .occlude()
                                        .absolute()
                                        .left_0()
                                        .top(px(top))
                                        .h(viewport.height - px(top))
                                        .shadow_lg()
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .child(drawer),
                                ),
                        ),
                    )
                    .with_priority(1),
                )
            })
            .children(menu)
            .children(picker)
            .into_any_element()
    }

    /// The sidebar itself: full, or the icon rail.
    fn side_panel(
        &mut self,
        collapsed: bool,
        width: f32,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = pal(cx);
        let toggle_label = if collapsed {
            "Expand sidebar"
        } else {
            "Collapse sidebar"
        };
        let header = div()
            .h(z(44.))
            .flex_none()
            .flex()
            .items_center()
            .when(collapsed, |d| d.justify_center())
            .when(!collapsed, |d| {
                d.pl(z(14.)).pr(z(8.)).child(
                    div()
                        .flex_1()
                        .text_size(z(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Wraithgrid"),
                )
            })
            .child(
                icon_btn(
                    "side-toggle",
                    Glyph::Sidebar,
                    toggle_label.into(),
                    false,
                    &p,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_sidebar(narrow, cx))),
            );
        let base = div()
            .h_full()
            .w(z(if collapsed { RAIL_W } else { width }))
            .flex()
            .flex_col()
            .bg(p.side)
            .rounded(z(super::PANEL_R))
            .overflow_hidden()
            .text_color(p.tx)
            .text_size(z(13.))
            .child(header);
        if collapsed {
            return base.child(self.side_rail(cx)).child(
                div()
                    .flex_none()
                    .flex()
                    .justify_center()
                    .py(z(10.))
                    .border_t_1()
                    .border_color(p.line)
                    .child(self.account_switcher(true, width, window, cx)),
            );
        }
        base.child(self.side_nav(cx))
            .child(self.side_usage(cx))
            .child(self.side_projects(cx))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(z(6.))
                    .p(z(8.))
                    .border_t_1()
                    .border_color(p.line)
                    .child(self.account_switcher(false, width, window, cx))
                    .child(self.side_meta(cx)),
            )
    }

    pub(super) fn hints(&self) -> (String, String) {
        let b = bindings(&self.config().settings.shortcuts);
        let hint = |id: ShortcutId| {
            b.iter()
                .find(|(i, _)| *i == id)
                .map(|(_, b)| binding_hint(b))
                .unwrap_or_default()
        };
        (hint(ShortcutId::NewPane), hint(ShortcutId::Palette))
    }

    /// New chat, Search and Scheduled (`.side-nav`).
    fn side_nav(&mut self, cx: &mut Context<Self>) -> Div {
        let p = pal(cx);
        let (new_hint, search_hint) = self.hints();
        let scheduled = self.side_view() == "scheduled";
        let row = |id: &'static str,
                   g: Glyph,
                   label: &'static str,
                   kbd: Option<String>,
                   tip: String,
                   on: bool| {
            let group = SharedString::from(id);
            div()
                .id(id)
                .a11y_toggle(label, on)
                .group(group.clone())
                .h(z(34.))
                .px(z(8.))
                .flex()
                .items_center()
                .gap(z(10.))
                .rounded(z(8.))
                .cursor_pointer()
                .when(on, |d| d.bg(p.sel))
                .when(!on, |d| d.hover(|s| s.bg(p.hover)))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(glyph(g, 16., p.mu, p.side))
                .child(div().flex_1().child(label))
                .when_some(kbd, |d, k| {
                    d.child(
                        kbd_el(k, &p, cx)
                            .opacity(0.)
                            .group_hover(group, |s| s.opacity(1.)),
                    )
                })
        };
        div()
            .px(z(10.))
            .pt(z(2.))
            .pb(z(6.))
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(
                row(
                    "side-new-chat",
                    Glyph::Compose,
                    "New chat",
                    Some(new_hint.clone()),
                    format!("New chat ({new_hint})"),
                    false,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_new_pane(window, cx))),
            )
            .child(
                row(
                    "side-search",
                    Glyph::Search,
                    "Search",
                    Some(search_hint.clone()),
                    format!("Search ({search_hint})"),
                    false,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx))),
            )
            .child(
                row(
                    "side-scheduled",
                    Glyph::Clock,
                    "Scheduled",
                    None,
                    "Scheduled chats".into(),
                    scheduled,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_scheduled(window, cx))),
            )
    }

    /// The Usage block (`SidebarUsage`): who new chats use, a line per window with its reset,
    /// and where auto-switch (#61) would go. Opens the Usage screen.
    fn side_usage(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = pal(cx);
        let c = self.config();
        let on = self.side_view() == "usage";
        let account = c
            .settings
            .default_account_id
            .as_ref()
            .and_then(|id| c.account(id))
            .or(c.accounts.first())
            .cloned();
        let now = wraithgrid_usage::now_ms();
        let lim = account
            .as_ref()
            .and_then(|a| self.limits.get(&a.id))
            .copied();
        let five = wraithgrid_usage::model::live_window(lim.and_then(|l| l.five_hour), now);
        let week = wraithgrid_usage::model::live_window(lim.and_then(|l| l.seven_day), now);
        let at = c.settings.auto_switch_at;
        let next = account
            .as_ref()
            .filter(|_| c.settings.auto_switch_mode != AutoSwitchMode::Off)
            .and_then(|a| {
                let accounts = self.usage_accounts();
                let from = accounts.iter().find(|u| u.id == a.id)?.clone();
                wraithgrid_usage::auto_switch::next_account(&accounts, &from, &self.limits, now, at)
                    .map(|n| n.name.clone())
            });
        // `sessionLimitLabel` for the account new chats use.
        let named = c
            .accounts
            .iter()
            .filter(|a| a.cli.spec().reports_limits)
            .count()
            > 1;
        let tip = match (&account, five) {
            (Some(a), Some(w)) => {
                let who = if named {
                    format!("{}: ", a.name)
                } else {
                    String::new()
                };
                let whose = if named {
                    " (the account new chats use)"
                } else {
                    ""
                };
                format!(
                    "Usage: {who}{}% of the current session limit{whose}",
                    (w.utilization * 100.0).round() as i64
                )
            }
            (Some(a), None) if named => format!(
                "Usage: no limits from {} yet (they show after its first chat turn)",
                a.name
            ),
            _ => "Usage".into(),
        };
        let mono = crate::terminal::mono(cx);
        let line = |name: &'static str, w: wraithgrid_usage::model::UsageWindow| {
            let pct = (w.utilization * 100.0).round() as i64;
            let (bar, text) = if pct >= 95 {
                (p.err, p.err)
            } else if pct >= 80 {
                (p.warn, p.warn)
            } else {
                (p.mu, p.tx)
            };
            div()
                .h(z(20.))
                .flex()
                .items_center()
                .gap(z(8.))
                .font_family(mono.clone())
                .text_size(z(11.))
                .text_color(p.fa)
                .child(div().w(z(34.)).flex_none().child(name))
                .child(
                    div()
                        .flex_1()
                        .relative()
                        .h(z(3.))
                        .rounded(z(2.))
                        .bg(p.line2)
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left_0()
                                .w(relative(pct.clamp(0, 100) as f32 / 100.0))
                                .rounded(z(2.))
                                .bg(bar),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(z(-2.))
                                .left(relative(at.clamp(0, 100) as f32 / 100.0))
                                .w(px(1.))
                                .h(z(7.))
                                .bg(p.fa)
                                .opacity(0.6),
                        ),
                )
                .child(
                    div()
                        .w(z(34.))
                        .flex_none()
                        .text_right()
                        .text_color(text)
                        .child(format!("{pct}%")),
                )
                .child(
                    div()
                        .w(z(46.))
                        .flex_none()
                        .text_right()
                        .child(reset_label(w.resets_at, now)),
                )
        };
        let foot = |d: Div| d.mt(z(6.)).text_size(z(11.)).text_color(p.fa);
        div()
            .id("side-usage")
            .group("side-usage")
            .a11y_button("Usage")
            .flex_none()
            .flex()
            .flex_col()
            .px(z(16.))
            .pt(z(10.))
            .pb(z(12.))
            .border_b_1()
            .border_color(p.line)
            .text_color(p.mu)
            .cursor_pointer()
            .when(on, |d| d.bg(p.hover))
            .hover(|s| s.bg(p.hover))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| this.open_usage(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(z(6.))
                    .mb(z(8.))
                    .text_size(z(12.))
                    .child(match &account {
                        Some(a) => provider_logo(a.cli.as_str(), &a.name, 12., &p),
                        None => glyph(Glyph::Gauge, 14., p.mu, p.side).into_any_element(),
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(account.as_ref().map_or("Usage".into(), |a| a.name.clone())),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .opacity(0.)
                            .group_hover("side-usage", |s| s.opacity(1.))
                            .child(glyph(Glyph::ChevronRight, 14., p.fa, p.side)),
                    ),
            )
            .when_some(five, |d, w| d.child(line("5h", w)))
            .when_some(week, |d, w| d.child(line("week", w)))
            .when(five.is_none() && week.is_none(), |d| {
                d.child(foot(div()).child("no limits yet"))
            })
            .when_some(
                next.filter(|_| five.is_some() || week.is_some()),
                |d, name| {
                    d.child(
                        foot(div())
                            .flex()
                            .gap(z(4.))
                            .child(format!("at {at}% switches to"))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.mu)
                                    .child(name),
                            ),
                    )
                },
            )
    }

    /// The Projects list: the only part that scrolls (#67).
    fn side_projects(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = pal(cx);
        let workspaces = self.config().workspaces.clone();
        let rows: Vec<AnyElement> = workspaces
            .iter()
            .enumerate()
            .map(|(i, w)| self.project_row(i, w, cx))
            .collect();
        div()
            .id("side-list")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(z(8.))
            .py(z(10.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(z(8.))
                    .pb(z(4.))
                    .text_size(z(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.fa)
                    .child("Projects")
                    .child(
                        div()
                            .id("side-new-project")
                            .a11y_button("New project")
                            .px(z(4.))
                            .py(z(2.))
                            .rounded(z(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(p.raise))
                            .tooltip(|window, cx| Tooltip::new("New project").build(window, cx))
                            .child(glyph(Glyph::Plus, 14., p.fa, p.side))
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.open_new_project(window, cx)
                                }),
                            ),
                    ),
            )
            .children(rows)
    }

    /// One project (`WorkspaceRow`): chevron, icon, name, machine, count or state; click opens
    /// its page, right-click its menu, + a new chat in it.
    fn project_row(&mut self, i: usize, w: &Workspace, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let c = self.config();
        let id = w.id.clone();
        let open = c.settings.sidebar_open_projects.contains(&w.id);
        let on = c.active_workspace == w.id && self.side_view() == "project";
        let statuses: Vec<Option<&'static str>> =
            w.panes.iter().map(|p| self.pane_status(&p.id)).collect();
        let running = statuses.contains(&Some("running")) || statuses.contains(&Some("starting"));
        let background = statuses.contains(&Some("background"));
        let waiting = statuses.contains(&Some("approval")) || statuses.contains(&Some("login"));
        let busy = waiting || running || background;
        let state = if waiting {
            "needs you"
        } else if running {
            "working"
        } else {
            "in background"
        };
        let count = chat_count(w);
        let hint = if i < 9 {
            format!(" ({})", chord(&["mod", "shift", &(i + 1).to_string()]))
        } else {
            String::new()
        };
        let tip = [
            Some(w.name.clone()),
            Some(count),
            busy.then(|| state.to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
            + &hint;
        let machine = w
            .machine_id
            .as_ref()
            .and_then(|m| c.machines.iter().find(|x| &x.id == m))
            .map(|m| {
                (
                    m.name.clone(),
                    format!("Runs on {} ({})", m.name, m.ssh_host),
                )
            });
        let menu_open = self
            .sidebar
            .menu
            .as_ref()
            .is_some_and(|m| m.kind == MenuKind::Project(w.id.clone()));
        let empty = w.panes.is_empty();
        let group = SharedString::from(format!("ws-g-{}", w.id));
        let mono = crate::terminal::mono(cx);
        let renaming = self
            .sidebar
            .rename
            .as_ref()
            .filter(|r| r.ws == w.id)
            .map(|r| r.input.clone());

        let head = match renaming {
            Some(input) => div()
                .id(SharedString::from(format!("ws-rename-{}", w.id)))
                .h(z(30.))
                .pl(z(20.))
                .flex()
                .items_center()
                .rounded(z(8.))
                .bg(p.sel)
                .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                    if e.keystroke.key == "escape" {
                        this.sidebar.rename = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .child(div().flex_1().child(Input::new(&input).small()))
                .into_any_element(),
            None => {
                let (tg_id, row_id, add_id) = (id.clone(), id.clone(), id.clone());
                let name = w.name.clone();
                let tg_tip = if open {
                    format!("Hide chats in {}", w.name)
                } else {
                    format!("Show chats in {}", w.name)
                };
                let add_tip = format!("New chat in {}", w.name);
                div()
                    .id(SharedString::from(format!("ws-wrap-{}", w.id)))
                    .group(group.clone())
                    .relative()
                    .h(z(30.))
                    .pl(z(4.))
                    .flex()
                    .items_center()
                    .gap(z(6.))
                    .rounded(z(8.))
                    .when(on, |d| d.bg(p.sel))
                    .when(!on && menu_open, |d| d.bg(p.hover))
                    .when(!on, |d| d.hover(|s| s.bg(p.hover)))
                    .child(
                        div()
                            .id(SharedString::from(format!("ws-tg-{}", w.id)))
                            .a11y_button(tg_tip.clone())
                            .w(z(16.))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .when(empty, |d| d.opacity(0.35).hover(|s| s.opacity(1.)))
                            .tooltip(move |window, cx| {
                                Tooltip::new(tg_tip.clone()).build(window, cx)
                            })
                            .child(glyph(
                                if open {
                                    Glyph::ChevronDown
                                } else {
                                    Glyph::ChevronRight
                                },
                                14.,
                                p.fa,
                                p.side,
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_project_open(&tg_id, !open, cx)
                            })),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("ws-row-{}", w.id)))
                            .a11y_button(name.clone())
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .pr(z(6.))
                            .flex()
                            .items_center()
                            .gap(z(6.))
                            .cursor_pointer()
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                            .child(project_icon(&w.icon, 16., p.mu))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(name),
                            )
                            .when_some(machine, |d, (m, mtip)| {
                                d.child(
                                    div()
                                        .id(SharedString::from(format!("ws-mach-{}", w.id)))
                                        .flex_none()
                                        .max_w(relative(0.4))
                                        .truncate()
                                        .px(z(4.))
                                        .rounded(z(4.))
                                        .border_1()
                                        .border_color(p.line)
                                        .text_size(z(10.5))
                                        .text_color(p.mu)
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(mtip.clone()).build(window, cx)
                                        })
                                        .child(m),
                                )
                            })
                            .map(|d| {
                                let tag = div()
                                    .flex_none()
                                    .font_family(mono.clone())
                                    .text_size(z(11.))
                                    .group_hover(group.clone(), |s| s.opacity(0.));
                                // A closed project says it's working; approvals show on the chat (#75).
                                if !open && (running || background) {
                                    d.child(tag.text_color(p.ok).child("working"))
                                } else if !empty {
                                    d.child(tag.text_color(p.fa).child(w.panes.len().to_string()))
                                } else {
                                    d
                                }
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_project_page(row_id.clone(), window, cx)
                            }))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener({
                                    let id = id.clone();
                                    move |this, e: &MouseDownEvent, _, cx| {
                                        this.open_project_menu(id.clone(), e.position, false, cx)
                                    }
                                }),
                            )
                            // From the keyboard: ←/→ fold the chat list, Shift+F10 or the
                            // Menu key opens the row's menu under it.
                            .focusable()
                            .tab_index(0)
                            .focus_visible(|s| s.border_1().border_color(p.acc))
                            .on_key_down(cx.listener({
                                let (id, rows) = (id.clone(), self.sidebar.row_bounds.clone());
                                move |this, e: &KeyDownEvent, _, cx| {
                                    let k = &e.keystroke;
                                    match k.key.as_str() {
                                        "right" | "left" if !k.modifiers.modified() => {
                                            this.set_project_open(&id, k.key == "right", cx)
                                        }
                                        "f10" if k.modifiers.shift => {}
                                        "menu" | "contextmenu" => {}
                                        _ => return,
                                    }
                                    if k.key != "right" && k.key != "left" {
                                        let at = rows
                                            .borrow()
                                            .get(&id)
                                            .map_or(point(px(40.), px(120.)), |b| {
                                                b.bottom_left() + point(px(12.), px(0.))
                                            });
                                        this.open_project_menu(id.clone(), at, true, cx);
                                    }
                                    cx.stop_propagation();
                                }
                            }))
                            .child({
                                let (rows, key) = (self.sidebar.row_bounds.clone(), id.clone());
                                canvas(
                                    move |b, _, _| {
                                        rows.borrow_mut().insert(key, b);
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full()
                            }),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("ws-add-{}", w.id)))
                            .a11y_button(add_tip.clone())
                            .absolute()
                            .right(z(4.))
                            .top(z(4.))
                            .size(z(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(z(6.))
                            .cursor_pointer()
                            .opacity(0.)
                            .group_hover(group.clone(), |s| s.opacity(1.))
                            .hover(|s| s.bg(p.raise))
                            .tooltip(move |window, cx| {
                                Tooltip::new(add_tip.clone()).build(window, cx)
                            })
                            .child(glyph(Glyph::Plus, 14., p.fa, p.side))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.side_new_chat(add_id.clone(), window, cx)
                            })),
                    )
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .when(i > 0, |d| d.mt(px(1.)))
            .child(head)
            .when(open, |d| d.child(self.project_chats(w, cx)))
            .into_any_element()
    }

    /// A project's open chats, most recently active first, off a 1px guide (#87); an empty
    /// project offers "New chat".
    fn project_chats(&mut self, w: &Workspace, cx: &mut Context<Self>) -> Div {
        let p = pal(cx);
        let now = wraithgrid_usage::now_ms();
        let mut chats: Vec<(&Pane, i64)> = w
            .panes
            .iter()
            .map(|pane| (pane, *self.sidebar.active_at.get(&pane.id).unwrap_or(&-1)))
            .collect();
        chats.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
        let grid = self.side_view() == "grid";
        let rows: Vec<AnyElement> = chats
            .into_iter()
            .map(|(pane, at)| {
                let c = self.config();
                let account = pane.account_id.as_deref().and_then(|a| c.account(a));
                let status = self.pane_status(&pane.id);
                let on = grid && self.focused.as_deref() == Some(pane.id.as_str());
                let busy = matches!(status, Some("running" | "starting" | "background"));
                let loud = matches!(status, Some("approval" | "login"));
                let branch = self
                    .git_heads
                    .get(&pane.cwd)
                    .and_then(|g| g.branch.clone())
                    .unwrap_or_else(|| folder_name(&pane.cwd));
                let title = pane_label(c, pane);
                let tip = [
                    Some(title.clone()),
                    Some(branch),
                    account.map(|a| a.name.clone()),
                ]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
                let mono = crate::terminal::mono(cx);
                let state = |text: &'static str, color: Hsla| {
                    div()
                        .flex_none()
                        .font_family(mono.clone())
                        .text_size(z(11.))
                        .text_color(color)
                        .child(text)
                };
                let right = if loud {
                    let word = if status == Some("login") {
                        "sign in"
                    } else {
                        "approve"
                    };
                    Some(state(word, p.warn).into_any_element())
                } else if busy {
                    Some(
                        state(
                            if status == Some("background") {
                                "background"
                            } else {
                                "working"
                            },
                            p.ok,
                        )
                        .into_any_element(),
                    )
                } else if at >= 0 {
                    Some(
                        div()
                            .flex_none()
                            .text_size(z(11.5))
                            .text_color(p.fa)
                            .child(ago(at, now))
                            .into_any_element(),
                    )
                } else {
                    None
                };
                let (ws, id) = (w.id.clone(), pane.id.clone());
                div()
                    .id(SharedString::from(format!("side-chat-{}", pane.id)))
                    .a11y_toggle(title.clone(), on)
                    .h(z(28.))
                    .px(z(8.))
                    .flex()
                    .items_center()
                    .gap(z(8.))
                    .rounded(z(8.))
                    .text_size(z(12.5))
                    .text_color(if on { p.tx } else { p.mu })
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.sel.opacity(0.5)))
                    .hover(|s| s.bg(p.hover).text_color(p.tx))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(match account {
                        Some(a) => provider_logo(a.cli.as_str(), &a.name, 13., &p),
                        None => glyph(Glyph::Monitor, 14., p.mu, p.side).into_any_element(),
                    })
                    .child(div().flex_1().min_w(px(0.)).truncate().child(title))
                    .children(right)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reveal_chat(&ws, &id, window, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        let ws = w.id.clone();
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(1.))
            .ml(z(33.))
            .mb(z(4.))
            .pl(z(6.))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(z(2.))
                    .bottom(z(2.))
                    .w(px(1.))
                    .bg(p.line),
            )
            .children(rows)
            .when(empty, |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("side-new-{}", w.id)))
                        .a11y_button(format!("New chat in {}", w.name))
                        .h(z(28.))
                        .px(z(8.))
                        .flex()
                        .items_center()
                        .gap(z(8.))
                        .rounded(z(8.))
                        .text_size(z(12.5))
                        .text_color(p.fa)
                        .cursor_pointer()
                        .hover(|s| s.bg(p.hover).text_color(p.tx))
                        .child(glyph(Glyph::Plus, 14., p.fa, p.side))
                        .child(div().flex_1().child("New chat"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.side_new_chat(ws.clone(), window, cx)
                        })),
                )
            })
    }

    /// The collapsed sidebar: New chat, Search, Usage, then each project's icon (`RailProject`).
    fn side_rail(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = pal(cx);
        let (new_hint, search_hint) = self.hints();
        let usage = self.side_view() == "usage";
        let c = self.config().clone();
        let projects: Vec<AnyElement> = c
            .workspaces
            .iter()
            .map(|w| {
                let working = w.panes.iter().any(|x| {
                    matches!(
                        self.pane_status(&x.id),
                        Some("running" | "starting" | "background")
                    )
                });
                let on = c.active_workspace == w.id;
                let tip = if working {
                    format!("{} · working", w.name)
                } else {
                    w.name.clone()
                };
                let id = w.id.clone();
                div()
                    .id(SharedString::from(format!("rail-ws-{}", w.id)))
                    .a11y_button(w.name.clone())
                    .relative()
                    .size(z(34.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(z(9.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.sel))
                    .when(!on, |d| d.hover(|s| s.bg(p.hover)))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    // Working, the icon pops (#121) rather than carrying a dot.
                    .child(match icon_or_folder(&w.icon).filter(|_| working) {
                        Some(icon) => div()
                            .size(z(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                img(svg_image(svg_of(icon, p.mu)))
                                    .flex_none()
                                    .with_animation(
                                        SharedString::from(format!("rail-pop-{}", w.id)),
                                        Animation::new(std::time::Duration::from_millis(1600))
                                            .repeat()
                                            .with_easing(bounce(ease_in_out)),
                                        |i, t| i.size(z(20. + 4. * t)),
                                    ),
                            )
                            .into_any_element(),
                        None => project_icon(&w.icon, 20., p.mu),
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_project_page(id.clone(), window, cx)
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .id("side-rail")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .gap(z(4.))
            .pt(z(4.))
            .pb(z(8.))
            .child(
                icon_btn(
                    "rail-new",
                    Glyph::Compose,
                    format!("New chat ({new_hint})"),
                    false,
                    &p,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_new_pane(window, cx))),
            )
            .child(
                icon_btn(
                    "rail-search",
                    Glyph::Search,
                    format!("Search ({search_hint})"),
                    false,
                    &p,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx))),
            )
            .child(
                icon_btn("rail-usage", Glyph::Gauge, "Usage".into(), usage, &p)
                    .on_click(cx.listener(|this, _, window, cx| this.open_usage(window, cx))),
            )
            .child(div().w(z(20.)).h(px(1.)).my(z(6.)).bg(p.line))
            .children(projects)
    }

    /// The footer: who new chats run as; its menu switches it and leads to accounts.
    fn account_switcher(
        &mut self,
        compact: bool,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let p = pal(cx);
        let c = self.config();
        let current = c
            .settings
            .default_account_id
            .as_ref()
            .and_then(|id| c.account(id))
            .or(c.accounts.first())
            .cloned();
        let label = current
            .as_ref()
            .map_or("No account".to_string(), |a| a.name.clone());
        // Its plan when known ("Pro · new chats"), else how many accounts there are.
        let plan = current
            .as_ref()
            .and_then(|a| self.plans.get(&a.id))
            .cloned();
        let sub = if c.accounts.is_empty() {
            "Add one to start".to_string()
        } else if current.as_ref().is_some_and(|a| !a.signed_in) {
            "Login needed".to_string()
        } else {
            format!(
                "{} · new chats",
                plan.unwrap_or_else(|| plural(c.accounts.len(), "account", "accounts"))
            )
        };
        let open = self
            .sidebar
            .menu
            .as_ref()
            .is_some_and(|m| m.kind == MenuKind::Accounts);
        let menu = open.then(|| {
            let w = if compact {
                240.
            } else {
                (width - 16.).max(240.)
            };
            let items = self.account_items(&p);
            self.menu_list("Accounts", items, Some(w), window, cx)
        });
        let tip = format!("New chats use {label}");
        div()
            .id(if compact { "acct-sw-rail" } else { "acct-sw" })
            .a11y_button(tip.clone())
            .relative()
            .flex()
            .items_center()
            .gap(z(10.))
            .when(!compact, |d| d.w_full().px(z(8.)).py(z(6.)))
            .when(compact, |d| d.p(z(4.)))
            .rounded(z(10.))
            .cursor_pointer()
            .when(open, |d| d.bg(p.hover))
            .hover(|s| s.bg(p.hover))
            .when(compact, |d| {
                d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if open {
                    this.close_menus(cx);
                    return;
                }
                this.close_menus(cx);
                this.sidebar.menu = Some(Menu {
                    kind: MenuKind::Accounts,
                    at: point(px(0.), px(0.)),
                    confirm: false,
                });
                this.menu_opened(false);
                cx.notify();
            }))
            .child(match &current {
                Some(a) => avatar(a.cli.as_str(), &a.name, 26., &p).into_any_element(),
                None => div()
                    .size(z(28.))
                    .flex_none()
                    .rounded_full()
                    .border_1()
                    .border_dashed()
                    .border_color(p.line2)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(glyph(Glyph::UserPlus, 14., p.fa, p.side))
                    .into_any_element(),
            })
            .when(!compact, |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(z(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(label),
                        )
                        .child(div().text_size(z(11.5)).text_color(p.fa).child(sub)),
                )
                .child(glyph(
                    if open {
                        Glyph::ChevronDown
                    } else {
                        Glyph::ChevronUp
                    },
                    14.,
                    p.fa,
                    p.side,
                ))
            })
            // Opens upwards from the switcher, as wide as it (`ContextMenu above`).
            .when_some(menu, |d, menu| {
                d.child(
                    div().absolute().top_0().left_0().child(
                        deferred(
                            anchored()
                                .anchor(Anchor::BottomLeft)
                                .offset(point(px(0.), px(-6.)))
                                .snap_to_window_with_margin(px(4.))
                                .child(menu),
                        )
                        .with_priority(3),
                    ),
                )
            })
    }

    /// Pane and account counts of the active project, and the version (`SidebarMeta`).
    fn side_meta(&mut self, cx: &mut Context<Self>) -> Div {
        let p = pal(cx);
        let ws = self.config().active();
        let panes = ws.panes.len();
        let accounts = ws
            .panes
            .iter()
            .filter_map(|x| x.account_id.as_ref())
            .collect::<HashSet<_>>()
            .len();
        div()
            .flex()
            .items_center()
            .gap(z(12.))
            .px(z(8.))
            .pt(z(6.))
            .pb(z(2.))
            .font_family(crate::terminal::mono(cx))
            .text_size(z(10.5))
            .text_color(p.fa)
            .child(div().flex_none().child(plural(panes, "pane", "panes")))
            .child(
                div()
                    .flex_none()
                    .child(plural(accounts, "account", "accounts")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_right()
                    .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
            )
    }

    // ---- Menus --------------------------------------------------------------------------

    fn project_items(&self, ws: &str, confirm: bool, p: &Pal) -> Vec<MenuItem<Act>> {
        let c = self.config();
        let Some(w) = c.workspaces.iter().find(|w| w.id == ws) else {
            return vec![];
        };
        let panes = w.panes.len();
        let can_delete = c.workspaces.len() > 1;
        let g = |g: Glyph| glyph(g, 14., p.mu, p.panel);
        let mut items =
            vec![MenuItem::new("New chat", Act::NewChat(ws.into())).icon(g(Glyph::Compose))];
        if !w.team.is_empty() {
            items.push(if team_pane_ids(&w.team, &w.panes).is_empty() {
                MenuItem::new("Start team", Act::StartTeam(ws.into())).icon(g(Glyph::Play))
            } else {
                MenuItem::new("Stop team", Act::StopTeam(ws.into())).icon(g(Glyph::Stop))
            });
        }
        // A web preview runs on this computer only (H5).
        if w.machine_id.is_none() {
            items.push(MenuItem::new("Preview", Act::Preview(ws.into())).icon(g(Glyph::Monitor)));
        }
        items.push(MenuItem::new("Rename", Act::Rename(ws.into())).icon(g(Glyph::Rename)));
        items.push(
            MenuItem::new("Change icon", Act::Icon(ws.into()))
                .icon(project_icon(&w.icon, 14., p.mu)),
        );
        // Where the panes run can change only while there are none.
        if !c.machines.is_empty() {
            let targets = std::iter::once((None, "this computer".to_string()))
                .chain(
                    c.machines
                        .iter()
                        .map(|m| (Some(m.id.clone()), m.name.clone())),
                )
                .filter(|(id, _)| *id != w.machine_id);
            for (id, name) in targets {
                let label = if panes > 0 {
                    format!("Run on {name} (close its panes first)")
                } else {
                    format!("Run on {name}")
                };
                items.push(MenuItem {
                    disabled: panes > 0,
                    ..MenuItem::new(label, Act::RunOn(ws.into(), id)).icon(g(Glyph::Monitor))
                });
            }
        }
        let label = if !can_delete {
            "Delete (last project)".to_string()
        } else if confirm {
            if panes > 0 {
                format!("Click again: closes {}", plural(panes, "pane", "panes"))
            } else {
                "Click again to delete".into()
            }
        } else {
            "Delete project".into()
        };
        let trash = glyph(
            Glyph::Trash,
            14.,
            if can_delete { p.err } else { p.mu },
            p.panel,
        );
        items.push(MenuItem {
            danger: true,
            disabled: !can_delete,
            ..MenuItem::new(label, Act::Delete(ws.into())).icon(trash)
        });
        items
    }

    fn account_items(&self, p: &Pal) -> Vec<MenuItem<Act>> {
        let c = self.config();
        let current = c
            .settings
            .default_account_id
            .as_ref()
            .and_then(|id| c.account(id))
            .or(c.accounts.first())
            .map(|a| a.id.clone());
        let mut in_use: HashMap<&str, usize> = HashMap::new();
        for pane in c.workspaces.iter().flat_map(|w| &w.panes) {
            if let Some(a) = pane.account_id.as_deref().filter(|_| !pane.shell) {
                *in_use.entry(a).or_default() += 1;
            }
        }
        let g = |g: Glyph| glyph(g, 14., p.mu, p.panel);
        let mut items: Vec<MenuItem<Act>> = c
            .accounts
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let open = in_use.get(a.id.as_str()).copied().unwrap_or(0);
                // Signed in, its plan when known (`accountInfo`), and its open panes (#49).
                let state = if a.signed_in {
                    self.plans
                        .get(&a.id)
                        .cloned()
                        .unwrap_or_else(|| "Signed in".into())
                } else {
                    "Login needed".into()
                };
                let sub = [
                    Some(state),
                    (open > 0).then(|| format!("{} open", plural(open, "pane", "panes"))),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                MenuItem {
                    sub_warn: !a.signed_in,
                    checked: current.as_deref() == Some(a.id.as_str()),
                    heading: (i == 0).then_some("New chats use"),
                    ..MenuItem::new(a.name.clone(), Act::UseAccount(a.id.clone()))
                        .sub(sub)
                        .icon(avatar(a.cli.as_str(), &a.name, 26., p))
                }
            })
            .collect();
        items.push(MenuItem::new("New account", Act::Accounts).icon(g(Glyph::UserPlus)));
        items.push(MenuItem {
            divider: true,
            ..MenuItem::new("Usage", Act::Usage).icon(g(Glyph::Gauge))
        });
        items.push(MenuItem::new("Manage accounts", Act::Accounts).icon(g(Glyph::Users)));
        items.push(MenuItem::new("Mods", Act::Mods).icon(g(Glyph::Mods)));
        items.push(MenuItem::new("Settings", Act::Settings).icon(g(Glyph::Settings)));
        items
    }

    /// A sidebar menu (`ContextMenu`), run by `run_side_menu`.
    fn menu_list(
        &mut self,
        label: &'static str,
        items: Vec<MenuItem<Act>>,
        width: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.menu_card(
            "side-menu",
            label,
            items,
            width,
            |this, act, window, cx| this.run_side_menu(act, window, cx),
            window,
            cx,
        )
    }

    /// Opens a project's menu at a point (right-click, or Shift+F10 on its row).
    fn open_project_menu(
        &mut self,
        ws: String,
        at: Point<Pixels>,
        keys: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_menus(cx);
        self.sidebar.menu = Some(Menu {
            kind: MenuKind::Project(ws),
            at,
            confirm: false,
        });
        self.menu_opened(keys);
        cx.notify();
    }

    /// A scrim that closes the open menu, and a project's menu at the pointer.
    fn side_menu_layer(
        &mut self,
        viewport: Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(menu) = &self.sidebar.menu else {
            return vec![];
        };
        let project = match &menu.kind {
            MenuKind::Project(ws) => Some((menu.at, ws.clone(), menu.confirm)),
            MenuKind::Accounts => None,
        };
        let mut out = vec![deferred(
            anchored().position(point(px(0.), px(0.))).child(
                div()
                    .id("side-menu-scrim")
                    .occlude()
                    .w(viewport.width)
                    .h(viewport.height)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.close_menus(cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _, _, cx| {
                            this.close_menus(cx);
                        }),
                    ),
            ),
        )
        .with_priority(2)
        .into_any_element()];
        if let Some((at, ws, confirm)) = project {
            let p = pal(cx);
            let items = self.project_items(&ws, confirm, &p);
            let card = self.menu_list("Project", items, None, window, cx);
            out.push(
                deferred(
                    anchored()
                        .position(at)
                        .snap_to_window_with_margin(px(4.))
                        .child(card),
                )
                .with_priority(3)
                .into_any_element(),
            );
        }
        out
    }

    /// Closes the sidebar's own menu; true when one was open.
    pub(super) fn close_side_menu(&mut self) -> bool {
        self.sidebar.menu.take().is_some()
    }

    /// A sidebar menu or the icon picker is open (drawn over the grid).
    pub(super) fn sidebar_popup_open(&self) -> bool {
        self.sidebar.menu.is_some() || self.sidebar.picker.is_some()
    }

    /// Closes an open sidebar menu, the icon picker or the narrow window's drawer; true when
    /// one was open.
    pub(super) fn close_side_popups(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.close_menus(cx)
            | self.sidebar.picker.take().is_some()
            | std::mem::take(&mut self.sidebar.drawer);
        if open {
            cx.notify();
        }
        open
    }

    fn run_side_menu(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        if let Act::Delete(_) = act {
            // Two steps: the first click only asks.
            if let Some(m) = self.sidebar.menu.as_mut().filter(|m| !m.confirm) {
                m.confirm = true;
                return cx.notify();
            }
        }
        self.close_menus(cx);
        match act {
            Act::NewChat(ws) => self.side_new_chat(ws, window, cx),
            Act::Preview(ws) => self.open_preview(&ws, window, cx),
            Act::StartTeam(ws) => self.start_team(ws, None, false, window, cx),
            Act::StopTeam(ws) => self.stop_team(ws, false, window, cx),
            Act::Rename(ws) => self.start_rename(ws, window, cx),
            Act::Icon(ws) => self.open_icon_picker(ws, window, cx),
            Act::RunOn(ws, machine) => self.update(cx, |c| {
                if let Some(w) = c
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == ws && w.panes.is_empty())
                {
                    w.machine_id = machine;
                }
            }),
            Act::Delete(ws) => {
                if self.project_view.is_some() && self.config().active_workspace == ws {
                    self.project_view = None;
                }
                self.delete_project(ws, cx);
            }
            Act::UseAccount(id) => self.update(cx, |c| c.settings.default_account_id = Some(id)),
            Act::Accounts => self.open_accounts(window, cx),
            Act::Usage => self.open_usage(window, cx),
            Act::Mods => self.open_mods(window, cx),
            Act::Settings => self.open_settings(window, cx),
        }
        cx.notify();
    }

    // ---- Rename and icon ----------------------------------------------------------------

    fn start_rename(&mut self, ws: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self
            .config()
            .workspaces
            .iter()
            .find(|w| w.id == ws)
            .map(|w| w.name.clone())
        else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let sub = cx.subscribe_in(&input, window, |this, _, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.save_rename(cx);
            }
        });
        input.update(cx, |s, cx| s.select_all(window, cx));
        window.focus(&input.focus_handle(cx), cx);
        self.sidebar.rename = Some(Rename {
            ws,
            input,
            _sub: sub,
        });
        cx.notify();
    }

    fn save_rename(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.sidebar.rename.take() else {
            return;
        };
        let name: String = r.input.read(cx).value().trim().chars().take(64).collect();
        if !name.is_empty() {
            self.update(cx, |c| {
                if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == r.ws) {
                    w.name = name;
                }
            });
        }
        cx.notify();
    }

    fn open_icon_picker(&mut self, ws: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = self.config().workspaces.iter().find(|w| w.id == ws) else {
            return;
        };
        // The project's own folder (else its first chat's), top-level markers; none remote.
        let suggested = match (&w.machine_id, w.folder.as_str()) {
            (Some(_), _) => vec![],
            (None, "") => w.panes.first().map(|p| p.cwd.clone()).map_or(vec![], |d| {
                suggest_icons(&read_markers(&resolve_user_path(&d, &home_dir())))
            }),
            (None, dir) => suggest_icons(&read_markers(&resolve_user_path(dir, &home_dir()))),
        };
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search icons"));
        let sub = cx.subscribe_in(&query, window, |_, _, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        });
        window.focus(&query.focus_handle(cx), cx);
        self.sidebar.picker = Some(Picker {
            ws,
            general: false,
            query,
            suggested,
            _sub: sub,
        });
        cx.notify();
    }

    fn pick_icon(&mut self, icon: String, cx: &mut Context<Self>) {
        if let Some(picker) = self.sidebar.picker.take() {
            self.update(cx, |c| {
                if let Some(w) = c.workspaces.iter_mut().find(|w| w.id == picker.ws) {
                    w.icon = icon;
                }
            });
        }
        cx.notify();
    }

    /// Change a project's icon (`ProjectIconPicker`): search, Tech / General, icons suggested
    /// from the folder, Remove icon.
    fn icon_picker_layer(
        &mut self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.sidebar.picker.as_ref()?;
        let w = self
            .config()
            .workspaces
            .iter()
            .find(|w| w.id == picker.ws)?
            .clone();
        let p = pal(cx);
        let query = picker.query.read(cx).value().to_string();
        let general = picker.general;
        let shown = picker_icons(general, &query);
        let suggested = if query.trim().is_empty() {
            picker.suggested.clone()
        } else {
            vec![]
        };
        let input = picker.query.clone();
        let cell = |id: String, cx: &mut Context<Self>| {
            let on = w.icon == id;
            let (tip, pick) = (icon_label(&id), id.clone());
            div()
                .id(SharedString::from(format!("pip-{id}")))
                .a11y_toggle(tip.clone(), on)
                .size(z(40.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(z(8.))
                .cursor_pointer()
                .when(on, |d| d.bg(p.sel))
                .hover(|s| s.bg(p.raise))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(project_icon(&id, 20., p.mu))
                .on_click(cx.listener(move |this, _, _, cx| this.pick_icon(pick.clone(), cx)))
        };
        let grid = |ids: Vec<String>, cx: &mut Context<Self>| {
            div()
                .flex()
                .flex_wrap()
                .gap(z(2.))
                .children(ids.into_iter().map(|id| cell(id, cx)).collect::<Vec<_>>())
        };
        let section = |t: &'static str| {
            div()
                .pt(z(8.))
                .pb(z(4.))
                .text_size(z(11.5))
                .text_color(p.fa)
                .child(t)
        };
        let tab = |id: &'static str, label: &'static str, on: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .role(Role::Tab)
                .aria_label(label)
                .aria_selected(on)
                .px(z(10.))
                .py(z(4.))
                .rounded(z(6.))
                .cursor_pointer()
                .text_color(if on { p.tx } else { p.mu })
                .when(on, |d| d.bg(p.sel))
                .hover(|s| s.bg(p.raise))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(pk) = this.sidebar.picker.as_mut() {
                        pk.general = id == "pip-general";
                    }
                    cx.notify();
                }))
        };
        let none = shown.is_empty();
        let current = if w.icon.is_empty() {
            "folder".to_string()
        } else {
            icon_label(&w.icon)
        };
        let has_icon = !w.icon.is_empty();
        let suggested_el = (!suggested.is_empty()).then(|| {
            div()
                .child(section("Suggested from the folder"))
                .child(grid(suggested, cx))
        });
        let shown_el = grid(shown, cx);
        let tech_tab = tab("pip-tech", "Tech", !general, cx);
        let general_tab = tab("pip-general", "General", general, cx);
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("pip-overlay")
                        .occlude()
                        .w(viewport.width)
                        .h(viewport.height)
                        .flex()
                        .justify_center()
                        .items_start()
                        .pt(px(90.))
                        .bg(hsla(0., 0., 0., 0.45))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.sidebar.picker = None;
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .id("pip")
                                .w(z(372.))
                                .max_h(viewport.height - px(140.))
                                .flex()
                                .flex_col()
                                .rounded(z(12.))
                                .border_1()
                                .border_color(p.line)
                                .bg(p.panel)
                                .shadow_lg()
                                .text_size(z(13.))
                                .text_color(p.tx)
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                                    if e.keystroke.key == "escape" {
                                        this.sidebar.picker = None;
                                        cx.stop_propagation();
                                        cx.notify();
                                    }
                                }))
                                .child(
                                    div()
                                        .p(z(12.))
                                        .pb(z(6.))
                                        .flex()
                                        .items_center()
                                        .gap(z(8.))
                                        .child(glyph(Glyph::Search, 14., p.mu, p.panel))
                                        .child(div().flex_1().child(Input::new(&input).small())),
                                )
                                .child(
                                    div()
                                        .px(z(12.))
                                        .flex()
                                        .gap(z(4.))
                                        .child(tech_tab)
                                        .child(general_tab),
                                )
                                .child(
                                    div()
                                        .id("pip-body")
                                        .min_h(px(0.))
                                        .overflow_y_scroll()
                                        .px(z(12.))
                                        .pb(z(8.))
                                        .children(suggested_el)
                                        .child(section(if general { "General" } else { "Tech" }))
                                        .child(shown_el)
                                        .when(none, |d| {
                                            d.child(div().py(z(12.)).text_color(p.fa).child(
                                                format!("No icon matches “{}”.", query.trim()),
                                            ))
                                        }),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .px(z(12.))
                                        .py(z(10.))
                                        .border_t_1()
                                        .border_color(p.line)
                                        .child(
                                            div()
                                                .id("pip-remove")
                                                .a11y_button("Remove icon")
                                                .px(z(10.))
                                                .py(z(4.))
                                                .rounded(z(6.))
                                                .border_1()
                                                .border_color(p.line)
                                                .when(!has_icon, |d| d.opacity(0.45))
                                                .when(has_icon, |d| {
                                                    d.cursor_pointer()
                                                        .hover(|s| s.bg(p.raise))
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.pick_icon(String::new(), cx)
                                                        }))
                                                })
                                                .child("Remove icon"),
                                        )
                                        .child(div().text_color(p.fa).child(current)),
                                ),
                        ),
                ),
            )
            .with_priority(4)
            .into_any_element(),
        )
    }
}

/// A square icon button (`.icon-btn`).
fn icon_btn(id: &'static str, g: Glyph, tip: String, on: bool, p: &Pal) -> Stateful<Div> {
    let p = *p;
    div()
        .id(id)
        .a11y_button(tip.clone())
        .size(z(30.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(z(6.))
        .cursor_pointer()
        .when(on, |d| d.bg(p.sel))
        .hover(|s| s.bg(p.raise))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .child(glyph(g, 16., p.mu, p.side))
}

fn kbd_el(text: String, p: &Pal, cx: &App) -> Div {
    div()
        .flex_none()
        .px(z(5.))
        .rounded(z(4.))
        .border_1()
        .border_color(p.line2)
        .bg(p.bg)
        .font_family(crate::terminal::mono(cx))
        .text_size(z(10.5))
        .text_color(p.mu)
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    // `super::*` brings gpui's `test` attribute (through `gpui_kit::*`); these are plain tests.
    use core::prelude::v1::test;

    #[test]
    fn suggests_icons_from_marker_files() {
        let files = |pairs: &[(&'static str, &str)]| -> HashMap<&'static str, String> {
            pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
        };
        assert_eq!(
            suggest_icons(&files(&[(
                "package.json",
                r#"{"dependencies":{"next":"1","react":"1"},"devDependencies":{"typescript":"5"}}"#
            )])),
            ["dev:nextjs", "mit:react", "mit:typescript"]
        );
        assert_eq!(
            suggest_icons(&files(&[
                ("composer.json", "laravel/framework"),
                ("Dockerfile", "")
            ])),
            ["mit:laravel", "mit:docker"]
        );
        assert_eq!(
            suggest_icons(&files(&[("package.json", "not json"), ("Cargo.toml", "")])),
            ["mit:nodejs", "mit:rust"]
        );
        assert!(suggest_icons(&HashMap::new()).is_empty());
    }

    #[test]
    fn icon_ids_and_bundled_sets() {
        assert!(valid_icon_id("mit:react"));
        assert!(!valid_icon_id("🦀"));
        assert!(!valid_icon_id("mit:React"));
        assert!(!valid_icon_id("x:react"));
        // Unknown ids draw the folder; every suggested id exists.
        assert!(std::ptr::eq(
            icon_or_folder("mit:nope").unwrap(),
            icon_or_folder(DEFAULT_ICON).unwrap()
        ));
        for id in [
            "dev:nextjs",
            "mit:nuxt",
            "mit:angular",
            "mit:svelte",
            "mit:astro",
            "dev:electron",
            "mit:tauri",
            "mit:vue",
            "mit:react",
            "mit:vite",
            "mit:typescript",
            "mit:nodejs",
            "mit:rust",
            "mit:go",
            "mit:django",
            "mit:python",
            "dev:flutter",
            "mit:ruby",
            "mit:docker",
            "mit:laravel",
            "mit:php",
            "dev:spring",
            "mit:kotlin",
            "mit:java",
        ] {
            assert!(project_icons().contains_key(id), "{id}");
        }
        assert!(provider_logos().contains_key("claude"));
        assert!(picker_icons(true, "").iter().all(|i| i.starts_with("ms:")));
        assert!(picker_icons(false, "react").contains(&"mit:react".to_string()));
        assert!(picker_icons(false, "")
            .iter()
            .all(|i| !i.starts_with("mit:folder-")));
        assert_eq!(icon_label("mit:react-ts"), "react ts");
    }

    #[test]
    fn labels() {
        assert_eq!(ago(0, 30_000), "now");
        assert_eq!(ago(0, 5 * 60_000), "5m");
        assert_eq!(ago(0, 3 * 3_600_000), "3h");
        assert_eq!(folder_name("~/code/api/"), "api");
        assert_eq!(plural(1, "pane", "panes"), "1 pane");
        if !cfg!(target_os = "macos") {
            assert_eq!(
                binding_hint(&Binding {
                    shift: true,
                    alt: false,
                    code: "KeyN".into()
                }),
                "Ctrl+Shift+N"
            );
            assert_eq!(chord(&["mod", "shift", "3"]), "Ctrl+Shift+3");
        }
    }
}
