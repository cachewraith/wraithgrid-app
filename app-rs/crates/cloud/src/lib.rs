//! The Wraithgrid account: signing the desktop in to wraithgrid-server and calling it. A pure
//! library: the app injects HTTP and drives the browser and timers itself.
//!
//! Sign-in is RFC 8252 (loopback redirect + PKCE): the app listens on 127.0.0.1, opens the
//! browser at `/v1/desktop/authorize`, the server sends the browser back with a one-time code,
//! and the app trades code + verifier for a bearer session bound to this computer.

pub mod api;
pub mod loopback;
pub mod pkce;
pub mod session;

pub use api::{ApiError, Client, DeviceInfo, HttpFn, HttpRequest, HttpResponse, TokenGrant};
pub use loopback::{Callback, Listener};
pub use pkce::Pkce;
pub use session::{install_id, write_private, SavedSession, INSTALL_ID_FILE, SESSION_FILE};

/// The production server; `WRAITHGRID_SERVER_URL` replaces it (dev: `http://localhost:3000`).
pub const DEFAULT_SERVER: &str = "https://api.wraithgrid.cachewraith.com";

/// Providers the browser sign-in (`/v1/desktop/authorize`) accepts (`OAuthProvider` in
/// wraithgrid-protocol); email + password happens in the app.
pub const PROVIDERS: [&str; 2] = ["google", "github"];

/// The server this app talks to, without a trailing slash.
pub fn server_url(env_value: Option<&str>) -> String {
    let url = env_value
        .map(str::trim)
        .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
        .unwrap_or(DEFAULT_SERVER);
    url.trim_end_matches('/').to_owned()
}

/// The browser URL that starts a sign-in.
pub fn authorize_url(server: &str, provider: &str, redirect_uri: &str, pkce: &Pkce) -> String {
    format!(
        "{server}/v1/desktop/authorize?provider={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}",
        encode(provider),
        encode(redirect_uri),
        encode(&pkce.challenge),
        encode(&pkce.state)
    )
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Reverses [`encode`] (and `+` as space, as forms send it). Bad escapes stay as they are.
pub fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 3 <= bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                        continue;
                    }
                    _ => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `linux` | `macos` | `windows` (`Platform` in wraithgrid-protocol).
pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

/// This computer's name for the devices list (1–64 chars).
pub fn device_name() -> String {
    let from_env = std::env::var("COMPUTERNAME").ok();
    let from_proc = || std::fs::read_to_string("/proc/sys/kernel/hostname").ok();
    let from_cmd = || {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let name = from_env
        .or_else(from_proc)
        .or_else(from_cmd)
        .unwrap_or_default();
    clean_name(&name)
}

fn clean_name(raw: &str) -> String {
    let name: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    if name.is_empty() {
        "Wraithgrid desktop".to_owned()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_from_env_or_default() {
        assert_eq!(server_url(None), DEFAULT_SERVER);
        assert_eq!(
            server_url(Some("http://localhost:3000/")),
            "http://localhost:3000"
        );
        assert_eq!(server_url(Some("  ")), DEFAULT_SERVER);
        assert_eq!(server_url(Some("file:///etc")), DEFAULT_SERVER);
    }

    #[test]
    fn authorize_url_carries_every_parameter_encoded() {
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM".into(),
            state: "s_tate-0123456789".into(),
        };
        let url = authorize_url(
            "http://localhost:3000",
            "github",
            "http://127.0.0.1:5000/callback",
            &pkce,
        );
        assert_eq!(
            url,
            "http://localhost:3000/v1/desktop/authorize?provider=github&redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Fcallback&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&state=s_tate-0123456789"
        );
    }

    #[test]
    fn decode_reverses_encode() {
        let s = "a b/ç?&=%";
        assert_eq!(decode(&encode(s)), s);
        assert_eq!(decode("a+b%2"), "a b%2");
    }

    #[test]
    fn device_names_are_cleaned() {
        assert_eq!(clean_name("  thinkpad\n"), "thinkpad");
        assert_eq!(clean_name(""), "Wraithgrid desktop");
        assert_eq!(clean_name(&"x".repeat(100)).len(), 64);
    }
}
