//! Web preview (#74, `PreviewView.tsx`): the project's dev server and its page in a pane. The
//! server runs over pipes in the project folder (`core::preview::DevServer`) and stops with the
//! pane; the page is a system webview (Wry) that only loads addresses on this computer. Console
//! errors and failed requests are caught in the page itself (no request interception in a
//! system webview) for "Attach to chat".
//!
//! WebKitGTK embeds only into an X11 window: on native Wayland the page can't be shown in the
//! pane, and the pane offers the browser instead (ADR 0005).

use std::path::PathBuf;

use futures::StreamExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_webview::WebView;
use wraithgrid_core::preview::{
    detect_dev_command, is_local_url, preview_report, read_project_files, shell_launch, DevServer,
    PreviewEvent, PreviewFailure, PREVIEW_REPORT_MAX,
};

/// Posts the page's console errors, uncaught exceptions, rejections and failed requests back
/// over Wry's IPC.
const CAPTURE: &str = r#"(() => {
  const send = (m) => { try { window.ipc.postMessage(JSON.stringify(m)) } catch {} };
  const show = (x) => x instanceof Error ? `${x.name}: ${x.message}` : typeof x === 'object' ? JSON.stringify(x) : String(x);
  const error = console.error;
  console.error = (...args) => { send({ kind: 'error', message: args.map(show).join(' ') }); error.apply(console, args) };
  window.addEventListener('error', (e) => {
    const t = e.target;
    if (t && t !== window && (t.src || t.href)) send({ kind: 'request', method: 'GET', url: t.src || t.href, result: 'failed to load' });
    else send({ kind: 'error', message: (e.message || 'error') + (e.filename ? ` (${e.filename.split('/').pop()}:${e.lineno})` : '') });
  }, true);
  window.addEventListener('unhandledrejection', (e) => send({ kind: 'error', message: 'Unhandled rejection: ' + show(e.reason) }));
  const f = window.fetch;
  window.fetch = async (...args) => {
    const req = args[0];
    const url = String(req && req.url || req);
    const method = (args[1] && args[1].method || req && req.method || 'GET').toUpperCase();
    try {
      const r = await f(...args);
      if (!r.ok) send({ kind: 'request', method, url: r.url || url, result: String(r.status) });
      return r;
    } catch (err) { send({ kind: 'request', method, url, result: String(err) }); throw err }
  };
  const open = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url, ...rest) {
    this.addEventListener('loadend', () => {
      if (this.status === 0 || this.status >= 400)
        send({ kind: 'request', method: String(method).toUpperCase(), url: String(url), result: this.status ? String(this.status) : 'network error' });
    });
    return open.call(this, method, url, ...rest);
  };
})();"#;

pub enum PreviewViewEvent {
    /// Start was pressed in the form: the project's command and address.
    Save { command: String, url: String },
    /// "Attach to chat": the report for the agent.
    Attach { report: String },
    /// Cancel on a project with no command: the pane closes.
    Close,
}

#[derive(Clone, PartialEq)]
enum State {
    Idle,
    Starting,
    Running,
    Stopped {
        code: Option<i32>,
        error: Option<String>,
    },
}

/// What the page tells the view.
enum PageMsg {
    Ipc(String),
    Loaded(String),
    /// A link to somewhere else than this computer: the browser opens it.
    External(String),
}

pub struct PreviewView {
    focus: FocusHandle,
    project: String,
    cwd: PathBuf,
    command: String,
    fixed_url: String,
    state: State,
    url: Option<String>,
    log: Vec<String>,
    show_log: bool,
    editing: bool,
    server: Option<DevServer>,
    webview: Option<Entity<WebView>>,
    webview_error: Option<String>,
    /// Shown in its pane, nothing drawn over it: a native webview covers whatever is above it.
    visible: bool,
    address: Entity<InputState>,
    at: String,
    errors: Vec<String>,
    failed: Vec<PreviewFailure>,
    form_command: Entity<InputState>,
    form_url: Entity<InputState>,
    source: Option<String>,
    /// A detected command, put in the form at the next draw (setting a value needs the window).
    pending_command: Option<String>,
    attach_error: Option<String>,
    _server_rx: Option<Task<()>>,
    _page_rx: Option<Task<()>>,
}

