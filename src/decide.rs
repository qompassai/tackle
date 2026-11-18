//! The write-class gate: a pure `decide()` over (tool, shape).
//!
//! Fleet pattern (D-113): consequential calls pass a deterministic
//! gate in server code, outside any model loop. For tackle the
//! classes are read (no gate), write (undo-grouped mutation), and
//! destructive (a write whose primary effect removes something).
//! The gate's rules for U2:
//!
//! - Every write or destructive call must carry a valid
//!   idempotency key; without one the call is refused before any
//!   editor traffic.
//! - A batch may contain only the six object/component/prefab
//!   writes: no nested batch, no scripts (the filesystem class is
//!   not undoable, so it can never join an undo group), no
//!   undo/redo, no scene save.
//! - Unknown tools are not classified here; dispatch refuses them
//!   before the gate is consulted.
//!
//! Pure and total: the same input always yields the same decision,
//! and every input yields one.

/// The capability class of one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Read,
    Write,
    Destructive,
}

/// Classify one tool by name. `None` for names tackle does not
/// serve at all.
pub fn classify(tool: &str) -> Option<Class> {
    match tool {
        "tackle_console"
        | "tackle_editor_state"
        | "tackle_gameobject_properties"
        | "tackle_project_info"
        | "tackle_scene_hierarchy"
        | "tackle_selection" => Some(Class::Read),
        "tackle_batch_execute"
        | "tackle_component_add"
        | "tackle_gameobject_create"
        | "tackle_gameobject_modify"
        | "tackle_prefab_instantiate"
        | "tackle_redo"
        | "tackle_scene_save"
        | "tackle_script_create"
        | "tackle_script_patch"
        | "tackle_undo" => Some(Class::Write),
        "tackle_component_remove" | "tackle_gameobject_delete" => Some(Class::Destructive),
        _ => None,
    }
}

/// Tools a batch may contain: the undo-groupable editor writes.
pub fn batchable(tool: &str) -> bool {
    matches!(
        tool,
        "tackle_component_add"
            | "tackle_component_remove"
            | "tackle_gameobject_create"
            | "tackle_gameobject_delete"
            | "tackle_gameobject_modify"
            | "tackle_prefab_instantiate"
    )
}

/// One gate verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Refuse(String),
}

/// Judge one already-classified call. `key` is the call's
/// idempotency key as presented (validation of its shape happens in
/// [`crate::idempotency::validate_key`]; the gate judges presence by
/// class).
pub fn decide(tool: &str, class: Class, key: Option<&str>) -> Decision {
    match class {
        Class::Read => Decision::Allow,
        Class::Write | Class::Destructive => match key {
            Some(key) if !key.is_empty() => Decision::Allow,
            _ => Decision::Refuse(format!(
                "{tool}: write-class calls require an idempotency_key"
            )),
        },
    }
}

/// Judge one batch member: it must be a batchable write tool.
pub fn decide_batch_member(tool: &str) -> Decision {
    match classify(tool) {
        Some(Class::Write | Class::Destructive) if batchable(tool) => Decision::Allow,
        Some(_) => Decision::Refuse(format!(
            "{tool}: not allowed inside tackle_batch_execute (only the six object, component and prefab writes are undo-groupable)"
        )),
        None => Decision::Refuse(format!("{tool}: unknown tool inside tackle_batch_execute")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_reads_need_no_key() {
        assert_eq!(decide("tackle_console", Class::Read, None), Decision::Allow);
    }

    #[test]
    fn ok_writes_with_key_allow() {
        assert_eq!(
            decide("tackle_undo", Class::Write, Some("k-1")),
            Decision::Allow
        );
        assert_eq!(
            decide("tackle_gameobject_delete", Class::Destructive, Some("k-2")),
            Decision::Allow
        );
    }

    #[test]
    fn adv_write_without_key_is_refused() {
        match decide("tackle_scene_save", Class::Write, None) {
            Decision::Refuse(message) => assert!(message.contains("idempotency_key")),
            other => panic!("expected refusal, got {other:?}"),
        }
        match decide("tackle_scene_save", Class::Write, Some("")) {
            Decision::Refuse(_) => {}
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn adv_batch_members_are_restricted_to_the_undoable_six() {
        assert_eq!(
            decide_batch_member("tackle_gameobject_create"),
            Decision::Allow
        );
        for tool in [
            "tackle_batch_execute",
            "tackle_script_create",
            "tackle_undo",
            "tackle_scene_save",
            "tackle_console",
            "tackle_nope",
        ] {
            match decide_batch_member(tool) {
                Decision::Refuse(_) => {}
                other => panic!("{tool}: expected refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn ok_every_served_tool_classifies() {
        for tool in [
            "tackle_batch_execute",
            "tackle_component_add",
            "tackle_component_remove",
            "tackle_console",
            "tackle_editor_state",
            "tackle_gameobject_create",
            "tackle_gameobject_delete",
            "tackle_gameobject_modify",
            "tackle_gameobject_properties",
            "tackle_prefab_instantiate",
            "tackle_project_info",
            "tackle_redo",
            "tackle_scene_hierarchy",
            "tackle_scene_save",
            "tackle_script_create",
            "tackle_script_patch",
            "tackle_selection",
            "tackle_undo",
        ] {
            assert!(classify(tool).is_some(), "{tool} must classify");
        }
        assert_eq!(classify("tackle_nope"), None);
    }
}
