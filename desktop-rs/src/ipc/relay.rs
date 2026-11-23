//! `relay:*`: the relay WebSocket for the renderer's RelayHost (src/renderer/lib/relay/host.ts,
//! web app / phone access). Rust owns the socket: a webview's WebSocket follows the page's
//! mixed-content rules and gets throttled while the window is hidden in the tray. The ticket
//! comes from this device's session (cloud.rs), so the renderer never names a URL; frames pass
//! through as text (sealed end to end by the renderer): `relay:event` out, `relay:send` in.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// One frame at most (the server's own cap is 64 KiB of ciphertext, base64 inside JSON).
const FRAME_MAX: usize = 256 * 1024;

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
                let (ws, _) = match tokio_tungstenite::connect_async(url.as_str()).await {
                    Ok(ok) => ok,
                    Err(e) => {
                        eprintln!("wraithgrid: relay connect failed: {e}");
                        return event(&app, gen, "closed", json!({ "error": e.to_string() }));
                    }
                };
                event(&app, gen, "open", json!({}));
                let (mut write, mut read) = ws.split();
                loop {
                    tokio::select! {
                        msg = read.next() => match msg {
                            Some(Ok(Message::Text(text))) if text.len() <= FRAME_MAX => {
                                event(&app, gen, "frame", json!({ "text": text.as_str() }));
                            }
                            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                            Some(Ok(_)) => {}
                        },
                        out = rx.recv() => match out {
                            Some(text) => {
                                if write.send(Message::text(text)).await.is_err() {
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
