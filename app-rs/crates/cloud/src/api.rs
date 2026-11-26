//! wraithgrid-server's desktop endpoints. Shapes follow wraithgrid-protocol; bodies the app
//! only passes on to the renderer (user, config) stay JSON values.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

pub const TIMEOUT: Duration = Duration::from_secs(20);
/// Sent on every request so the server keeps this device's version current.
pub const VERSION_HEADER: &str = "x-wraithgrid-version";

pub struct HttpRequest<'a> {
    pub method: &'a str,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Option<String>,
}

pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub type HttpFn = Box<dyn Fn(&HttpRequest<'_>) -> anyhow::Result<HttpResponse> + Send + Sync>;

/// A refused or failed call: the server's `ApiError`, or `network` when it couldn't be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl ApiError {
    /// The session is gone (expired, revoked, signed out elsewhere): sign out locally.
    pub fn is_signed_out(&self) -> bool {
        self.status == 401
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// `RegisterDeviceRequest`.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub platform: String,
    pub app_version: String,
}

/// What `/v1/desktop/token` answers (`DesktopTokenResponse`).
#[derive(Debug, Clone)]
pub struct TokenGrant {
    pub token: String,
    pub expires_at: String,
    pub device_id: String,
    pub user: Value,
}

#[derive(Deserialize)]
struct SessionBody {
    token: String,
    #[serde(rename = "expiresAt")]
    expires_at: String,
}

pub struct Client {
    pub server: String,
    pub app_version: String,
    http: HttpFn,
}

impl Client {
    pub fn new(server: String, app_version: String, http: HttpFn) -> Self {
        Self {
            server,
            app_version,
            http,
        }
    }

    pub fn live(server: String, app_version: String) -> Self {
        Self::new(server, app_version, Box::new(ureq_http))
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, ApiError> {
        let mut headers = vec![
            ("accept", "application/json".to_owned()),
            ("user-agent", format!("Wraithgrid/{}", self.app_version)),
            (VERSION_HEADER, self.app_version.clone()),
        ];
        if let Some(t) = token {
            headers.push(("authorization", format!("Bearer {t}")));
        }
        if body.is_some() {
            headers.push(("content-type", "application/json".to_owned()));
        }
        let req = HttpRequest {
            method,
            url: format!("{}{path}", self.server),
            headers,
            body: body.map(|b| b.to_string()),
        };
        let res = (self.http)(&req).map_err(|e| ApiError {
            status: 0,
            code: "network".into(),
            message: format!("Can't reach the Wraithgrid server ({e})"),
        })?;
        let parsed: Value = if res.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&res.body).unwrap_or(Value::Null)
        };
        if (200..300).contains(&res.status) {
            return Ok(parsed);
        }
        let err = &parsed["error"];
        Err(ApiError {
            status: res.status,
            code: err["code"].as_str().unwrap_or("http_error").to_owned(),
            message: err["message"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("The server answered {}", res.status)),
        })
    }

    /// `GET /v1/config` (public): minimum version, read-only mode, flags.
    pub fn config(&self) -> Result<Value, ApiError> {
        self.call("GET", "/v1/config", None, None)
    }

    /// Code + verifier → a session for this device.
    pub fn exchange(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        device: &DeviceInfo,
    ) -> Result<TokenGrant, ApiError> {
        let body = json!({
            "code": code,
            "codeVerifier": verifier,
            "redirectUri": redirect_uri,
            "device": device_json(device),
        });
        grant(self.call("POST", "/v1/desktop/token", None, Some(body))?)
    }

    /// Email + password inside the app → a session for this device (like [`Client::exchange`]).
    pub fn email_sign_in(
        &self,
        email: &str,
        password: &str,
        device: &DeviceInfo,
    ) -> Result<TokenGrant, ApiError> {
        let body = json!({
            "email": email,
            "password": password,
            "device": device_json(device),
        });
        grant(self.call("POST", "/v1/desktop/email/sign-in", None, Some(body))?)
    }

    /// Creates an account; the server emails a confirmation link (same answer for a taken email).
    pub fn email_sign_up(&self, name: &str, email: &str, password: &str) -> Result<(), ApiError> {
        let body = json!({ "name": name, "email": email, "password": password });
        self.call("POST", "/v1/desktop/email/sign-up", None, Some(body))
            .map(|_| ())
    }

    /// The emailed 6-digit code confirms the address and signs this device in.
    pub fn email_verify(
        &self,
        email: &str,
        otp: &str,
        device: &DeviceInfo,
    ) -> Result<TokenGrant, ApiError> {
        let body = json!({ "email": email, "otp": otp, "device": device_json(device) });
        grant(self.call("POST", "/v1/desktop/email/verify", None, Some(body))?)
    }

    /// A new confirmation code (same answer for unknown emails).
    pub fn email_resend(&self, email: &str) -> Result<(), ApiError> {
        let body = json!({ "email": email });
        self.call("POST", "/v1/desktop/email/resend", None, Some(body))
            .map(|_| ())
    }

    /// Is this reset code right? Doesn't use it up (wrong tries still count).
    pub fn email_reset_check(&self, email: &str, otp: &str) -> Result<(), ApiError> {
        let body = json!({ "email": email, "otp": otp });
        self.call("POST", "/v1/desktop/email/reset/check", None, Some(body))
            .map(|_| ())
    }

    /// Code + new password: every other device is signed out, this one signed in.
    pub fn email_reset_confirm(
        &self,
        email: &str,
        otp: &str,
        password: &str,
        device: &DeviceInfo,
    ) -> Result<TokenGrant, ApiError> {
        let body = json!({
            "email": email,
            "otp": otp,
            "password": password,
            "device": device_json(device),
        });
        grant(self.call("POST", "/v1/desktop/email/reset/confirm", None, Some(body))?)
    }

    /// `PATCH /v1/me` → the updated `User`.
    pub fn update_name(&self, token: &str, name: &str) -> Result<Value, ApiError> {
        let body = json!({ "name": name });
        Ok(self.call("PATCH", "/v1/me", Some(token), Some(body))?["user"].clone())
    }

    /// Needs the current password; the server signs every other device out.
    pub fn change_password(&self, token: &str, current: &str, new: &str) -> Result<(), ApiError> {
        let body = json!({ "currentPassword": current, "newPassword": new });
        self.call("POST", "/v1/me/password", Some(token), Some(body))
            .map(|_| ())
    }

    /// Emails a 6-digit code to set a new password, if the account exists.
    pub fn email_reset(&self, email: &str) -> Result<(), ApiError> {
        self.call(
            "POST",
            "/v1/desktop/email/reset",
            None,
            Some(json!({ "email": email })),
        )
        .map(|_| ())
    }

    /// A fresh token for this device; the old one stops working.
    pub fn refresh(&self, token: &str) -> Result<(String, String), ApiError> {
        let v = self.call("POST", "/v1/desktop/refresh", Some(token), None)?;
        let s: SessionBody = serde_json::from_value(v).map_err(bad_answer)?;
        Ok((s.token, s.expires_at))
    }

    /// `GET /v1/me` → the `User`.
    pub fn me(&self, token: &str) -> Result<Value, ApiError> {
        Ok(self.call("GET", "/v1/me", Some(token), None)?["user"].clone())
    }

    /// Revokes a device and its session (sign out).
    pub fn revoke_device(&self, token: &str, device_id: &str) -> Result<(), ApiError> {
        let path = format!("/v1/devices/{}", crate::encode(device_id));
        self.call("DELETE", &path, Some(token), None).map(|_| ())
    }

    /// `POST /v1/relay/pairings`: a pending pairing for a browser or phone, and its one-time code.
    pub fn relay_pair(&self, token: &str) -> Result<Value, ApiError> {
        self.call("POST", "/v1/relay/pairings", Some(token), Some(json!({})))
    }

    /// `GET /v1/relay/pairings`: this desktop's pairings.
    pub fn relay_pairings(&self, token: &str) -> Result<Value, ApiError> {
        self.call("GET", "/v1/relay/pairings", Some(token), None)
    }

    /// `DELETE /v1/relay/pairings/:id`.
    pub fn relay_unpair(&self, token: &str, id: &str) -> Result<(), ApiError> {
        let path = format!("/v1/relay/pairings/{}", crate::encode(id));
        self.call("DELETE", &path, Some(token), None).map(|_| ())
    }

    /// One-click pairing: this desktop's public key for a browser's request (shown there as the
    /// pairing code).
    pub fn relay_request_key(
        &self,
        token: &str,
        id: &str,
        desktop_key: &str,
    ) -> Result<(), ApiError> {
        let path = format!("/v1/relay/requests/{}/key", crate::encode(id));
        self.call(
            "POST",
            &path,
            Some(token),
            Some(json!({ "desktopKey": desktop_key })),
        )
        .map(|_| ())
    }

    /// One-click pairing: the user's Allow (`approve`) or Deny on this desktop.
    pub fn relay_request_answer(&self, token: &str, id: &str, allow: bool) -> Result<(), ApiError> {
        let answer = if allow { "approve" } else { "deny" };
        let path = format!("/v1/relay/requests/{}/{answer}", crate::encode(id));
        self.call("POST", &path, Some(token), Some(json!({})))
            .map(|_| ())
    }

    /// `POST /v1/relay/ticket`: a one-time ticket for the relay WebSocket.
    pub fn relay_ticket(&self, token: &str) -> Result<Value, ApiError> {
        self.call("POST", "/v1/relay/ticket", Some(token), Some(json!({})))
    }

    /// The relay WebSocket for a ticket: the server's URL with ws(s) instead of http(s).
    pub fn relay_socket_url(&self, ticket: &str) -> String {
        let base = self
            .server
            .strip_prefix("https://")
            .map(|r| format!("wss://{r}"))
            .or_else(|| {
                self.server
                    .strip_prefix("http://")
                    .map(|r| format!("ws://{r}"))
            })
            .unwrap_or_else(|| self.server.clone());
        format!("{base}/v1/relay/connect?ticket={}", crate::encode(ticket))
    }
}

