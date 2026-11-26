//! `cloud:*`: the Wraithgrid account on wraithgrid-server (wraithgrid-cloud). Sign-in opens
//! the browser and waits on a loopback port; the session lives in cloud-session.json and is
//! rotated at start and once a day. `cloud:changed` tells the renderer whenever it changes.
//! `cloud:relay*`: pairing a browser/phone and the relay ticket, with this device's session
//! (the renderer runs the relay itself, src/renderer/lib/relay); its keys live in relay.json.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use wraithgrid_cloud::{
    authorize_url, device_name, install_id, platform, server_url, write_private, ApiError,
    Callback, Client, DeviceInfo, Listener, Pkce, SavedSession, TokenGrant, PROVIDERS,
    SESSION_FILE,
};
use wraithgrid_core::paths::current_user_data_dir;

/// The server keeps a sign-in request for 10 minutes.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const ROTATE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The relay's keys and pairings (the renderer's JSON, kept owner-only).
const RELAY_FILE: &str = "relay.json";
const RELAY_FILE_MAX: usize = 512 * 1024;
/// The web app pairing links open (`WRAITHGRID_APP_URL` overrides it, e.g. a local dev server).
const DEFAULT_APP_URL: &str = "https://app.wraithgrid.cachewraith.com";

fn app_url() -> String {
    std::env::var("WRAITHGRID_APP_URL")
        .ok()
        .filter(|u| u.starts_with("https://") || u.starts_with("http://localhost"))
        .unwrap_or_else(|| DEFAULT_APP_URL.into())
}

fn relay_file() -> PathBuf {
    current_user_data_dir().join(RELAY_FILE)
}

pub struct Cloud {
    client: Client,
    file: PathBuf,
    session: Mutex<Option<SavedSession>>,
    /// The open sign-in's cancel flag.
    pending: Mutex<Option<Arc<AtomicBool>>>,
}

impl Cloud {
    fn status(&self) -> Value {
        let session = self.session.lock().unwrap();
        json!({
            "server": self.client.server,
            "signedIn": session.is_some(),
            "user": session.as_ref().map(|s| s.user.clone()),
            "deviceId": session.as_ref().map(|s| s.device_id.clone()),
            "pending": self.pending.lock().unwrap().is_some(),
        })
    }

    fn token(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.token.clone())
    }

    fn store(&self, s: Option<SavedSession>) {
        match &s {
            Some(s) => {
                if let Err(e) = s.save(&self.file) {
                    eprintln!("wraithgrid: couldn't save the Wraithgrid session: {e}");
                }
            }
            None => SavedSession::remove(&self.file),
        }
        *self.session.lock().unwrap() = s;
    }

    /// Applies a change to the saved session, unless it was replaced or signed out meanwhile.
    fn update_if(&self, token: &str, f: impl FnOnce(&mut SavedSession)) -> bool {
        let mut next = match self.session.lock().unwrap().as_ref() {
            Some(s) if s.token == token => s.clone(),
            _ => return false,
        };
        f(&mut next);
        self.store(Some(next));
        true
    }
}

fn changed(app: &AppHandle) {
    let status = app.state::<Cloud>().status();
    super::emit(app, "cloud:changed", status);
}

pub fn setup(app: &AppHandle) {
    let server = server_url(std::env::var("WRAITHGRID_SERVER_URL").ok().as_deref());
    let file = current_user_data_dir().join(SESSION_FILE);
    let session = SavedSession::load(&file, &server);
    app.manage(Cloud {
        client: Client::live(server, APP_VERSION.to_owned()),
        file,
        session: Mutex::new(session),
        pending: Mutex::new(None),
    });

    // Rotate the token (a computer used once a month stays signed in) and pick up plan changes.
    let app = app.clone();
    std::thread::Builder::new()
        .name("cloud-rotate".into())
        .spawn(move || loop {
            rotate(&app);
            std::thread::sleep(ROTATE_EVERY);
        })
        .expect("cloud rotate thread");
}

