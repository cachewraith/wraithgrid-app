//! A terminal pane: `alacritty_terminal` keeps the grid, our own GPUI element draws it
//! (Zed's terminal_view is GPL and only read for reference). Panes live in the background
//! host (ADR 0001/0004), so the view only draws and sends input: the host's reader thread
//! parses output straight into the grid (`TermCore::feed`) and wakes the view at most once
//! per frame, and closing a view never stops the agent.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config as TermConfig, RenderableContent, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color, CursorShape, NamedColor, Processor, Rgb, StdSyncHandler,
};
use futures::StreamExt as _;
use gpui_kit::*;

use crate::keys::{self, Mods};

/// The terminal font from Settings, at the app's zoom. Set by the window as a global so
/// every pane follows a change at once.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalStyle {
    pub family: String,
    pub size: f32,
}

impl Global for TerminalStyle {}

/// The installed monospace family for code and paths outside terminals: GPUI's generic
/// `monospace` doesn't resolve on Linux. Resolved once at startup.
pub struct MonoFont(pub SharedString);

impl Global for MonoFont {}

/// Picks the first installed of the Settings font and the fallbacks.
pub fn resolve_mono(preferred: &str, cx: &App) -> SharedString {
    let installed = cx.text_system().all_font_names();
    std::iter::once(preferred)
        .chain(MONO_FAMILIES.iter().copied())
        .find(|f| installed.iter().any(|i| i == f))
        .unwrap_or("monospace")
        .to_string()
        .into()
}

pub fn mono(cx: &App) -> SharedString {
    cx.try_global::<MonoFont>()
        .map(|m| m.0.clone())
        .unwrap_or_else(|| "monospace".into())
}

/// A pane's terminal tells the window when something was sent (Enter), so the pane has a
/// conversation to continue next time (`hasConversation`).
pub enum TerminalEvent {
    Submitted,
}
const PADDING: f32 = 6.0;
const SCROLLBACK: usize = 10_000;
/// Fallbacks after the Settings font, until one is bundled (`WRAITHGRID_FONT` overrides).
const MONO_FAMILIES: &[&str] = &[
    "JetBrains Mono",
    "JetBrainsMono Nerd Font Mono",
    "Fira Code",
    "IBM Plex Mono",
    "Source Code Pro",
    "Cascadia Mono",
    "SF Mono",
    "Menlo",
    "Consolas",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
];

struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Sends text to the pane's process (through the host).
pub type PaneWriter = Arc<dyn Fn(&str) + Send + Sync>;
/// Tells the pane's process its new size (through the host).
pub type PaneResizer = Arc<dyn Fn(u16, u16) + Send + Sync>;
/// Starts or reattaches the pane once its size is known.
pub type PaneStarter = Box<dyn FnOnce(u16, u16, &mut App)>;

/// Terminal replies (device attributes, cursor reports) go straight back to the process.
#[derive(Clone)]
pub struct Listener {
    writer: PaneWriter,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        match event {
            TermEvent::PtyWrite(text) => (self.writer)(&text),
            TermEvent::Title(title) => *self.title.lock().unwrap() = Some(title),
            TermEvent::ResetTitle => *self.title.lock().unwrap() = None,
            _ => {}
        }
    }
}

/// A pane's screen state, shared by the host's reader thread (which parses into it) and
/// the view (which draws it).
pub struct TermCore {
    term: FairMutex<Term<Listener>>,
    parser: Mutex<Processor<StdSyncHandler>>,
    dirty: AtomicBool,
    wake: futures::channel::mpsc::UnboundedSender<()>,
    wake_rx: Mutex<Option<futures::channel::mpsc::UnboundedReceiver<()>>>,
    title: Arc<Mutex<Option<String>>>,
    /// Why the pane stopped: its exit, or an error starting it.
    ended: Mutex<Option<String>>,
    bytes: AtomicU64,
}

