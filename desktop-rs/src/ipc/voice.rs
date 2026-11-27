//! `voice:*`: voice typing in the chat box (src/renderer/lib/voice.ts): the speech model's
//! download, then record → stop → text. The work is in crate::voice; events go out as
//! `voice:progress` (download), `voice:level` (how loud, while recording) and `voice:ended`
//! (a recording that hit its time limit).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::voice::{self, Engine, Recording};

#[derive(Default)]
pub struct Voice {
    rec: Mutex<Option<Recording>>,
    engine: Arc<Engine>,
    downloading: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

pub fn setup(app: &AppHandle) {
    app.manage(Voice::default());
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    _payload: Value,
) -> Option<Result<Value, String>> {
    let v = app.state::<Voice>();
    Some(Ok(match channel {
        "voice:status" => json!({
            "ok": true,
            "ready": voice::model_ready(),
            "bytes": voice::MODEL_BYTES,
            "downloading": v.downloading.load(Ordering::SeqCst),
            "maxSeconds": voice::MAX_SECS,
        }),
        "voice:download" => {
            if voice::model_ready() {
                return Some(Ok(super::ok()));
            }
            if v.downloading.swap(true, Ordering::SeqCst) {
                return Some(Ok(super::fail("The speech model is already downloading.")));
            }
            v.cancel.store(false, Ordering::SeqCst);
            let (cancel, downloading, task_app) =
                (v.cancel.clone(), v.downloading.clone(), app.clone());
            let r = blocking(move || {
                let r = voice::download_model(&cancel, |done, total| {
                    super::emit(
                        &task_app,
                        "voice:progress",
                        json!({ "done": done, "total": total }),
                    );
                });
                downloading.store(false, Ordering::SeqCst);
                r
            })
            .await;
            match r {
                Ok(Ok(())) => super::ok(),
                Ok(Err(e)) | Err(e) => super::fail(e),
            }
        }
        "voice:cancelDownload" => {
            v.cancel.store(true, Ordering::SeqCst);
            super::ok()
        }
        "voice:start" => {
            if !voice::model_ready() {
                return Some(Ok(
                    json!({ "ok": false, "code": "no_model", "error": "Download the speech model first." }),
                ));
            }
            // A new recording replaces one left running (another chat's box).
            v.rec.lock().unwrap().take();
            let (a1, a2) = (app.clone(), app.clone());
            let started = blocking(move || {
                voice::record(
                    move |level| super::emit(&a1, "voice:level", json!({ "level": level })),
                    move || super::emit(&a2, "voice:ended", json!({})),
                )
            })
            .await;
            match started {
                Ok(Ok(rec)) => {
                    *v.rec.lock().unwrap() = Some(rec);
                    // Load the model while the user talks, so stopping answers sooner.
                    let engine = v.engine.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        let _ = engine.warm();
                    });
                    super::ok()
                }
                Ok(Err(e)) | Err(e) => super::fail(e),
            }
        }
        "voice:stop" => {
            let Some(rec) = v.rec.lock().unwrap().take() else {
                return Some(Ok(super::fail("Not recording.")));
            };
            let engine = v.engine.clone();
            match blocking(move || engine.transcribe(&rec.finish())).await {
                Ok(Ok(text)) => json!({ "ok": true, "text": text }),
                Ok(Err(e)) | Err(e) => super::fail(e),
            }
        }
        "voice:cancel" => {
            v.rec.lock().unwrap().take();
            super::ok()
        }
        _ => return None,
    }))
}
