//! Bounding for editor payloads before they reach a screen.
//!
//! Editor replies are external data: the TUI never renders them
//! unbounded and never trusts their shape. [`validate_payload`]
//! enforces named maxima — oversized strings and arrays are
//! truncated with a visible marker (the operator still sees the
//! data's head), while wrong top-level shapes and runaway nesting
//! are refused outright. Truncation is display hygiene only; the
//! facade's recorded call is untouched.

use serde_json::{Map, Value};

/// Largest serialized editor payload accepted, in bytes (the
/// bridge's own frame ceiling is 1 MiB of editor JSON).
pub const PAYLOAD_BYTES_MAX: usize = 1024 * 1024;

/// Deepest JSON nesting rendered; deeper is refused, not walked
/// (no recursion over attacker-shaped depth without a cap).
pub const DEPTH_MAX: usize = 16;

/// Most items kept from any one array.
pub const ARRAY_ITEMS_MAX: usize = 500;

/// Most keys kept from any one object.
pub const OBJECT_KEYS_MAX: usize = 128;

/// Most characters kept from any one string.
pub const TEXT_CHARS_MAX: usize = 4096;

/// Suffix marking a truncated string.
pub const TRUNCATED_SUFFIX: &str = "…[truncated]";

/// Validate and bound one tool result for display.
///
/// Returns the bounded value, or an error string naming the shape
/// problem. Read tools whose payload must be an object
/// (`tackle_editor_state`, `tackle_scene_hierarchy`,
/// `tackle_gameobject_properties`, `tackle_project_info`) refuse
/// non-object payloads: a hierarchy that is not an object is not a
/// hierarchy, and presenting it as one would be a lie.
pub fn validate_payload(tool: &str, value: &Value) -> Result<Value, String> {
    let serialized = serde_json::to_string(value).unwrap_or_default();
    if serialized.len() > PAYLOAD_BYTES_MAX {
        return Err(format!(
            "{tool}: payload of {} bytes exceeds {PAYLOAD_BYTES_MAX}",
            serialized.len()
        ));
    }
    if requires_object(tool) && !value.is_object() {
        return Err(format!(
            "{tool}: payload must be a JSON object, got {}",
            kind_name(value)
        ));
    }
    bound(value, 0)
}

fn requires_object(tool: &str) -> bool {
    matches!(
        tool,
        "tackle_editor_state"
            | "tackle_scene_hierarchy"
            | "tackle_gameobject_properties"
            | "tackle_project_info"
    )
}

fn kind_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn bound(value: &Value, depth: usize) -> Result<Value, String> {
    if depth > DEPTH_MAX {
        return Err(format!("payload nesting exceeds depth {DEPTH_MAX}"));
    }
    match value {
        Value::String(text) => Ok(Value::String(truncate_text(text))),
        Value::Array(items) => {
            let mut kept: Vec<Value> = Vec::new();
            for item in items.iter().take(ARRAY_ITEMS_MAX) {
                kept.push(bound(item, depth + 1)?);
            }
            if items.len() > ARRAY_ITEMS_MAX {
                kept.push(Value::String(format!(
                    "…[{} more items truncated]",
                    items.len() - ARRAY_ITEMS_MAX
                )));
            }
            Ok(Value::Array(kept))
        }
        Value::Object(map) => {
            let mut kept = Map::new();
            for (key, item) in map.iter().take(OBJECT_KEYS_MAX) {
                kept.insert(truncate_text(key), bound(item, depth + 1)?);
            }
            if map.len() > OBJECT_KEYS_MAX {
                kept.insert(
                    "…[truncated]".to_string(),
                    Value::String(format!("{} keys omitted", map.len() - OBJECT_KEYS_MAX)),
                );
            }
            Ok(Value::Object(kept))
        }
        other => Ok(other.clone()),
    }
}

fn truncate_text(text: &str) -> String {
    if text.chars().count() <= TEXT_CHARS_MAX {
        return text.to_string();
    }
    let mut kept: String = text.chars().take(TEXT_CHARS_MAX).collect();
    kept.push_str(TRUNCATED_SUFFIX);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ok_small_object_passes_unchanged() {
        let value = json!({"scene": "Main", "totalCount": 2});
        assert_eq!(
            validate_payload("tackle_scene_hierarchy", &value).expect("ok"),
            value
        );
    }

    #[test]
    fn adv_non_object_hierarchy_is_refused() {
        let value = json!(["not", "an", "object"]);
        assert!(validate_payload("tackle_scene_hierarchy", &value).is_err());
        assert!(validate_payload("tackle_editor_state", &json!(42)).is_err());
        assert!(validate_payload("tackle_editor_state", &Value::Null).is_err());
    }

    #[test]
    fn adv_oversized_string_is_truncated_with_marker() {
        let big = "x".repeat(TEXT_CHARS_MAX + 100);
        let out = validate_payload("tackle_console", &json!({"m": big})).expect("ok");
        let text = out["m"].as_str().expect("string");
        assert!(text.ends_with(TRUNCATED_SUFFIX));
        assert!(text.chars().count() <= TEXT_CHARS_MAX + TRUNCATED_SUFFIX.len());
    }

    #[test]
    fn adv_huge_array_is_truncated_to_cap() {
        let items: Vec<Value> = (0..(ARRAY_ITEMS_MAX + 50)).map(Value::from).collect();
        let out = validate_payload("tackle_console", &json!({"entries": items})).expect("ok");
        let kept = out["entries"].as_array().expect("array");
        // Cap plus one marker entry.
        assert_eq!(kept.len(), ARRAY_ITEMS_MAX + 1);
    }

    #[test]
    fn adv_deep_nesting_is_refused() {
        let mut value = json!(1);
        for _ in 0..(DEPTH_MAX + 4) {
            value = json!({"n": value});
        }
        assert!(validate_payload("tackle_console", &value).is_err());
    }

    #[test]
    fn adv_oversized_payload_is_refused() {
        let big = "y".repeat(PAYLOAD_BYTES_MAX + 1);
        assert!(validate_payload("tackle_console", &json!(big)).is_err());
    }
}
