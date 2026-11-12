//! Short random ids that satisfy the config schema's id pattern (`src/renderer/lib/ids.ts`):
//! `<prefix>-<10 hex chars>`, like `p-3f9a0c12bd`.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

pub fn new_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    format!("{prefix}-{:010x}", h.finish() & 0xff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_are_valid_and_distinct() {
        let a = super::new_id("p");
        assert!(crate::config::is_id(&a) && a.len() == 12 && a.starts_with("p-"));
        assert_ne!(a, super::new_id("p"));
    }
}