impl PreviewView {
    pub fn new(
        project: String,
        cwd: PathBuf,
        command: String,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(
            &address,
            window,
            |this, input, e: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = e {
                    let text = input.read(cx).value().trim().to_string();
                    this.go(text, window, cx);
                }
            },
        )
        .detach();
        let form_command = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("npm run dev")
                .default_value(command.clone())
        });
        let form_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("http://localhost:…")
                .default_value(url.clone())
        });
        for i in [&form_command, &form_url] {
            cx.observe(i, |_, _, cx| cx.notify()).detach();
        }
        let mut view = PreviewView {
            focus: cx.focus_handle(),
            project,
            cwd,
            command,
            fixed_url: url,
            state: State::Idle,
            url: None,
            log: vec![],
            show_log: false,
            editing: false,
            server: None,
            webview: None,
            webview_error: None,
            visible: true,
            address,
            at: String::new(),
            errors: vec![],
            failed: vec![],
            form_command,
            form_url,
            source: None,
            pending_command: None,
            attach_error: None,
            _server_rx: None,
            _page_rx: None,
        };
        if view.command.is_empty() {
            view.detect(cx);
        } else {
            // A project that has its command starts straight away (also after a restart).
            view.start(cx);
        }
        view
    }

    /// The form's command from the project's files (`detect`), when it has none yet.
    fn detect(&mut self, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move { detect_dev_command(&read_project_files(&cwd)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some((command, source)) = found {
                    this.source = Some(source);
                    if this.form_command.read(cx).value().trim().is_empty() {
                        this.pending_command = Some(command);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Runs the project's command; the page opens at the fixed address, else at the first
    /// local address the server prints.
    fn start(&mut self, cx: &mut Context<Self>) {
        self.server = None;
        self.webview = None;
        self.webview_error = None;
        self.log.clear();
        self.errors.clear();
        self.failed.clear();
        self.url = is_local_url(&self.fixed_url).then(|| self.fixed_url.clone());
        self.state = State::Starting;
        let (file, args) = shell_launch(
            &self.command,
            cfg!(windows),
            std::env::var("ComSpec").ok().as_deref(),
        );
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<PreviewEvent>();
        let env = crate::runtime::spawn_env();
        match DevServer::start(&file, &args, &self.cwd, &env, move |e| {
            let _ = tx.unbounded_send(e);
        }) {
            Ok(server) => self.server = Some(server),
            Err(e) => {
                self.state = State::Stopped {
                    code: None,
                    error: Some(e),
                };
                return cx.notify();
            }
        }
        self._server_rx = Some(cx.spawn(async move |this, cx| {
            while let Some(e) = rx.next().await {
                if this
                    .update(cx, |this, cx| this.server_event(e, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn server_event(&mut self, e: PreviewEvent, cx: &mut Context<Self>) {
        match e {
            PreviewEvent::Log(line) => {
                self.log.push(line);
                let over = self.log.len().saturating_sub(500);
                self.log.drain(..over);
                if self.state == State::Starting && self.url.is_some() {
                    self.state = State::Running;
                }
            }
            PreviewEvent::Url(url) => {
                if self.url.is_none() {
                    self.url = Some(url);
                }
                self.state = State::Running;
            }
            PreviewEvent::Exit { code, error } => {
                self.server = None;
                self.state = State::Stopped { code, error };
            }
        }
        cx.notify();
    }

    /// Ends the dev server now (the app quits).
    pub fn stop_server(&mut self) {
        if let Some(s) = self.server.take() {
            s.stop();
        }
    }

    /// The pane is on screen with nothing over it, or not.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            if let Some(w) = &self.webview {
                w.update(cx, |w, _| if visible { w.show() } else { w.hide() });
            }
        }
    }

    /// Makes the page's webview once there is an address to show.
    fn ensure_webview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(url) = self.url.clone() else { return };
        if self.webview.is_some() || self.webview_error.is_some() {
            return;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<PageMsg>();
        let (ipc, nav, ext) = (tx.clone(), tx.clone(), tx);
        let builder = wry::WebViewBuilder::new()
            .with_url(&url)
            .with_initialization_script(CAPTURE)
            .with_ipc_handler(move |req| {
                let _ = ipc.unbounded_send(PageMsg::Ipc(req.body().clone()));
            })
            .with_on_page_load_handler(move |event, url| {
                if let wry::PageLoadEvent::Finished = event {
                    let _ = nav.unbounded_send(PageMsg::Loaded(url));
                }
            })
            // Only addresses on this computer load here; others go to the browser.
            .with_navigation_handler(move |url| {
                let here = is_local_url(&url) || url.starts_with("about:");
                if !here {
                    let _ = ext.unbounded_send(PageMsg::External(url));
                }
                here
            });
        match WebView::build(builder, window, cx) {
            Ok(w) => {
                let w = cx.new(|_| w);
                if !self.visible {
                    w.update(cx, |w, _| w.hide());
                }
                self.webview = Some(w);
            }
            Err(e) => self.webview_error = Some(e.to_string()),
        }
        self.at = url.clone();
        self.address
            .update(cx, |i, cx| i.set_value(url, window, cx));
        self._page_rx = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(m) = rx.next().await {
                if this
                    .update_in(cx, |this, window, cx| this.page_msg(m, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn page_msg(&mut self, m: PageMsg, window: &mut Window, cx: &mut Context<Self>) {
        match m {
            PageMsg::Loaded(url) => {
                // A new page starts a new report.
                if url != self.at {
                    self.errors.clear();
                    self.failed.clear();
                }
                self.at = url.clone();
                self.address
                    .update(cx, |i, cx| i.set_value(url, window, cx));
            }
            PageMsg::External(url) => cx.open_url(&url),
            PageMsg::Ipc(body) => {
                let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
                match v["kind"].as_str() {
                    Some("error") => self.errors.push(s("message")),
                    Some("request") => self.failed.push(PreviewFailure {
                        method: s("method"),
                        url: s("url"),
                        result: s("result"),
                    }),
                    _ => {}
                }
                let over = self.errors.len().saturating_sub(PREVIEW_REPORT_MAX);
                self.errors.drain(..over);
                let over = self.failed.len().saturating_sub(PREVIEW_REPORT_MAX);
                self.failed.drain(..over);
            }
        }
        cx.notify();
    }

    /// The address bar: a local address loads here, anything else opens in the browser.
    fn go(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let url = if text.contains("://") {
            text
        } else {
            format!("http://{text}")
        };
        if is_local_url(&url) {
            if let Some(w) = &self.webview {
                w.update(cx, |w, _| w.load_url(&url));
            }
        } else {
            cx.open_url(&url);
            let at = self.at.clone();
            self.address.update(cx, |i, cx| i.set_value(at, window, cx));
        }
    }

    fn attach(&mut self, cx: &mut Context<Self>) {
        let url = if self.at.is_empty() {
            self.url.clone().unwrap_or_default()
        } else {
            self.at.clone()
        };
        let size = self.webview.as_ref().map(|w| {
            let b = w.read(cx).bounds();
            (
                f32::from(b.size.width) as u32,
                f32::from(b.size.height) as u32,
            )
        });
        let (_, report) = preview_report(&url, size, &self.errors, &self.failed);
        cx.emit(PreviewViewEvent::Attach { report });
    }

    /// What the window answered to "Attach to chat": an error to show, or None.
    pub fn set_attach_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.attach_error = error;
        cx.notify();
    }
}

impl EventEmitter<PreviewViewEvent> for PreviewView {}

impl Focusable for PreviewView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for PreviewView {
    fn drop(&mut self) {
        if let Some(s) = self.server.take() {
            s.stop();
        }
    }
}

fn button(id: impl Into<SharedString>, label: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(ElementId::Name(id.into()))
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .text_sm()
        .cursor_pointer()
        .hover(|d| d.bg(theme.secondary))
        .child(label.into())
}

impl Render for PreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(command) = self.pending_command.take() {
            self.form_command
                .update(cx, |i, cx| i.set_value(command, window, cx));
        }
        let theme = cx.theme().clone();
        let muted = |t: String| div().text_xs().text_color(theme.muted_foreground).child(t);
        let mono = crate::terminal::mono(cx);
        if self.editing || (self.state == State::Idle && self.command.is_empty()) {
            let command = self.form_command.read(cx).value().trim().to_string();
            let url = self.form_url.read(cx).value().trim().to_string();
            let url_ok = url.is_empty() || is_local_url(&url);
            let can = !command.is_empty() && url_ok;
            return div()
                .id("pv-start")
                .track_focus(&self.focus)
                .size_full()
                .p_6()
                .bg(theme.background)
                .child(
                    div()
                        .max_w(px(520.))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("Run {}'s dev server", self.project)),
                        )
                        .child(muted(format!(
                            "Runs in {}; stops when this pane closes.",
                            self.cwd.display()
                        )))
                        .child(muted(format!(
                            "Command{}",
                            self.source
                                .as_ref()
                                .map_or(String::new(), |s| format!(" · from {s}"))
                        )))
                        .child(Input::new(&self.form_command))
                        .child(muted(
                            "Address · read from the server's output when empty".into(),
                        ))
                        .child(Input::new(&self.form_url))
                        .when(!url_ok, |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.warning)
                                    .child("Only an address on this computer (localhost)."),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    button("pv-go", "Start", cx)
                                        .when(!can, |d| d.opacity(0.5))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !can {
                                                return;
                                            }
                                            let command = this
                                                .form_command
                                                .read(cx)
                                                .value()
                                                .trim()
                                                .to_string();
                                            let url =
                                                this.form_url.read(cx).value().trim().to_string();
                                            cx.emit(PreviewViewEvent::Save {
                                                command: command.clone(),
                                                url: url.clone(),
                                            });
                                            this.command = command;
                                            this.fixed_url = url;
                                            this.editing = false;
                                            this.start(cx);
                                        })),
                                )
                                .child(button("pv-cancel", "Cancel", cx).on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if this.command.is_empty() {
                                            cx.emit(PreviewViewEvent::Close);
                                        } else {
                                            this.editing = false;
                                            cx.notify();
                                        }
                                    },
                                ))),
                        ),
                )
                .into_any_element();
        }
        self.ensure_webview(window, cx);
        let stopped = matches!(self.state, State::Stopped { .. });
        let problems = self.errors.len() + self.failed.len();
        let log = div()
            .id("pv-log")
            .max_h(px(220.))
            .overflow_y_scroll()
            .p_2()
            .bg(theme.secondary)
            .font_family(mono)
            .text_xs()
            .child(if self.log.is_empty() {
                "No output yet.".to_string()
            } else {
                self.log.join("\n")
            });
        div()
            .id("pv")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .when_some(
                match &self.state {
                    State::Stopped { code, error } => Some((*code, error.clone())),
                    _ => None,
                },
                |d, (code, error)| {
                    d.child(
                        div()
                            .px_2()
                            .py_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .bg(theme.secondary)
                            .child(error.unwrap_or_else(|| "The dev server stopped.".into()))
                            .when_some(code, |d, c| d.child(muted(format!("exit {c}"))))
                            .child(div().flex_1())
                            .child(button("pv-edit", "Edit command", cx).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.editing = true;
                                    cx.notify();
                                },
                            )))
                            .child(
                                button(
                                    "pv-log-btn",
                                    if self.show_log { "Hide log" } else { "Show log" },
                                    cx,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_log = !this.show_log;
                                    cx.notify();
                                })),
                            )
                            .child(button("pv-restart", "↻ Restart", cx).on_click(cx.listener(
                                |this, _, _, cx| this.start(cx),
                            ))),
                    )
                },
            )
            .when(self.url.is_some(), |d| {
                let back = self.webview.clone();
                let fwd = self.webview.clone();
                let reload = self.webview.clone();
                let at = self.at.clone();
                d.child(
                    div()
                        .px_2()
                        .py_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(button("pv-back", "←", cx).on_click(cx.listener(
                            move |_, _, _, cx| {
                                if let Some(w) = &back {
                                    w.update(cx, |w, _| {
                                        let _ = w.back();
                                    });
                                }
                            },
                        )))
                        .child(button("pv-fwd", "→", cx).on_click(cx.listener(
                            move |_, _, _, cx| {
                                if let Some(w) = &fwd {
                                    w.update(cx, |w, _| {
                                        let _ = w.forward();
                                    });
                                }
                            },
                        )))
                        .child(button("pv-reload", "↻", cx).on_click(cx.listener(
                            move |_, _, _, cx| {
                                if let Some(w) = &reload {
                                    let _ = w.read(cx).raw().reload();
                                }
                            },
                        )))
                        .child(div().flex_1().child(Input::new(&self.address)))
                        .child(
                            button("pv-open", "↗", cx)
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new("Open in browser")
                                        .build(window, cx)
                                })
                                .on_click(move |_, _, cx| cx.open_url(&at)),
                        )
                        .child(button("pv-log-tb", "Log", cx).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.show_log = !this.show_log;
                                cx.notify();
                            },
                        )))
                        .child(
                            button(
                                "pv-attach",
                                if problems > 0 {
                                    format!("📎 Attach to chat · {problems}")
                                } else {
                                    "📎 Attach to chat".into()
                                },
                                cx,
                            )
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(
                                    "Console errors and failed requests into the chat",
                                )
                                .build(window, cx)
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.attach(cx))),
                        ),
                )
            })
            .when_some(self.attach_error.clone(), |d, e| {
                d.child(div().px_2().text_sm().text_color(theme.danger).child(e))
            })
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .when(stopped, |d| d.opacity(0.5))
                    .child(match (&self.url, &self.webview, &self.webview_error) {
                        (Some(_), Some(w), _) => div().size_full().child(w.clone()),
                        (Some(url), None, Some(_)) => {
                            let url = url.clone();
                            div()
                                .size_full()
                                .p_6()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(div().child(format!("The dev server runs at {url}.")))
                                .child(muted(
                                    "This window can't show web pages here: the page view needs X11 (on Wayland, start Wraithgrid under XWayland)."
                                        .into(),
                                ))
                                .child(
                                    div().flex().child(
                                        button("pv-browser", "Open in browser", cx)
                                            .on_click(move |_, _, cx| cx.open_url(&url)),
                                    ),
                                )
                        }
                        _ => div().size_full().p_6().when(!stopped, |d| {
                            d.child(muted(format!(
                                "Starting {}… the page opens at the first local address it prints.",
                                self.command
                            )))
                        }),
                    }),
            )
            .when(
                self.show_log || (self.url.is_none() && !self.log.is_empty()),
                |d| d.child(log),
            )
            .into_any_element()
    }
}