impl TermCore {
    pub fn new(writer: PaneWriter) -> Arc<TermCore> {
        let title = Arc::new(Mutex::new(None));
        let listener = Listener {
            writer,
            title: title.clone(),
        };
        let config = TermConfig {
            scrolling_history: SCROLLBACK,
            ..TermConfig::default()
        };
        let (wake, wake_rx) = futures::channel::mpsc::unbounded();
        Arc::new(TermCore {
            term: FairMutex::new(Term::new(
                config,
                &GridSize { cols: 80, rows: 24 },
                listener,
            )),
            parser: Mutex::new(Processor::new()),
            dirty: AtomicBool::new(false),
            wake,
            wake_rx: Mutex::new(Some(wake_rx)),
            title,
            ended: Mutex::new(None),
            bytes: AtomicU64::new(0),
        })
    }

    /// Parses output into the grid. Called on the host's reader thread.
    pub fn feed(&self, data: &str) {
        let mut parser = self.parser.lock().unwrap();
        // Small slices, so the view never waits on a whole read for the lock (ADR 0005).
        for slice in data.as_bytes().chunks(4096) {
            parser.advance(&mut *self.term.lock(), slice);
        }
        self.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
        self.wake();
    }

    /// The pane stopped (`message` says why); its screen stays.
    pub fn end(&self, message: String) {
        *self.ended.lock().unwrap() = Some(message);
        self.wake();
    }

    fn wake(&self) {
        // One wake-up per frame at most: the view clears the flag when it draws.
        if !self.dirty.swap(true, Ordering::AcqRel) {
            let _ = self.wake.unbounded_send(());
        }
    }

    pub fn title(&self) -> Option<String> {
        self.title.lock().unwrap().clone()
    }

    pub fn ended(&self) -> Option<String> {
        self.ended.lock().unwrap().clone()
    }
}

pub struct TerminalView {
    focus: FocusHandle,
    core: Arc<TermCore>,
    writer: PaneWriter,
    resizer: PaneResizer,
    starter: Option<PaneStarter>,
    /// What the pane runs, shown until the program sets a title.
    command: String,
    /// Bytes Shift+Enter sends for "new line" in this pane's CLI (claude's ESC CR).
    new_line: Option<&'static str>,
    /// The resolved family for the Settings font, cached until the Settings font changes.
    family: Option<(String, String)>,
    size: (usize, usize),
    cell: Size<Pixels>,
    origin: Point<Pixels>,
    cursor_bounds: Option<Bounds<Pixels>>,
    marked: Option<String>,
    selecting: bool,
    scroll_px: Pixels,
    _pump: Task<()>,
}

