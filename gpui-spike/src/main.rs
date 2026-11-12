//! Phase 0 spike for the GPUI rewrite (#166): a 2×2 terminal grid, a streaming chat
//! bubble and a webview pane in one window with Wraithgrid's identity (app id, small
//! minimum size, close-only title bar when tiled). `--bench N` prints frame and memory
//! numbers after N seconds and quits, so runs can be compared with the Electron app.

mod chat;
mod chrome;
mod keys;
mod preview;
mod term;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::{window_border, ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use chat::ChatView;
use chrome::Chrome;
use preview::PreviewPane;
use term::TermPane;

#[derive(Default)]
struct Args {
    x11: bool,
    cmd: Option<Vec<String>>,
    bench: Option<u64>,
    tab: Tab,
    url: Option<String>,
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--x11" => args.x11 = true,
            "--cmd" => {
                args.cmd = it
                    .next()
                    .map(|c| c.split_whitespace().map(String::from).collect())
            }
            "--bench" => args.bench = it.next().and_then(|s| s.parse().ok()),
            "--url" => args.url = it.next(),
            "--tab" => {
                args.tab = match it.next().as_deref() {
                    Some("chat") => Tab::Chat,
                    Some("preview") => Tab::Preview,
                    _ => Tab::Terminals,
                }
            }
            "-h" | "--help" => {
                println!("wraithgrid-spike [--x11] [--cmd 'yes'] [--bench SECS] [--tab terminals|chat|preview] [--url URL]");
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum Tab {
    #[default]
    Terminals,
    Chat,
    Preview,
}

/// Gaps between painted frames, for "smooth" as a number rather than a feeling.
#[derive(Default)]
struct FrameStats {
    last: Option<Instant>,
    gaps_ms: Vec<f32>,
    second: u64,
    fps: u64,
}

impl FrameStats {
    fn record(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last.replace(now) {
            self.gaps_ms
                .push(now.duration_since(last).as_secs_f32() * 1000.);
        }
        self.second += 1;
    }
}

struct Spike {
    tab: Tab,
    panes: Vec<Entity<TermPane>>,
    chat: Entity<ChatView>,
    preview: Option<Entity<PreviewPane>>,
    url: Option<String>,
    frames: Rc<RefCell<FrameStats>>,
    chrome: Chrome,
    decorations: RefCell<String>,
    _ticker: Task<()>,
}

impl Spike {
    fn new(args: &Args, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let panes: Vec<_> = (0..4)
            .map(|_| cx.new(|cx| TermPane::new(args.cmd.clone(), cx).expect("spawn pty")))
            .collect();
        window.focus(&panes[0].focus_handle(cx), cx);
        let frames = Rc::new(RefCell::new(FrameStats::default()));
        let bench = args.bench;
        let started = Instant::now();
        let ticker = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let done = this
                .update(cx, |this, cx| {
                    {
                        let mut f = this.frames.borrow_mut();
                        f.fps = std::mem::take(&mut f.second);
                    }
                    cx.notify();
                    bench.is_some_and(|secs| started.elapsed() >= Duration::from_secs(secs))
                })
                .unwrap_or(true);
            if done {
                let _ = this.update(cx, |this, cx| this.report(started.elapsed(), cx));
                cx.update(|cx| cx.quit());
                break;
            }
        });
        let mut this = Self {
            tab: Tab::Terminals,
            panes,
            chat: cx.new(ChatView::new),
            preview: None,
            url: args.url.clone(),
            frames,
            chrome: chrome::current(),
            decorations: RefCell::default(),
            _ticker: ticker,
        };
        this.select(args.tab, window, cx);
        this
    }

