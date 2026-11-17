//! Checks on renderer payloads, as the zod schemas of src/shared/ipc-contract.ts and
//! src/shared/schema.ts do. Each returns None when the field is missing or invalid.

use serde_json::Value;

/// `idSchema`: 1–64 of `[A-Za-z0-9_-]`.
pub fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn id(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| is_id(s))
        .map(str::to_string)
}

/// A string of `min..=max` UTF-16 code units (zod counts those).
pub fn string(v: &Value, key: &str, min: usize, max: usize) -> Option<String> {
    let s = v.get(key)?.as_str()?;
    let n = s.encode_utf16().count();
    (n >= min && n <= max).then(|| s.to_string())
}

/// An optional string: Some(None) when absent, None when present but invalid.
pub fn opt_string(v: &Value, key: &str, min: usize, max: usize) -> Option<Option<String>> {
    match v.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(_) => string(v, key, min, max).map(Some),
    }
}

/// A boolean with zod's `.default(d)`: absent is `d`, anything but a bool is invalid.
pub fn bool_or(v: &Value, key: &str, d: bool) -> Option<bool> {
    match v.get(key) {
        None | Some(Value::Null) => Some(d),
        Some(b) => b.as_bool(),
    }
}

/// A terminal dimension: an integer 1–1000.
pub fn dim(v: &Value, key: &str) -> Option<u16> {
    let n = v.get(key)?.as_f64()?;
    (n.fract() == 0.0 && (1.0..=1000.0).contains(&n)).then_some(n as u16)
}

/// Pane or CLI args: at most 32 strings of at most 1000 chars, no NUL.
pub fn arg_list(v: &Value) -> Option<Vec<String>> {
    let list = v.as_array()?;
    if list.len() > 32 {
        return None;
    }
    list.iter()
        .map(|a| {
            a.as_str()
                .filter(|s| s.encode_utf16().count() <= 1000 && !s.contains('\0'))
                .map(str::to_string)
        })
        .collect()
}

/// `sshHostSchema`.
pub fn is_ssh_host(s: &str) -> bool {
    let ok = |b: u8, first: bool| {
        b.is_ascii_alphanumeric() || b"_.@%:[]".contains(&b) || (!first && b == b'-')
    };
    !s.is_empty() && s.len() <= 255 && s.bytes().enumerate().all(|(i, b)| ok(b, i == 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn checks() {
        assert!(is_id("pane-1_A"));
        assert!(!is_id("a b"));
        assert!(!is_id(""));
        assert_eq!(dim(&json!({ "c": 80 }), "c"), Some(80));
        assert_eq!(dim(&json!({ "c": 0 }), "c"), None);
        assert_eq!(dim(&json!({ "c": 1.5 }), "c"), None);
        assert_eq!(bool_or(&json!({}), "b", true), Some(true));
        assert_eq!(bool_or(&json!({ "b": 1 }), "b", true), None);
        assert!(arg_list(&json!(["a", "b"])).is_some());
        assert!(arg_list(&json!(["a\0"])).is_none());
        assert!(is_ssh_host("me@box.local"));
        assert!(!is_ssh_host("-oProxyCommand=x"));
    }
}
