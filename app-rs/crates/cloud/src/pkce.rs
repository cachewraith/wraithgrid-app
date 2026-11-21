//! PKCE S256 (RFC 7636) and the `state` value of one sign-in.

use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct Pkce {
    /// Kept by the app; sent only to `/v1/desktop/token`.
    pub verifier: String,
    /// base64url(SHA-256(verifier)), sent to `/v1/desktop/authorize`.
    pub challenge: String,
    /// Echoed back on the redirect; a callback with another state is not ours.
    pub state: String,
}

impl Pkce {
    pub fn new() -> anyhow::Result<Self> {
        let verifier = b64url(&random::<32>()?); // 43 chars
        let state = b64url(&random::<24>()?); // 32 chars
        Ok(Self {
            challenge: challenge(&verifier),
            verifier,
            state,
        })
    }
}

pub fn challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

fn random<const N: usize>() -> anyhow::Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(buf)
}

/// base64url without padding.
pub fn b64url(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_base64url_sha256() {
        // Checked against Python's hashlib + base64.urlsafe_b64encode.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mJ92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "ngF5GsXcbwljx6u133FFr3Xht9xooA_DuaX_3QwODtc"
        );
    }

    #[test]
    fn b64url_matches_the_rfc_alphabet() {
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn fresh_values_fit_the_server_limits() {
        let p = Pkce::new().unwrap();
        assert_eq!(p.verifier.len(), 43);
        assert_eq!(p.challenge.len(), 43);
        assert!((16..=128).contains(&p.state.len()));
        assert_ne!(p.verifier, Pkce::new().unwrap().verifier);
    }
}
