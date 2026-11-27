//! `relay:*`: the relay WebSocket for the renderer's RelayHost (src/renderer/lib/relay/host.ts,
//! web app / phone access). Rust owns the socket: a webview's WebSocket follows the page's
//! mixed-content rules and gets throttled while the window is hidden in the tray. The ticket
//! comes from this device's session (cloud.rs), so the renderer never names a URL; frames pass
//! through as text (sealed end to end by the renderer): `relay:event` out, `relay:send` in.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;
use tokio::time::{interval_at, timeout, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;

/// One frame at most (the server's own cap is 64 KiB of ciphertext, base64 inside JSON).
const FRAME_MAX: usize = 256 * 1024;
/// A connect that hasn't finished by then (black-holed network, captive portal) is given up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// We ping this often. It keeps NAT/proxy idle timers from silently dropping the link and gives
/// the server's idle timeout (120 s) traffic, and its pong proves the link is alive.
const PING_EVERY: Duration = Duration::from_secs(25);
/// Nothing at all received for this long (not even a pong): the link is dead (sleep/wake, a
/// network change leave the TCP connection half-open, and a read would wait forever). The
/// socket is dropped so the renderer reconnects, instead of looking connected while the server
/// has long told the browser this computer is offline.
const DEAD_AFTER: Duration = Duration::from_secs(70);

#[derive(Default)]
pub struct Relay {
    /// The open socket's outbox, by generation (a new connect replaces the old socket).
    out: Mutex<Option<(u64, mpsc::UnboundedSender<String>)>>,
    gen: AtomicU64,
}

pub fn setup(app: &AppHandle) {
    app.manage(Relay::default());
}

fn event(app: &AppHandle, gen: u64, kind: &str, extra: Value) {
    let mut v = json!({ "gen": gen, "kind": kind });
    if let (Value::Object(m), Value::Object(e)) = (&mut v, extra) {
        m.extend(e);
    }
    super::emit(app, "relay:event", v);
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    let relay = app.state::<Relay>();
    Some(Ok(match channel {
        "relay:connect" => {
            let url = match super::cloud::relay_socket(app).await {
                Ok(url) => url,
                Err(e) => return Some(Ok(super::fail(e))),
            };
            // The renderer names the generation, so it can match events that beat this answer.
            let Some(gen) = payload.get("gen").and_then(Value::as_u64) else {
                return Some(Ok(super::fail("Invalid relay connection.")));
            };
            relay.gen.store(gen, Ordering::SeqCst);
            let (tx, mut rx) = mpsc::unbounded_channel::<String>();
            *relay.out.lock().unwrap() = Some((gen, tx));
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let connect = tokio_tungstenite::connect_async(url.as_str());
                let (ws, _) = match timeout(CONNECT_TIMEOUT, connect).await {
                    Ok(Ok(ok)) => ok,
                    Ok(Err(e)) => {
                        eprintln!("wraithgrid: relay connect failed: {e}");
                        return event(&app, gen, "closed", json!({ "error": e.to_string() }));
                    }
                    Err(_) => {
                        eprintln!("wraithgrid: relay connect timed out");
                        let error = "Connecting to Wraithgrid timed out.";
                        return event(&app, gen, "closed", json!({ "error": error }));
                    }
                };
                event(&app, gen, "open", json!({}));
                let (mut write, mut read) = ws.split();
                let mut ping = interval_at(Instant::now() + PING_EVERY, PING_EVERY);
                ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
                let mut heard = Instant::now();
                loop {
                    tokio::select! {
                        msg = read.next() => match msg {
                            Some(Ok(Message::Text(text))) if text.len() <= FRAME_MAX => {
                                heard = Instant::now();
                                event(&app, gen, "frame", json!({ "text": text.as_str() }));
                            }
                            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                            // Pings, pongs: tungstenite answers the server's pings itself.
                            Some(Ok(_)) => heard = Instant::now(),
                        },
                        _ = ping.tick() => {
                            if heard.elapsed() >= DEAD_AFTER {
                                eprintln!("wraithgrid: relay link silent, reconnecting");
                                break;
                            }
                            let ping = Message::Ping(Vec::new().into());
                            if timeout(PING_EVERY, write.send(ping)).await.map_or(true, |r| r.is_err()) {
                                break;
                            }
                        },
                        out = rx.recv() => match out {
                            Some(text) => {
                                let sent = timeout(DEAD_AFTER, write.send(Message::text(text))).await;
                                if sent.map_or(true, |r| r.is_err()) {
                                    break;
                                }
                            }
                            // `relay:close`, or a newer connect took over.
                            None => {
                                let _ = write.send(Message::Close(None)).await;
                                break;
                            }
                        },
                    }
                }
                event(&app, gen, "closed", json!({}));
            });
            json!({ "ok": true, "gen": gen })
        }
        "relay:send" => {
            let gen = payload.get("gen").and_then(Value::as_u64).unwrap_or(0);
            let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() || text.len() > FRAME_MAX {
                return Some(Ok(super::fail("Invalid frame.")));
            }
            match relay.out.lock().unwrap().as_ref() {
                Some((g, tx)) if *g == gen && tx.send(text.to_string()).is_ok() => super::ok(),
                _ => super::fail("The relay is not connected."),
            }
        }
        "relay:close" => {
            let gen = payload.get("gen").and_then(Value::as_u64).unwrap_or(0);
            let mut out = relay.out.lock().unwrap();
            if out.as_ref().is_some_and(|(g, _)| *g == gen) {
                *out = None;
            }
            super::ok()
        }
        _ => return None,
    }))
}
