//! Web preview in a pane (ADR 0003's job): a Wry webview whose console errors, uncaught
//! exceptions, rejected promises and failed requests are posted back over Wry's IPC,
//! which is what "Attach to chat" needs. Electron intercepted requests in its own
//! session; a system webview can't, so failed requests are caught by wrapping
//! fetch/XHR and listening for resource errors in the page instead.

use futures::StreamExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use gpui_webview::WebView;

const CAPTURE: &str = r#"(() => {
  const send = (kind, message) => {
    try { window.ipc.postMessage(JSON.stringify({ kind, message: String(message), url: location.href })) } catch {}
  };
  const show = (x) => x instanceof Error ? `${x.name}: ${x.message}${x.stack ? '\n' + x.stack : ''}` : typeof x === 'object' ? JSON.stringify(x) : String(x);
  const error = console.error;
  console.error = (...args) => { send('console.error', args.map(show).join(' ')); error.apply(console, args) };
  window.addEventListener('error', (e) => {
    const t = e.target;
    if (t && t !== window && (t.src || t.href)) send('resource', 'failed to load ' + (t.src || t.href));
    else send('error', (e.message || 'error') + (e.filename ? ' at ' + e.filename + ':' + e.lineno : ''));
  }, true);
  window.addEventListener('unhandledrejection', (e) => send('unhandledrejection', show(e.reason)));
  const f = window.fetch;
  window.fetch = async (...args) => {
    const url = String(args[0] && args[0].url || args[0]);
    try {
      const r = await f(...args);
      if (!r.ok) send('fetch', r.status + ' ' + url);
      return r;
    } catch (err) { send('fetch', 'failed ' + url + ': ' + err); throw err }
  };
  const open = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url, ...rest) {
    this.addEventListener('loadend', () => { if (this.status === 0 || this.status >= 400) send('xhr', this.status + ' ' + url) });
    return open.call(this, method, url, ...rest);
  };
})();"#;

const TEST_PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>preview</title>
<style>body{font:14px system-ui;background:#16181d;color:#ddd;padding:24px}button{margin:4px}</style></head>
<body>
<h2>Preview test page</h2>
<p>Errors on load: console.error, an uncaught exception, a failed fetch, a missing image.</p>
<button onclick="console.error('clicked', {a: 1})">console.error</button>
<button onclick="null.boom()">throw</button>
<button onclick="Promise.reject(new Error('rejected'))">reject</button>
<button onclick="fetch('http://127.0.0.1:9/api')">fetch</button>
<img src="http://127.0.0.1:9/missing.png" width="1" height="1">
<script>
console.error('boot failed:', new Error('config missing'));
setTimeout(() => { undefinedFunction() }, 300);
fetch('http://127.0.0.1:9/health');
</script>
</body></html>"#;

pub struct PreviewPane {
    webview: Option<Entity<WebView>>,
    error: Option<String>,
    captured: Vec<(String, String)>,
    _rx: Option<Task<()>>,
}

impl PreviewPane {
    pub fn new(url: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        let mut builder = wry::WebViewBuilder::new()
            .with_initialization_script(CAPTURE)
            .with_ipc_handler(move |request| {
                let _ = tx.unbounded_send(request.body().clone());
            });
        builder = match url {
            Some(url) => builder.with_url(url),
            None => builder.with_html(TEST_PAGE),
        };
        let (webview, error) = match WebView::build(builder, window, cx) {
            Ok(webview) => (Some(cx.new(|_| webview)), None),
            Err(err) => {
                eprintln!("[preview] webview failed: {err}");
                (None, Some(err.to_string()))
            }
        };
        let task = cx.spawn(async move |this, cx| {
            while let Some(body) = rx.next().await {
                let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let kind = parsed["kind"].as_str().unwrap_or("?").to_string();
                let message = parsed["message"].as_str().unwrap_or(&body).to_string();
                eprintln!("[preview] {kind}: {message}");
                if this
                    .update(cx, |this, cx| {
                        this.captured.push((kind, message));
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            webview,
            error,
            captured: Vec::new(),
            _rx: Some(task),
        }
    }

    pub fn captured_count(&self) -> usize {
        self.captured.len()
    }

    /// A native child view draws over everything in its bounds, so it must be hidden
    /// whenever the pane isn't on screen.
    pub fn webview(&self) -> Option<Entity<WebView>> {
        self.webview.clone()
    }

    pub fn set_visible(webview: Option<Entity<WebView>>, visible: bool, cx: &mut App) {
        if let Some(webview) = webview {
            webview.update(cx, |w, _| if visible { w.show() } else { w.hide() });
        }
    }
}

impl Render for PreviewPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let page = match (&self.webview, &self.error) {
            (Some(webview), _) => div().size_full().child(webview.clone()),
            (None, error) => div().size_full().p_4().child(format!(
                "Webview unavailable: {}. On Linux the webview needs X11 or XWayland: start with --x11.",
                error.as_deref().unwrap_or("unknown error")
            )),
        };
        div()
            .size_full()
            .flex()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(div().flex_1().h_full().child(page))
            .child(
                div()
                    .id("captured")
                    .w(px(340.))
                    .h_full()
                    .overflow_y_scroll()
                    .border_l_1()
                    .border_color(theme.border)
                    .p_2()
                    .text_xs()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().child(format!(
                        "Captured for Attach to chat ({})",
                        self.captured.len()
                    )))
                    .children(self.captured.iter().map(|(kind, message)| {
                        div()
                            .p_1()
                            .rounded_sm()
                            .bg(theme.muted)
                            .child(div().text_color(theme.danger).child(kind.clone()))
                            .child(message.clone())
                    })),
            )
    }
}