fn device_json(device: &DeviceInfo) -> Value {
    json!({
        "name": device.name,
        "platform": device.platform,
        "appVersion": device.app_version,
    })
}

/// `DesktopTokenResponse` → what the app keeps.
fn grant(v: Value) -> Result<TokenGrant, ApiError> {
    let session: SessionBody = serde_json::from_value(v["session"].clone()).map_err(bad_answer)?;
    let device_id = v["device"]["id"]
        .as_str()
        .ok_or_else(|| bad_answer("device"))?;
    Ok(TokenGrant {
        token: session.token,
        expires_at: session.expires_at,
        device_id: device_id.to_owned(),
        user: v["user"].clone(),
    })
}

fn bad_answer(e: impl std::fmt::Display) -> ApiError {
    ApiError {
        status: 0,
        code: "bad_answer".into(),
        message: format!("The server's answer didn't make sense ({e})"),
    }
}

pub fn ureq_http(req: &HttpRequest<'_>) -> anyhow::Result<HttpResponse> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into();
    let mut res = match (req.method, &req.body) {
        (method @ ("POST" | "PATCH"), body) => {
            let mut r = if method == "PATCH" {
                agent.patch(&req.url)
            } else {
                agent.post(&req.url)
            };
            for (k, v) in &req.headers {
                r = r.header(*k, v.as_str());
            }
            match body {
                Some(b) => r.send(b.as_str())?,
                None => r.send_empty()?,
            }
        }
        (method, _) => {
            let mut r = match method {
                "DELETE" => agent.delete(&req.url),
                _ => agent.get(&req.url),
            };
            for (k, v) in &req.headers {
                r = r.header(*k, v.as_str());
            }
            r.call()?
        }
    };
    let status = res.status().as_u16();
    let body = res.body_mut().read_to_string()?;
    Ok(HttpResponse { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<(String, String, Vec<(&'static str, String)>, Option<String>)>>>;

    fn client(status: u16, body: &'static str) -> (Client, Seen) {
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let http: HttpFn = Box::new(move |r| {
            log.lock().unwrap().push((
                r.method.to_owned(),
                r.url.clone(),
                r.headers.clone(),
                r.body.clone(),
            ));
            Ok(HttpResponse {
                status,
                body: body.to_owned(),
            })
        });
        (Client::new("http://srv".into(), "2.3.0".into(), http), seen)
    }

    #[test]
    fn exchange_sends_code_verifier_and_device() {
        let (c, seen) = client(
            200,
            r#"{"session":{"token":"tok","expiresAt":"2026-12-20T00:00:00.000Z"},"device":{"id":"dev_1"},"user":{"email":"a@b.dev"}}"#,
        );
        let device = DeviceInfo {
            name: "thinkpad".into(),
            platform: "linux".into(),
            app_version: "2.3.0".into(),
        };
        let g = c
            .exchange("code", "verifier", "http://127.0.0.1:1/callback", &device)
            .unwrap();
        assert_eq!(g.token, "tok");
        assert_eq!(g.device_id, "dev_1");
        assert_eq!(g.user["email"], "a@b.dev");
        let seen = seen.lock().unwrap();
        let (method, url, headers, body) = &seen[0];
        assert_eq!(
            (method.as_str(), url.as_str()),
            ("POST", "http://srv/v1/desktop/token")
        );
        assert!(headers.contains(&(VERSION_HEADER, "2.3.0".into())));
        assert!(!headers.iter().any(|(k, _)| *k == "authorization"));
        let body: Value = serde_json::from_str(body.as_ref().unwrap()).unwrap();
        assert_eq!(body["codeVerifier"], "verifier");
        assert_eq!(body["device"]["appVersion"], "2.3.0");
    }

    #[test]
    fn email_sign_in_sends_credentials_and_device_without_a_token() {
        let (c, seen) = client(
            200,
            r#"{"session":{"token":"tok","expiresAt":"2026-12-20T00:00:00.000Z"},"device":{"id":"dev_2"},"user":{"email":"a@b.dev"}}"#,
        );
        let device = DeviceInfo {
            name: "thinkpad".into(),
            platform: "linux".into(),
            app_version: "2.3.0".into(),
        };
        let g = c.email_sign_in("a@b.dev", "pw", &device).unwrap();
        assert_eq!((g.token.as_str(), g.device_id.as_str()), ("tok", "dev_2"));
        let seen = seen.lock().unwrap();
        let (_, url, headers, body) = &seen[0];
        assert_eq!(url, "http://srv/v1/desktop/email/sign-in");
        assert!(!headers.iter().any(|(k, _)| *k == "authorization"));
        let body: Value = serde_json::from_str(body.as_ref().unwrap()).unwrap();
        assert_eq!(body["password"], "pw");
        assert_eq!(body["device"]["name"], "thinkpad");
    }

    #[test]
    fn verify_and_reset_confirm_send_the_code_and_device() {
        let ok = r#"{"session":{"token":"tok","expiresAt":"2026-12-20T00:00:00.000Z"},"device":{"id":"dev_3"},"user":{"email":"a@b.dev"}}"#;
        let device = DeviceInfo {
            name: "thinkpad".into(),
            platform: "linux".into(),
            app_version: "2.3.0".into(),
        };
        let (c, seen) = client(200, ok);
        assert_eq!(
            c.email_verify("a@b.dev", "012345", &device)
                .unwrap()
                .device_id,
            "dev_3"
        );
        c.email_reset_confirm("a@b.dev", "012345", "new password!", &device)
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].1, "http://srv/v1/desktop/email/verify");
        assert_eq!(seen[1].1, "http://srv/v1/desktop/email/reset/confirm");
        let body: Value = serde_json::from_str(seen[1].3.as_ref().unwrap()).unwrap();
        assert_eq!(
            (body["otp"].as_str(), body["password"].as_str()),
            (Some("012345"), Some("new password!"))
        );
        assert_eq!(body["device"]["platform"], "linux");
    }

    #[test]
    fn reset_check_sends_email_and_code_only() {
        let (c, seen) = client(200, r#"{"valid":true}"#);
        c.email_reset_check("a@b.dev", "012345").unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].1, "http://srv/v1/desktop/email/reset/check");
        let body: Value = serde_json::from_str(seen[0].3.as_ref().unwrap()).unwrap();
        assert_eq!(body, json!({ "email": "a@b.dev", "otp": "012345" }));
    }

    #[test]
    fn profile_calls_use_the_token() {
        let (c, seen) = client(200, r#"{"user":{"name":"Mara"}}"#);
        assert_eq!(c.update_name("tok", "Mara").unwrap()["name"], "Mara");
        let (c2, seen2) = client(204, "");
        c2.change_password("tok", "old", "a brand new one").unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            (seen[0].0.as_str(), seen[0].1.as_str()),
            ("PATCH", "http://srv/v1/me")
        );
        let seen2 = seen2.lock().unwrap();
        assert_eq!(seen2[0].1, "http://srv/v1/me/password");
        assert!(seen2[0].2.contains(&("authorization", "Bearer tok".into())));
    }

    #[test]
    fn relay_socket_url_swaps_the_scheme_and_encodes_the_ticket() {
        let c = |server: &str| Client::new(server.into(), "1".into(), Box::new(|_| unreachable!()));
        assert_eq!(
            c("https://api.example.com").relay_socket_url("a b"),
            "wss://api.example.com/v1/relay/connect?ticket=a%20b"
        );
        assert_eq!(
            c("http://localhost:3000").relay_socket_url("t"),
            "ws://localhost:3000/v1/relay/connect?ticket=t"
        );
    }

    #[test]
    fn signed_in_calls_send_the_bearer_token() {
        let (c, seen) = client(200, r#"{"user":{"id":"usr_1"}}"#);
        assert_eq!(c.me("tok").unwrap()["id"], "usr_1");
        let seen = seen.lock().unwrap();
        assert!(seen[0].2.contains(&("authorization", "Bearer tok".into())));
    }

    #[test]
    fn server_errors_keep_code_and_message() {
        let (c, _) = client(
            426,
            r#"{"error":{"code":"upgrade_required","message":"Update Wraithgrid to 2.4.0 or later"}}"#,
        );
        let device = DeviceInfo {
            name: "x".into(),
            platform: "linux".into(),
            app_version: "2.3.0".into(),
        };
        let e = c.exchange("c", "v", "r", &device).unwrap_err();
        assert_eq!(e.code, "upgrade_required");
        assert_eq!(e.message, "Update Wraithgrid to 2.4.0 or later");
        assert!(!e.is_signed_out());
        let (c, _) = client(401, "");
        assert!(c.me("tok").unwrap_err().is_signed_out());
    }

    #[test]
    fn unreachable_server_is_a_network_error() {
        let c = Client::new(
            "http://srv".into(),
            "2.3.0".into(),
            Box::new(|_| anyhow::bail!("refused")),
        );
        let e = c.config().unwrap_err();
        assert_eq!((e.status, e.code.as_str()), (0, "network"));
    }
}