fn rotate(app: &AppHandle) {
    let cloud = app.state::<Cloud>();
    let Some(token) = cloud.token() else { return };
    match cloud.client.refresh(&token) {
        Ok((fresh, expires_at)) => {
            // Keep the new token before anything else: the old one only lasts a short grace on
            // the server, and a request that fails with it must not find it still current.
            let kept = cloud.update_if(&token, |s| {
                s.token = fresh.clone();
                s.expires_at = expires_at;
            });
            if kept {
                if let Ok(user) = cloud.client.me(&fresh) {
                    cloud.update_if(&fresh, |s| s.user = user);
                }
            }
            changed(app);
        }
        Err(e) if e.is_signed_out() => {
            if cloud.update_if(&token, |_| {}) {
                cloud.store(None);
                changed(app);
            }
        }
        // Offline: the current token keeps working; try again next time.
        Err(_) => {}
    }
}

/// One sign-in, start to finish (blocking: waits for the browser).
fn sign_in(app: &AppHandle, provider: &str, cancel: &AtomicBool) -> Result<(), String> {
    let cloud = app.state::<Cloud>();
    let pkce = Pkce::new().map_err(|e| e.to_string())?;
    let listener = Listener::bind().map_err(|e| format!("Couldn't start sign-in: {e}"))?;
    let url = authorize_url(
        &cloud.client.server,
        provider,
        &listener.redirect_uri,
        &pkce,
    );
    if !super::window::open_url(&url) {
        return Err("Couldn't open your browser.".into());
    }
    let code = match listener.wait(&pkce.state, Instant::now() + SIGN_IN_TIMEOUT, cancel) {
        Callback::Code(code) => code,
        Callback::Error(e) => {
            return Err(match e.as_str() {
                "access_denied" => "Sign-in was canceled or refused.".into(),
                "login_required" => "Sign-in didn't finish in the browser.".into(),
                other => format!("Sign-in failed ({other})."),
            })
        }
        Callback::TimedOut => return Err("Sign-in timed out. Try again.".into()),
        Callback::Cancelled => return Err("Sign-in canceled.".into()),
    };
    let grant = cloud
        .client
        .exchange(
            &code,
            &pkce.verifier,
            &listener.redirect_uri,
            &this_device(),
        )
        .map_err(|e| e.message)?;
    keep(&cloud, grant);
    Ok(())
}

fn this_device() -> DeviceInfo {
    DeviceInfo {
        name: device_name(),
        platform: platform().into(),
        app_version: APP_VERSION.into(),
        // The same install signing in again keeps its device (and pairings) on the account.
        install_id: install_id(&current_user_data_dir()),
    }
}

fn keep(cloud: &Cloud, grant: TokenGrant) {
    cloud.store(Some(SavedSession {
        server: cloud.client.server.clone(),
        token: grant.token,
        expires_at: grant.expires_at,
        device_id: grant.device_id,
        user: grant.user,
    }));
}

fn text(payload: &Value, key: &str) -> String {
    payload[key].as_str().unwrap_or_default().trim().to_owned()
}

/// A secret exactly as typed (passwords keep their spaces).
fn raw(payload: &Value, key: &str) -> String {
    payload[key].as_str().unwrap_or_default().to_owned()
}

/// Runs a call that ends signed in on this device; keeps the session and tells the renderer.
async fn sign_in_with(
    app: &AppHandle,
    call: impl FnOnce(&Client, &DeviceInfo) -> Result<TokenGrant, ApiError> + Send + 'static,
) -> Value {
    let task_app = app.clone();
    let result = blocking(move || {
        let cloud = task_app.state::<Cloud>();
        call(&cloud.client, &this_device()).map(|grant| keep(&cloud, grant))
    })
    .await;
    match result {
        Ok(()) => {
            changed(app);
            json!({ "ok": true, "status": app.state::<Cloud>().status() })
        }
        Err(e) => json!({ "ok": false, "error": e.message, "code": e.code }),
    }
}

/// A one-time relay ticket, as the WebSocket URL to open (relay.rs connects to it).
pub(super) async fn relay_socket(app: &AppHandle) -> Result<String, String> {
    let Some(token) = app.state::<Cloud>().token() else {
        return Err("Sign in to Wraithgrid first.".into());
    };
    let task_app = app.clone();
    blocking(move || {
        let c = &task_app.state::<Cloud>().client;
        let v = c.relay_ticket(&token).map_err(|e| e.message)?;
        let ticket = v["ticket"].as_str().unwrap_or_default();
        Ok(c.relay_socket_url(ticket))
    })
    .await
}