    fn select(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == Tab::Preview && self.preview.is_none() {
            let url = self.url.clone();
            self.preview = Some(cx.new(|cx| PreviewPane::new(url, window, cx)));
        }
        if let Some(preview) = &self.preview {
            let webview = preview.read(cx).webview();
            PreviewPane::set_visible(webview, tab == Tab::Preview, cx);
        }
        if tab == Tab::Terminals {
            window.focus(&self.panes[0].focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn report(&self, elapsed: Duration, cx: &App) {
        let f = self.frames.borrow();
        let mut gaps = f.gaps_ms.clone();
        gaps.sort_by(f32::total_cmp);
        let pct = |p: f32| {
            gaps.get(((gaps.len() as f32 - 1.) * p) as usize)
                .copied()
                .unwrap_or(0.)
        };
        let secs = elapsed.as_secs_f64();
        let bytes: Vec<String> = self
            .panes
            .iter()
            .map(|p| format!("{:.1}", p.read(cx).total_bytes() as f64 / secs / 1e6))
            .collect();
        let (rss, anon, pss) = memory();
        println!(
            "bench: {secs:.1}s frames={} avg_fps={:.1} gap_ms p50={:.1} p95={:.1} p99={:.1} max={:.1} pane_MBps=[{}] rss_MB={rss:.0} anon_MB={anon:.0} pss_MB={pss:.0} preview_events={} decorations=[{}]",
            gaps.len() + 1,
            (gaps.len() + 1) as f64 / secs,
            pct(0.5),
            pct(0.95),
            pct(0.99),
            gaps.last().copied().unwrap_or(0.),
            bytes.join(", "),
            self.preview.as_ref().map_or(0, |p| p.read(cx).captured_count()),
            self.decorations.borrow(),
        );
    }

    fn title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // Chrome from the env as in platform.ts; GPUI's decorations only add "a client-side
        // window snapped to an edge is tiled" and "the compositor draws a real frame (KDE)".
        let (tiled_edge, native_frame) = match window.window_decorations() {
            Decorations::Client { tiling } => (
                tiling.top || tiling.left || tiling.right || tiling.bottom,
                false,
            ),
            Decorations::Server => (false, self.chrome == Chrome::Custom),
        };
        let (show_close, show_min_max) = match self.chrome {
            Chrome::Mac => (false, false),
            Chrome::Tiling => (true, false),
            Chrome::Overlay => (true, true),
            Chrome::Custom => (!native_frame, !native_frame && !tiled_edge),
        };
        *self.decorations.borrow_mut() = format!(
            "{:?} chrome={:?} controls={}",
            window.window_decorations(),
            self.chrome,
            match (show_close, show_min_max) {
                (true, true) => "min,max,close",
                (true, false) => "close",
                _ => "none",
            }
        );
        let tab = |label: &'static str, which: Tab, cx: &mut Context<Self>| {
            let theme = cx.theme();
            div()
                .id(label)
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(self.tab == which, |d| d.bg(theme.secondary))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| this.select(which, window, cx)))
        };
        let control = |id: &'static str,
                       glyph: &'static str,
                       area: WindowControlArea,
                       cx: &mut Context<Self>| {
            div()
                .id(id)
                .w(px(36.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|d| d.bg(cx.theme().secondary))
                .window_control_area(area)
                .child(glyph)
                .on_click(cx.listener(move |_, _, window, cx| match area {
                    WindowControlArea::Close => cx.quit(),
                    WindowControlArea::Max => window.zoom_window(),
                    _ => window.minimize_window(),
                }))
        };
        let f = self.frames.borrow();
        let (_, anon, pss) = memory();
        let stats = format!(
            "{} fps · {pss:.0} MB pss · {anon:.0} MB anon · {:?}",
            f.fps, self.chrome
        );
        drop(f);
        div()
            .id("title-bar")
            .h(px(36.))
            .flex()
            .items_center()
            .gap_1()
            .pl_3()
            .when(cfg!(target_os = "macos"), |d| d.pl(px(80.)))
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.border)
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.zoom_window();
                } else {
                    window.start_window_move();
                }
            })
            .child(div().pr_2().child("Wraithgrid"))
            .child(tab("Terminals", Tab::Terminals, cx))
            .child(tab("Chat", Tab::Chat, cx))
            .child(tab("Preview", Tab::Preview, cx))
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .pr_2()
                    .child(stats),
            )
            .when(show_min_max, |d| {
                d.child(control("min", "–", WindowControlArea::Min, cx))
                    .child(control("max", "□", WindowControlArea::Max, cx))
            })
            .when(show_close, |d| {
                d.child(control("close", "✕", WindowControlArea::Close, cx))
            })
    }

    fn grid(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let pane = |i: usize, window: &mut Window, cx: &mut Context<Self>| {
            let entity = &self.panes[i];
            let focused = entity.focus_handle(cx).contains_focused(window, cx);
            let p = entity.read(cx);
            let header = format!("{}  ·  {:.1} MB/s", p.title(), p.rate / 1e6);
            let theme = cx.theme();
            div()
                .flex_1()
                .min_w(px(0.))
                .min_h(px(0.))
                .flex()
                .flex_col()
                .border_1()
                .border_color(if focused { theme.ring } else { theme.border })
                .child(
                    div()
                        .h(px(22.))
                        .px_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .bg(theme.title_bar)
                        .child(header),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_hidden()
                        .child(entity.clone()),
                )
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(pane(0, window, cx))
                    .child(pane(1, window, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(pane(2, window, cx))
                    .child(pane(3, window, cx)),
            )
    }
}

impl Render for Spike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frames = self.frames.clone();
        let body: AnyElement = match self.tab {
            Tab::Terminals => self.grid(window, cx).into_any_element(),
            Tab::Chat => self.chat.clone().into_any_element(),
            Tab::Preview => self
                .preview
                .clone()
                .map(|p| p.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
        };
        let theme = cx.theme();
        window_border().child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .bg(theme.background)
                .text_color(theme.foreground)
                .text_sm()
                .child(self.title_bar(window, cx))
                .child(div().flex_1().min_h(px(0.)).child(body))
                .child(
                    canvas(|_, _, _| (), move |_, _, _, _| frames.borrow_mut().record())
                        .absolute()
                        .size_0(),
                ),
        )
    }
}