impl TerminalView {
    pub fn new(
        core: Arc<TermCore>,
        writer: PaneWriter,
        resizer: PaneResizer,
        starter: PaneStarter,
        command: String,
        new_line: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut rx = core
            .wake_rx
            .lock()
            .unwrap()
            .take()
            .expect("one view per pane core");
        let pump_core = core.clone();
        let pump = cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                pump_core.dirty.store(false, Ordering::Release);
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
                // Coalesce: never redraw a pane more than ~120 times a second.
                cx.background_executor()
                    .timer(Duration::from_millis(8))
                    .await;
            }
        });
        Self {
            focus: cx.focus_handle(),
            core,
            writer,
            resizer,
            starter: Some(starter),
            command,
            new_line,
            family: None,
            size: (0, 0),
            cell: size(px(8.), px(17.)),
            origin: Point::default(),
            cursor_bounds: None,
            marked: None,
            selecting: false,
            scroll_px: px(0.),
            _pump: pump,
        }
    }

    pub fn title(&self) -> String {
        self.core.title().unwrap_or_else(|| self.command.clone())
    }

    pub fn ended(&self) -> Option<String> {
        self.core.ended()
    }

    fn write(&mut self, bytes: &[u8]) {
        {
            let mut term = self.core.term.lock();
            term.scroll_display(Scroll::Bottom);
            term.selection = None;
        }
        (self.writer)(&String::from_utf8_lossy(bytes));
    }

    fn copy(&self, cx: &mut App) {
        if let Some(text) = self.core.term.lock().selection_to_string() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Text pastes as text; an image (a Wayland screenshot) is saved to a file and its
    /// path pasted, as the Electron app does for agent panes.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        for entry in item.entries() {
            if let ClipboardEntry::Image(image) = entry {
                let path = std::env::temp_dir().join(format!(
                    "wraithgrid-paste-{}.{}",
                    image.id,
                    image.format.extension()
                ));
                if std::fs::write(&path, &image.bytes).is_ok() {
                    let text = path.to_string_lossy().into_owned();
                    self.paste_text(&text);
                }
                return;
            }
        }
        if let Some(text) = item.text() {
            self.paste_text(&text);
        }
    }

    fn paste_text(&mut self, text: &str) {
        let bracketed = self
            .core
            .term
            .lock()
            .mode()
            .contains(TermMode::BRACKETED_PASTE);
        self.write(&keys::paste_bytes(text, bracketed));
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = k.modifiers;
        let shortcut = |key: &str| {
            (m.control && m.shift && k.key == key) || (m.platform && !m.control && k.key == key)
        };
        if shortcut("c") {
            self.copy(cx);
        } else if shortcut("v")
            || (m.control && !m.shift && !m.alt && k.key == "v" && !cfg!(target_os = "macos"))
        {
            self.paste(cx);
        } else if let (true, Some(new_line)) = (
            k.key == "enter" && m.shift && !m.control && !m.alt,
            self.new_line,
        ) {
            self.write(new_line.as_bytes());
        } else if m.shift && (k.key == "pageup" || k.key == "pagedown") {
            let page = self.size.1 as i32;
            self.core
                .term
                .lock()
                .scroll_display(Scroll::Delta(if k.key == "pageup" { page } else { -page }));
            cx.notify();
        } else {
            let mods = Mods {
                ctrl: m.control,
                alt: m.alt,
                shift: m.shift,
                platform: m.platform,
            };
            let app_cursor = self.core.term.lock().mode().contains(TermMode::APP_CURSOR);
            let Some(bytes) = keys::to_bytes(&k.key, k.key_char.as_deref(), mods, app_cursor)
            else {
                return;
            };
            self.write(&bytes);
            if k.key == "enter" {
                cx.emit(TerminalEvent::Submitted);
            }
        }
        cx.stop_propagation();
    }

    fn grid_point(&self, position: Point<Pixels>) -> (GridPoint, Side) {
        let term = self.core.term.lock();
        let x = (position.x - self.origin.x - px(PADDING)).max(px(0.)) / self.cell.width;
        let y = (position.y - self.origin.y).max(px(0.)) / self.cell.height;
        let col = (x.floor() as usize).min(self.size.0.saturating_sub(1));
        let row = (y.floor() as usize).min(self.size.1.saturating_sub(1));
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let line = Line(row as i32 - term.grid().display_offset() as i32);
        (GridPoint::new(line, Column(col)), side)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let (point, side) = self.grid_point(event.position);
        let ty = match event.click_count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.core.term.lock().selection = Some(Selection::new(ty, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (point, side) = self.grid_point(event.position);
        if let Some(selection) = self.core.term.lock().selection.as_mut() {
            selection.update(point, side);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        #[cfg(target_os = "linux")]
        if let Some(text) = self.core.term.lock().selection_to_string() {
            cx.write_to_primary(ClipboardItem::new_string(text));
        }
        let _ = cx;
    }

    fn scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_px += event.delta.pixel_delta(self.cell.height).y;
        let lines = (self.scroll_px / self.cell.height) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_px -= self.cell.height * lines as f32;
        let mode = *self.core.term.lock().mode();
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            let key: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            (self.writer)(&String::from_utf8_lossy(
                &key.repeat(lines.unsigned_abs() as usize),
            ));
        } else {
            self.core.term.lock().scroll_display(Scroll::Delta(lines));
        }
        cx.notify();
    }

    /// Lays out one frame: resizes the grid to the bounds, then batches cells into
    /// background rects and shaped text runs placed on the cell grid.
    fn layout(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) -> Frame {
        let style = cx
            .try_global::<TerminalStyle>()
            .cloned()
            .unwrap_or(TerminalStyle {
                family: "JetBrains Mono".into(),
                size: 13.0,
            });
        let font_size = px(style.size);
        let text_system = window.text_system();
        if self
            .family
            .as_ref()
            .is_none_or(|(wanted, _)| *wanted != style.family)
        {
            let resolved = std::env::var("WRAITHGRID_FONT").unwrap_or_else(|_| {
                let installed = text_system.all_font_names();
                std::iter::once(style.family.as_str())
                    .chain(MONO_FAMILIES.iter().copied())
                    .find(|f| installed.iter().any(|i| i == f))
                    .unwrap_or("monospace")
                    .to_string()
            });
            self.family = Some((style.family.clone(), resolved));
        }
        let family = self
            .family
            .as_ref()
            .map(|(_, f)| f.clone())
            .unwrap_or_default();
        let font = font(family.clone());
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .map(|s| s.width)
            .unwrap_or(px(8.));
        let line_height = px((style.size * 1.3).round());
        self.cell = size(cell_width, line_height);
        self.origin = bounds.origin;

        let cols = (((bounds.size.width - px(PADDING * 2.)) / cell_width).floor() as usize).max(2);
        let rows = ((bounds.size.height / line_height).floor() as usize).max(2);
        if (cols, rows) != self.size {
            self.size = (cols, rows);
            self.core.term.lock().resize(GridSize { cols, rows });
            match self.starter.take() {
                // The first size known: start the pane, or reattach to it, at this size.
                Some(start) => start(cols as u16, rows as u16, cx),
                None => (self.resizer)(cols as u16, rows as u16),
            }
        }

        let focused = self.focus.is_focused(window);
        let palette = cx.try_global::<crate::theme::TerminalColors>().cloned();
        let term = self.core.term.lock();
        let RenderableContent {
            display_iter,
            selection,
            cursor,
            display_offset,
            colors,
            mode,
        } = term.renderable_content();
        let background = color(
            Color::Named(NamedColor::Background),
            colors,
            palette.as_ref(),
        );
        let selection_bg: Hsla = palette
            .as_ref()
            .map_or(rgba(0x3d5a8acc).into(), |p| p.selection);
        let cell_origin = |row: usize, col: usize| {
            point(
                bounds.left() + px(PADDING) + cell_width * col as f32,
                bounds.top() + line_height * row as f32,
            )
        };

        let mut frame = Frame {
            background,
            line_height,
            rects: Vec::new(),
            lines: Vec::new(),
            cursor: Vec::new(),
        };
        let mut bg_run: Option<(usize, usize, usize, Hsla)> = None;
        let mut text_run: Option<TextBatch> = None;
        let flush_bg = |run: Option<(usize, usize, usize, Hsla)>, frame: &mut Frame| {
            if let Some((row, start, end, color)) = run {
                let origin = cell_origin(row, start);
                frame.rects.push(fill(
                    Bounds::new(origin, size(cell_width * (end - start) as f32, line_height)),
                    color,
                ));
            }
        };
        let flush_text = |batch: Option<TextBatch>, frame: &mut Frame| {
            let Some(batch) = batch else { return };
            if batch.text.trim().is_empty() && !batch.style.underline && !batch.style.strike {
                return;
            }
            let run = TextRun {
                len: batch.text.len(),
                font: Font {
                    weight: if batch.style.bold {
                        FontWeight::BOLD
                    } else {
                        FontWeight::NORMAL
                    },
                    style: if batch.style.italic {
                        FontStyle::Italic
                    } else {
                        FontStyle::Normal
                    },
                    ..font.clone()
                },
                color: batch.style.fg,
                background_color: None,
                underline: batch.style.underline.then(|| UnderlineStyle {
                    color: Some(batch.style.fg),
                    thickness: px(1.),
                    wavy: false,
                }),
                strikethrough: batch.style.strike.then(|| StrikethroughStyle {
                    color: Some(batch.style.fg),
                    thickness: px(1.),
                }),
            };
            let force_width = (!batch.style.wide).then_some(cell_width);
            let shaped =
                window
                    .text_system()
                    .shape_line(batch.text.into(), font_size, &[run], force_width);
            frame
                .lines
                .push((cell_origin(batch.row, batch.col), shaped));
        };

        for indexed in display_iter {
            let cell = indexed.cell;
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let row = (indexed.point.line.0 + display_offset as i32) as usize;
            let col = indexed.point.column.0;
            let (mut fg, mut bg) = (
                color(cell.fg, colors, palette.as_ref()),
                color(cell.bg, colors, palette.as_ref()),
            );
            let default_bg = matches!(cell.bg, Color::Named(NamedColor::Background));
            let mut paint_bg = !default_bg;
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
                paint_bg = true;
            }
            if cell.flags.contains(Flags::DIM) {
                fg.a *= 0.66;
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            if selection.is_some_and(|s| s.contains(indexed.point)) {
                bg = selection_bg;
                paint_bg = true;
            }
            let wide = cell.flags.contains(Flags::WIDE_CHAR);
            let width = if wide { 2 } else { 1 };

            let bg_continues =
                matches!(bg_run, Some((r, _, end, c)) if r == row && end == col && c == bg);
            if paint_bg && bg_continues {
                if let Some(run) = bg_run.as_mut() {
                    run.2 = col + width;
                }
            } else {
                flush_bg(bg_run.take(), &mut frame);
                if paint_bg {
                    bg_run = Some((row, col, col + width, bg));
                }
            }

            let style = CellStyle {
                fg,
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                strike: cell.flags.contains(Flags::STRIKEOUT),
                wide,
            };
            let continues = matches!(&text_run, Some(b) if !wide && b.row == row && b.col + b.cols == col && b.style == style);
            if !continues {
                flush_text(text_run.take(), &mut frame);
                text_run = Some(TextBatch {
                    row,
                    col,
                    cols: 0,
                    text: String::new(),
                    style,
                });
            }
            let batch = text_run.as_mut().unwrap();
            batch.text.push(cell.c);
            if let Some(extra) = cell.zerowidth() {
                batch.text.extend(extra);
            }
            batch.cols += width;
        }
        flush_bg(bg_run.take(), &mut frame);
        flush_text(text_run.take(), &mut frame);

        // Cursor, and the IME's uncommitted text over it.
        let cursor_row = cursor.point.line.0 + display_offset as i32;
        self.cursor_bounds = None;
        if (0..rows as i32).contains(&cursor_row) && mode.contains(TermMode::SHOW_CURSOR) {
            let origin = cell_origin(cursor_row as usize, cursor.point.column.0);
            let cell_bounds = Bounds::new(origin, size(cell_width, line_height));
            self.cursor_bounds = Some(cell_bounds);
            let cursor_color = color(Color::Named(NamedColor::Cursor), colors, palette.as_ref());
            if let Some(marked) = self.marked.clone().filter(|m| !m.is_empty()) {
                let run = TextRun {
                    len: marked.len(),
                    font: font.clone(),
                    color: color(
                        Color::Named(NamedColor::Foreground),
                        colors,
                        palette.as_ref(),
                    ),
                    background_color: None,
                    underline: Some(UnderlineStyle {
                        color: None,
                        thickness: px(1.),
                        wavy: false,
                    }),
                    strikethrough: None,
                };
                let shaped =
                    window
                        .text_system()
                        .shape_line(marked.into(), font_size, &[run], None);
                frame.cursor.push(fill(
                    Bounds::new(origin, size(shaped.width, line_height)),
                    background,
                ));
                frame.lines.push((origin, shaped));
            } else if !focused {
                let edge = px(1.);
                let b = cell_bounds;
                for r in [
                    Bounds::new(b.origin, size(b.size.width, edge)),
                    Bounds::new(point(b.left(), b.bottom() - edge), size(b.size.width, edge)),
                    Bounds::new(b.origin, size(edge, b.size.height)),
                    Bounds::new(point(b.right() - edge, b.top()), size(edge, b.size.height)),
                ] {
                    frame.cursor.push(fill(r, cursor_color));
                }
            } else {
                let shape = match cursor.shape {
                    CursorShape::Beam => Bounds::new(origin, size(px(2.), line_height)),
                    CursorShape::Underline => Bounds::new(
                        point(origin.x, cell_bounds.bottom() - px(2.)),
                        size(cell_width, px(2.)),
                    ),
                    CursorShape::Hidden => Bounds::default(),
                    _ => cell_bounds,
                };
                let mut c = cursor_color;
                c.a = 0.75;
                frame.cursor.push(fill(shape, c));
            }
        }
        drop(term);
        let _ = cx;
        frame
    }
}

