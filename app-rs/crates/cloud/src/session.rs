//! The signed-in session, kept in `cloud-session.json` in the user-data dir (owner-only file,
//! like the agent CLIs keep their own tokens). Removed on sign-out.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SESSION_FILE: &str = "cloud-session.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSession {
    /// The server the token belongs to; a session for another server is ignored.
    pub server: String,
    pub token: String,
    /// ISO 8601, as the server sent it.
    pub expires_at: String,
    pub device_id: String,
    /// The last `User` the server sent.
    pub user: Value,
}

impl SavedSession {
    /// The saved session for `server`, or None (missing, unreadable, another server).
    pub fn load(file: &Path, server: &str) -> Option<Self> {
        let s: Self = serde_json::from_str(&fs::read_to_string(file).ok()?).ok()?;
        (s.server == server && !s.token.is_empty()).then_some(s)
    }

    pub fn save(&self, file: &Path) -> io::Result<()> {
        let data = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        write_private(file, &data)
    }

    pub fn remove(file: &Path) {
        let _ = fs::remove_file(file);
    }
}

/// Writes a file only its owner can read (0600 on Unix), replacing it in one step: the
/// session token, and the relay's keys (relay.json).
pub fn write_private(file: &Path, data: &str) -> io::Result<()> {
    let tmp = file.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    {
        let mut f = opts.open(&tmp)?;
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, file)
}

/// The file holding this install's random id.
pub const INSTALL_ID_FILE: &str = "install-id";

/// This install's id (made once, kept in the user-data folder): the server reuses the same
/// device when this install signs in again, instead of adding another. None when it can't be
/// read or written (the server then makes a device, as before).
pub fn install_id(dir: &Path) -> Option<String> {
    let file = dir.join(INSTALL_ID_FILE);
    if let Ok(text) = fs::read_to_string(&file) {
        let id = text.trim();
        if (16..=64).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Some(id.to_owned());
        }
    }
    let mut buf = [0u8; 18];
    getrandom::fill(&mut buf).ok()?;
    let id = crate::pkce::b64url(&buf); // 24 chars
    fs::create_dir_all(dir).ok()?;
    write_private(&file, &id).ok()?;
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_owner_only_and_loads_for_the_same_server() {
        let dir = std::env::temp_dir().join(format!("wg-cloud-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(SESSION_FILE);
        let s = SavedSession {
            server: "http://srv".into(),
            token: "tok".into(),
            expires_at: "2026-12-20T00:00:00.000Z".into(),
            device_id: "dev_1".into(),
            user: serde_json::json!({ "email": "a@b.dev" }),
        };
        s.save(&file).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(SavedSession::load(&file, "http://srv"), Some(s));
        assert_eq!(SavedSession::load(&file, "http://other"), None);
        SavedSession::remove(&file);
        assert_eq!(SavedSession::load(&file, "http://srv"), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn install_id_is_made_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("wg-install-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let a = install_id(&dir).unwrap();
        assert_eq!(a.len(), 24);
        assert_eq!(install_id(&dir).unwrap(), a);
        // A damaged file gets a fresh id.
        fs::write(dir.join(INSTALL_ID_FILE), "bad id").unwrap();
        let b = install_id(&dir).unwrap();
        assert_ne!(b, a);
        let _ = fs::remove_dir_all(&dir);
    }
}
