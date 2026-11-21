//! The one-shot loopback listener of a sign-in: `http://127.0.0.1:<port>/callback`, where the
//! server sends the browser back with `?code=&state=` (or `?error=&state=`).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::decode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callback {
    /// The one-time code for `/v1/desktop/token`.
    Code(String),
    /// The server's error (`access_denied`, `login_required`…).
    Error(String),
    /// Nothing arrived before the deadline.
    TimedOut,
    Cancelled,
}

pub struct Listener {
    listener: TcpListener,
    pub redirect_uri: String,
}

impl Listener {
    /// Binds a free port on 127.0.0.1 only (never all interfaces).
    pub fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}/callback"),
        })
    }

    /// Answers requests until the callback carrying `state` arrives, `deadline` passes or
    /// `cancel` is set. Other paths (favicon) and other states are answered and ignored.
    pub fn wait(&self, state: &str, deadline: Instant, cancel: &AtomicBool) -> Callback {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Callback::Cancelled;
            }
            if Instant::now() >= deadline {
                return Callback::TimedOut;
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(result) = answer(stream, state) {
                        return result;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }
}

fn answer(mut stream: TcpStream, state: &str) -> Option<Callback> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let target = read_target(&mut stream)?;
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    if path != "/callback" {
        respond(&mut stream, 404, "Not found", "");
        return None;
    }
    let param = |name: &str| {
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| decode(v))
    };
    if param("state").as_deref() != Some(state) {
        respond(
            &mut stream,
            400,
            "This sign-in link is old",
            "Start signing in again from Wraithgrid.",
        );
        return None;
    }
    if let Some(code) = param("code").filter(|c| !c.is_empty()) {
        respond(
            &mut stream,
            200,
            "You're signed in",
            "You can close this tab and go back to Wraithgrid.",
        );
        return Some(Callback::Code(code));
    }
    let error = param("error").unwrap_or_else(|| "access_denied".to_owned());
    respond(
        &mut stream,
        200,
        "Sign-in didn't finish",
        "Go back to Wraithgrid to try again.",
    );
    Some(Callback::Error(error))
}

/// The request target of a `GET` (up to 8 KiB of headers), or None for anything else.
fn read_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    let mut parts = text.lines().next()?.split(' ');
    (parts.next()? == "GET").then_some(())?;
    parts.next().map(str::to_owned)
}

fn respond(stream: &mut TcpStream, status: u16, title: &str, text: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{title}</title>\
<body style=\"font:15px/1.5 system-ui,sans-serif;max-width:420px;margin:15vh auto;padding:0 20px;color:#1a1a1a\">\
<h1 style=\"font-size:20px;margin:0 0 8px\">{title}</h1><p style=\"margin:0;color:#555\">{text}</p>"
    );
    // The URL carried a one-time code: no caching, no referrer, nothing else may load.
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\
Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(uri: &str, path: &str) -> String {
        let addr = uri
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap()
            .to_owned();
        let mut s = TcpStream::connect(addr).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    fn wait_on(l: Listener, state: &'static str) -> std::thread::JoinHandle<Callback> {
        std::thread::spawn(move || {
            l.wait(
                state,
                Instant::now() + Duration::from_secs(5),
                &AtomicBool::new(false),
            )
        })
    }

    #[test]
    fn returns_the_code_of_our_state_and_ignores_the_rest() {
        let l = Listener::bind().unwrap();
        let uri = l.redirect_uri.clone();
        assert!(uri.starts_with("http://127.0.0.1:") && uri.ends_with("/callback"));
        let waiting = wait_on(l, "our-state");
        assert!(get(&uri, "/favicon.ico").starts_with("HTTP/1.1 404"));
        assert!(get(&uri, "/callback?code=abc&state=other").starts_with("HTTP/1.1 400"));
        let page = get(&uri, "/callback?code=a%2Db_c&state=our-state");
        assert!(page.starts_with("HTTP/1.1 200"));
        assert!(page.contains("Referrer-Policy: no-referrer"));
        assert_eq!(waiting.join().unwrap(), Callback::Code("a-b_c".into()));
    }

    #[test]
    fn reports_the_server_error() {
        let l = Listener::bind().unwrap();
        let uri = l.redirect_uri.clone();
        let waiting = wait_on(l, "s");
        get(&uri, "/callback?error=access_denied&state=s");
        assert_eq!(
            waiting.join().unwrap(),
            Callback::Error("access_denied".into())
        );
    }

    #[test]
    fn gives_up_at_the_deadline_or_when_cancelled() {
        let l = Listener::bind().unwrap();
        let none = AtomicBool::new(false);
        assert_eq!(l.wait("s", Instant::now(), &none), Callback::TimedOut);
        let cancel = AtomicBool::new(true);
        let later = Instant::now() + Duration::from_secs(60);
        assert_eq!(l.wait("s", later, &cancel), Callback::Cancelled);
    }
}
