//! Server-side idempotency for write-class calls (PROGRAM core
//! contract: every mutating call carries a client-supplied
//! idempotency key; applied keys are recorded; a retry returns the
//! original result).
//!
//! Scope: one server run is one idempotency session — the store
//! lives in memory beside the per-run capability token and dies
//! with the process, exactly as the token does. Only successful
//! results are recorded: a failed call may be retried under the
//! same key with corrected arguments. Reusing a key with
//! *different* arguments is a typed refusal, never a silent replay.
//! The store is bounded (FIFO eviction at [`STORE_CAP`]).

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{Map, Value};

/// Keys recorded at most; the oldest are evicted first.
pub const STORE_CAP: usize = 512;
/// Longest accepted key, in characters.
pub const KEY_MAX: usize = 128;

/// Validate one presented key: 1..=128 chars from
/// `[A-Za-z0-9._:-]`. Returns the refusal message on failure.
pub fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.chars().count() > KEY_MAX {
        return Err(format!("idempotency_key must be 1..={KEY_MAX} characters"));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    {
        return Err("idempotency_key may only contain [A-Za-z0-9._:-] characters".to_string());
    }
    Ok(())
}

/// A canonical fingerprint of one call: the tool name plus the
/// arguments with the key itself removed, serialized canonically
/// (serde_json maps order keys deterministically in this build) and
/// reduced with FNV-1a so stored entries stay small even for
/// 64 KiB script bodies.
pub fn fingerprint(tool: &str, arguments: &Map<String, Value>) -> String {
    let mut stripped = arguments.clone();
    stripped.remove("idempotency_key");
    let canonical = Value::Object(stripped).to_string();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{tool}:{hash:016x}:{}", canonical.len())
}

struct Entry {
    key: String,
    fingerprint: String,
    result_text: String,
}

/// What a lookup found.
#[derive(Debug, PartialEq)]
pub enum Lookup {
    /// Never applied: execute the call.
    Miss,
    /// Applied with identical arguments: replay this result text.
    Hit(String),
    /// Applied with different arguments: refuse.
    Conflict,
}

#[derive(Default)]
pub struct Store {
    entries: Mutex<VecDeque<Entry>>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup(&self, key: &str, fingerprint: &str) -> Lookup {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match entries.iter().find(|entry| entry.key == key) {
            None => Lookup::Miss,
            Some(entry) if entry.fingerprint == fingerprint => {
                Lookup::Hit(entry.result_text.clone())
            }
            Some(_) => Lookup::Conflict,
        }
    }

    pub fn record(&self, key: &str, fingerprint: String, result_text: String) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.iter().any(|entry| entry.key == key) {
            return;
        }
        if entries.len() >= STORE_CAP {
            entries.pop_front();
        }
        entries.push_back(Entry {
            key: key.to_string(),
            fingerprint,
            result_text,
        });
    }

    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    #[test]
    fn ok_key_shapes() {
        assert!(validate_key("create-seraphine-1").is_ok());
        assert!(validate_key("a.b:c_d").is_ok());
        assert!(validate_key(&"x".repeat(KEY_MAX)).is_ok());
    }

    #[test]
    fn adv_key_shapes_refused() {
        assert!(validate_key("").is_err());
        assert!(validate_key(&"x".repeat(KEY_MAX + 1)).is_err());
        assert!(validate_key("has space").is_err());
        assert!(validate_key("slash/key").is_err());
        assert!(validate_key("unicode-é").is_err());
    }

    #[test]
    fn ok_miss_hit_conflict_cycle() {
        let store = Store::new();
        let fp = fingerprint("tackle_undo", &args(&[("idempotency_key", json!("k1"))]));
        assert_eq!(store.lookup("k1", &fp), Lookup::Miss);
        store.record("k1", fp.clone(), "{\"undone\":true}".to_string());
        assert_eq!(
            store.lookup("k1", &fp),
            Lookup::Hit("{\"undone\":true}".to_string())
        );
        let other = fingerprint("tackle_redo", &args(&[("idempotency_key", json!("k1"))]));
        assert_eq!(store.lookup("k1", &other), Lookup::Conflict);
    }

    #[test]
    fn ok_fingerprint_ignores_the_key_itself() {
        let a = fingerprint("tackle_undo", &args(&[("idempotency_key", json!("k1"))]));
        let b = fingerprint("tackle_undo", &args(&[("idempotency_key", json!("k2"))]));
        assert_eq!(a, b);
    }

    #[test]
    fn adv_store_is_bounded_fifo() {
        let store = Store::new();
        for index in 0..(STORE_CAP + 5) {
            store.record(
                &format!("key-{index}"),
                format!("fp-{index}"),
                "result".to_string(),
            );
        }
        assert_eq!(store.len(), STORE_CAP);
        assert_eq!(store.lookup("key-0", "fp-0"), Lookup::Miss);
        assert!(matches!(store.lookup("key-509", "fp-509"), Lookup::Hit(_)));
    }
}