/// Runs a call with this device's session; `{ ok, ...answer }` or `{ ok: false, error, code }`.
async fn authed(
    app: &AppHandle,
    call: impl FnOnce(&Client, &str) -> Result<Value, ApiError> + Send + 'static,
) -> Value {
    let Some(token) = app.state::<Cloud>().token() else {
        return json!({ "ok": false, "error": "Sign in to Wraithgrid first.", "code": "signed_out" });
    };
    let task_app = app.clone();
    match blocking(move || call(&task_app.state::<Cloud>().client, &token)).await {
        Ok(Value::Object(mut m)) => {
            m.insert("ok".into(), json!(true));
            Value::Object(m)
        }
        Ok(_) => super::ok(),
        Err(e) => json!({ "ok": false, "error": e.message, "code": e.code }),
    }
}

/// Runs a call with nothing to keep; `{ ok }` or `{ ok: false, error, code }`.
async fn simple(
    app: &AppHandle,
    call: impl FnOnce(&Client) -> Result<(), ApiError> + Send + 'static,
) -> Value {
    let task_app = app.clone();
    match blocking(move || call(&task_app.state::<Cloud>().client)).await {
        Ok(()) => super::ok(),
        Err(e) => json!({ "ok": false, "error": e.message, "code": e.code }),
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .expect("cloud task panicked")
}

pub async fn handle(
    app: &AppHandle,
    channel: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    let cloud = app.state::<Cloud>();
    Some(Ok(match channel {
        "cloud:status" => cloud.status(),
        // The server's public config (which browser sign-ins it has set up); null offline.
        "cloud:config" => {
            let task_app = app.clone();
            blocking(move || task_app.state::<Cloud>().client.config().ok())
                .await
                .unwrap_or(Value::Null)
        }
        "cloud:signIn" => {
            let provider = payload["provider"].as_str().unwrap_or_default().to_owned();
            if !PROVIDERS.contains(&provider.as_str()) {
                return Some(Ok(super::fail("Unknown sign-in provider.")));
            }
            let cancel = {
                let mut pending = cloud.pending.lock().unwrap();
                if pending.is_some() {
                    return Some(Ok(super::fail(
                        "A sign-in is already open in your browser.",
                    )));
                }
                let flag = Arc::new(AtomicBool::new(false));
                *pending = Some(flag.clone());
                flag
            };
            changed(app);
            let task_app = app.clone();
            let result = blocking(move || sign_in(&task_app, &provider, &cancel)).await;
            *cloud.pending.lock().unwrap() = None;
            changed(app);
            match result {
                Ok(()) => json!({ "ok": true, "status": cloud.status() }),
                Err(e) => super::fail(e),
            }
        }
        // Email + password typed in the app; passwords and codes go straight to the server,
        // never to disk. Sign-in, code confirmation and reset all end signed in on this device.
        "cloud:emailSignIn" => {
            let (email, password) = (text(&payload, "email"), raw(&payload, "password"));
            sign_in_with(app, move |c, d| c.email_sign_in(&email, &password, d)).await
        }
        "cloud:emailVerify" => {
            let (email, otp) = (text(&payload, "email"), text(&payload, "otp"));
            sign_in_with(app, move |c, d| c.email_verify(&email, &otp, d)).await
        }
        "cloud:emailResetConfirm" => {
            let (email, otp) = (text(&payload, "email"), text(&payload, "otp"));
            let password = raw(&payload, "password");
            sign_in_with(app, move |c, d| {
                c.email_reset_confirm(&email, &otp, &password, d)
            })
            .await
        }
        "cloud:emailResetCheck" => {
            let (email, otp) = (text(&payload, "email"), text(&payload, "otp"));
            simple(app, move |c| c.email_reset_check(&email, &otp)).await
        }
        "cloud:emailSignUp" => {
            let (name, email) = (text(&payload, "name"), text(&payload, "email"));
            let password = raw(&payload, "password");
            simple(app, move |c| c.email_sign_up(&name, &email, &password)).await
        }
        "cloud:emailResend" => {
            let email = text(&payload, "email");
            simple(app, move |c| c.email_resend(&email)).await
        }
        "cloud:emailReset" => {
            let email = text(&payload, "email");
            simple(app, move |c| c.email_reset(&email)).await
        }
        "cloud:updateName" => {
            let name = text(&payload, "name");
            let Some(token) = cloud.token() else {
                return Some(Ok(super::fail("Not signed in.")));
            };
            let task_app = app.clone();
            let (token, result) = blocking(move || {
                let r = task_app.state::<Cloud>().client.update_name(&token, &name);
                (token, r)
            })
            .await;
            match result {
                Ok(user) => {
                    cloud.update_if(&token, |s| s.user = user);
                    changed(app);
                    json!({ "ok": true, "status": cloud.status() })
                }
                Err(e) => json!({ "ok": false, "error": e.message, "code": e.code }),
            }
        }
        "cloud:changePassword" => {
            let (current, next) = (raw(&payload, "current"), raw(&payload, "next"));
            let Some(token) = cloud.token() else {
                return Some(Ok(super::fail("Not signed in.")));
            };
            simple(app, move |c| c.change_password(&token, &current, &next)).await
        }
        "cloud:cancel" => {
            if let Some(flag) = cloud.pending.lock().unwrap().as_ref() {
                flag.store(true, Ordering::Relaxed);
            }
            super::ok()
        }
        "cloud:relayPair" => {
            let server = cloud.client.server.clone();
            let mut v = authed(app, |c, t| c.relay_pair(t)).await;
            if v["ok"] == json!(true) {
                v["server"] = json!(server);
                v["appUrl"] = json!(app_url());
            }
            v
        }
        "cloud:relayPairings" => authed(app, |c, t| c.relay_pairings(t)).await,
        "cloud:relayUnpair" => {
            let id = text(&payload, "id");
            if !id.starts_with("pair_") || id.len() > 100 {
                return Some(Ok(super::fail("No such pairing.")));
            }
            authed(app, move |c, t| c.relay_unpair(t, &id).map(|_| Value::Null)).await
        }
        "cloud:relayRequestKey" | "cloud:relayRequestAnswer" => {
            let id = text(&payload, "id");
            if !id.starts_with("pair_") || id.len() > 100 {
                return Some(Ok(super::fail("No such pairing.")));
            }
            if channel == "cloud:relayRequestKey" {
                let key = text(&payload, "desktopKey");
                authed(app, move |c, t| {
                    c.relay_request_key(t, &id, &key).map(|_| Value::Null)
                })
                .await
            } else {
                let allow = payload
                    .get("allow")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                authed(app, move |c, t| {
                    c.relay_request_answer(t, &id, allow).map(|_| Value::Null)
                })
                .await
            }
        }
        "cloud:relayLoad" => match std::fs::read_to_string(relay_file()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
            Err(_) => Value::Null,
        },
        "cloud:relaySave" => {
            let data = payload.get("data").cloned().unwrap_or(Value::Null);
            let text = data.to_string();
            if !data.is_object() || text.len() > RELAY_FILE_MAX {
                return Some(Ok(super::fail("Invalid relay data.")));
            }
            match write_private(&relay_file(), &text) {
                Ok(()) => super::ok(),
                Err(e) => super::fail(format!("Couldn't save the relay keys: {e}")),
            }
        }
        "cloud:signOut" => {
            let session = cloud.session.lock().unwrap().clone();
            cloud.store(None);
            // Pairings belong to this device's session, which ends here.
            let _ = std::fs::remove_file(relay_file());
            if let Some(s) = session {
                // Ends this device on the server too; offline, the token just expires there.
                let task_app = app.clone();
                blocking(move || {
                    let cloud = task_app.state::<Cloud>();
                    let _ = cloud.client.revoke_device(&s.token, &s.device_id);
                })
                .await;
            }
            changed(app);
            super::ok()
        }
        "cloud:refresh" => {
            let Some(token) = cloud.token() else {
                return Some(Ok(cloud.status()));
            };
            let task_app = app.clone();
            let (token, me) = blocking(move || {
                let me = task_app.state::<Cloud>().client.me(&token);
                (token, me)
            })
            .await;
            match me {
                Ok(user) => {
                    cloud.update_if(&token, |s| s.user = user);
                    changed(app);
                }
                Err(e) if e.is_signed_out() => {
                    if cloud.update_if(&token, |_| {}) {
                        cloud.store(None);
                        changed(app);
                    }
                }
                Err(_) => {}
            }
            cloud.status()
        }
        _ => return None,
    }))
}