/// Memory of this process in MB: resident, private (anon) and proportional (PSS). Most
/// of the resident set is shared GPU driver libraries, so PSS and anon are the fair
/// numbers to compare. WebKit's own processes are not included.
fn memory() -> (f64, f64, f64) {
    let field = |file: &str, name: &str| {
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|kb| kb.parse::<f64>().ok())
            .map_or(0., |kb| kb / 1024.)
    };
    (
        field("/proc/self/status", "VmRSS:"),
        field("/proc/self/status", "RssAnon:"),
        field("/proc/self/smaps_rollup", "Pss:"),
    )
}

fn main() {
    let args = parse_args();
    // WebKitGTK can only be embedded through X11, and a GDK_BACKEND=wayland in the session
    // (common on Hyprland) overrides gpui-webview's own choice. Panes get the user's value back.
    #[cfg(target_os = "linux")]
    if args.x11 {
        term::USER_GDK_BACKEND.get_or_init(|| std::env::var_os("GDK_BACKEND"));
        std::env::set_var("GDK_BACKEND", "x11");
    }
    #[cfg(target_os = "linux")]
    let app = if args.x11 {
        gpui_kit::platform::linux(WindowingModes::X11)
    } else {
        gpui_kit::application()
    };
    #[cfg(not(target_os = "linux"))]
    let app = gpui_kit::application();

    app.run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1280.), px(800.)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("Wraithgrid".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(11.))),
            }),
            app_id: Some("wraithgrid".into()),
            window_min_size: Some(size(px(chrome::MIN_WIDTH), px(chrome::MIN_HEIGHT))),
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Spike::new(&args, window, cx))
        })
        .expect("open window");
        cx.activate(true);
    });
}
