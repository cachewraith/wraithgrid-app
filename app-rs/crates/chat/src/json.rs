//! Small readers for untrusted JSON (a CLI's output, a saved view), with the TS adapters'
//! forgiving semantics: a missing or mistyped field reads as empty rather than failing.

use serde_json::{Map, Value};

/// The value as an object, if it is one.
pub(crate) fn obj(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object()
}

/// A field of an object, or Null.
pub(crate) fn get<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

/// A string, or '' for anything else (`str()` in the TS).
pub(crate) fn str_of(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// A field as a string, or ''.
pub(crate) fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    str_of(get(v, key))
}

/// The object elements of an array field (non-objects dropped); empty for a non-array.
pub(crate) fn objects<'a>(v: &'a Value, key: &str) -> Vec<&'a Value> {
    match get(v, key) {
        Value::Array(list) => list.iter().filter(|x| x.is_object()).collect(),
        _ => Vec::new(),
    }
}

/// JS truthiness.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// JS `String(v)` for the scalars an index can be; a missing value is "undefined".
pub(crate) fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        Some(other) => other.to_string(),
    }
}

/// The first line of a string.
pub(crate) fn first_line(s: &str) -> &str {
    s.split('\n').next().unwrap_or("")
}

/// Milliseconds since the Unix epoch (`Date.now()`).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A short random-enough token for ids that must differ between processes (the TS used
/// `Math.random()`); std's hasher keys are seeded randomly per process.
pub(crate) fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(now_ms());
    to_base36(h.finish() % 36u64.pow(4))
}

pub(crate) fn to_base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}