#[derive(Clone, Copy, PartialEq)]
struct CellStyle {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    wide: bool,
}

struct TextBatch {
    row: usize,
    col: usize,
    cols: usize,
    text: String,
    style: CellStyle,
}

pub struct Frame {
    background: Hsla,
    line_height: Pixels,
    rects: Vec<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Vec<PaintQuad>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let focus = self.focus.clone();
        div()
            .id("terminal")
            .size_full()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll))
            .child(
                canvas(
                    {
                        let view = view.clone();
                        move |bounds, window, cx| {
                            view.update(cx, |pane, cx| pane.layout(bounds, window, cx))
                        }
                    },
                    move |bounds, frame: Frame, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, view), cx);
                        window.paint_quad(fill(bounds, frame.background));
                        for rect in frame.rects {
                            window.paint_quad(rect);
                        }
                        for (origin, line) in &frame.lines {
                            let _ = line.paint(
                                *origin,
                                frame.line_height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
                        for quad in frame.cursor {
                            window.paint_quad(quad);
                        }
                    },
                )
                .size_full(),
            )
    }
}

/// IME: the platform's composition is kept as marked text and drawn at the cursor; a
/// commit (or plain typed text) is written to the pty.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked.as_ref().map_or(0, |m| m.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        self.write(text.as_bytes());
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        _: Option<std::ops::Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!text.is_empty()).then(|| text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

fn hsla(rgb: Rgb) -> Hsla {
    Rgba {
        r: rgb.r as f32 / 255.,
        g: rgb.g as f32 / 255.,
        b: rgb.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

/// The 16 ANSI colours, then foreground, background and cursor: a dark default until the
/// theme format is ported.
const ANSI: [u32; 16] = [
    0x1d2027, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xc8ccd4, 0x5c6370,
    0xf0838c, 0xb0d68f, 0xf0d197, 0x82c0f5, 0xd59ae6, 0x76cdd8, 0xffffff,
];
const FOREGROUND: u32 = 0xd6dae0;
const BACKGROUND: u32 = 0x0f1115;
const CURSOR: u32 = 0xe6e6e6;

fn default_color(index: usize) -> Rgb {
    let hex = |v: u32| Rgb {
        r: (v >> 16) as u8,
        g: (v >> 8) as u8,
        b: v as u8,
    };
    match index {
        0..=15 => hex(ANSI[index]),
        16..=231 => {
            let i = index - 16;
            let level = |x: usize| if x == 0 { 0 } else { (55 + 40 * x) as u8 };
            Rgb {
                r: level(i / 36),
                g: level(i / 6 % 6),
                b: level(i % 6),
            }
        }
        232..=255 => {
            let v = (8 + 10 * (index - 232)) as u8;
            Rgb { r: v, g: v, b: v }
        }
        256 | 267 => hex(FOREGROUND),
        257 => hex(BACKGROUND),
        258 => hex(CURSOR),
        // Dim black..white, then dim foreground.
        259..=266 => {
            let c = hex(ANSI[index - 259]);
            Rgb {
                r: c.r / 3 * 2,
                g: c.g / 3 * 2,
                b: c.b / 3 * 2,
            }
        }
        _ => {
            let c = hex(FOREGROUND);
            Rgb {
                r: c.r / 3 * 2,
                g: c.g / 3 * 2,
                b: c.b / 3 * 2,
            }
        }
    }
}

/// A cell color: what the program set (OSC 4/10/11), else the theme's palette, else the
/// built-in one. Dim colors are the palette's at two thirds.
fn color(color: Color, colors: &Colors, palette: Option<&crate::theme::TerminalColors>) -> Hsla {
    let index = match color {
        Color::Spec(rgb) => return hsla(rgb),
        Color::Named(named) => named as usize,
        Color::Indexed(i) => i as usize,
    };
    if let Some(set) = colors[index] {
        return hsla(set);
    }
    if let Some(p) = palette {
        let dim = |mut c: Hsla| {
            c.l *= 0.67;
            c
        };
        match index {
            0..=15 => return p.ansi[index],
            256 | 267 => return p.foreground,
            257 => return p.background,
            258 => return p.cursor,
            259..=266 => return dim(p.ansi[index - 259]),
            268 => return dim(p.foreground),
            _ => {}
        }
    }
    hsla(default_color(index))
}
