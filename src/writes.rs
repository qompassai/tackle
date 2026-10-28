//! The U2 write core's MCP-layer half: argument validation,
//! decide() gating, idempotency, batch orchestration, and the
//! bounded-filesystem script tools.
//!
//! Every write call flows: classify → key shape → decide() →
//! idempotency lookup → execute → record (successes only). Editor
//! writes are planned as (command, args) pairs by pure validators
//! shared between the single tools and batch members, so a batch
//! validates every member before the editor is touched. Batch
//! execution is orchestrated here: `batch_begin` opens one editor
//! undo group, members run with `joinGroup`, the first failure
//! triggers `batch_revert`, success ends in `batch_commit`
//! (collapse to a single undo step).
//!
//! Script create/patch are the PROGRAM's documented filesystem
//! exception class: filesystem writes cannot join the editor undo
//! stack, so they are bounded instead — project-relative `.cs`
//! paths under `Assets/`, size caps, exact-match patching — and
//! idempotency-keyed like every other write. After a write the
//! editor recompiles; the result reports the compile outcome and
//! any `error CS` diagnostics read back from the console.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::bridge::{Bridge, CallError};
use crate::decide::{self, Class, Decision};
use crate::idempotency::{self, Lookup, Store};
use crate::log;

/// Longest script body accepted, in bytes.
pub const SCRIPT_BYTES_MAX: usize = 64 * 1024;
/// Longest script file the patch tool will read, in bytes.
pub const SCRIPT_READ_MAX: u64 = 256 * 1024;
/// Longest object name accepted, in bytes.
pub const NAME_BYTES_MAX: usize = 256;
/// Most operations in one batch.
pub const BATCH_OPS_MAX: usize = 32;
/// How long a compile may take to settle before it is reported as
/// not compiled.
pub const COMPILE_DEADLINE: Duration = Duration::from_secs(120);

/// One validated editor write: the bridge command and its args.
struct Planned {
    command: &'static str,
    args: Value,
}

fn editor_path_arg(arguments: &Map<String, Value>, key: &str) -> Result<String, String> {
    let value = arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} is required and must be a string"))?;
    Ok(value.to_string())
}

/// The U1 hierarchy-path shape, reused for write targets.
fn valid_hierarchy_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 1024 {
        return Err("path must be non-empty and at most 1024 bytes".to_string());
    }
    if path.split('/').any(str::is_empty) {
        return Err("path must not contain empty segments".to_string());
    }
    Ok(())
}

fn valid_object_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > NAME_BYTES_MAX {
        return Err(format!(
            "name must be non-empty and at most {NAME_BYTES_MAX} bytes"
        ));
    }
    if name.contains('/') {
        return Err("name must not contain `/` (it is one hierarchy segment)".to_string());
    }
    Ok(())
}

fn optional_string(arguments: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// One optional vector argument: the served schema's `[x, y, z]`
/// array form (an `{x, y, z}` object is also accepted), normalized
/// to the object shape the editor's JsonUtility Vector3 expects.
fn vec3(arguments: &Map<String, Value>, key: &str) -> Result<Option<Value>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            if items.len() != 3 || !items.iter().all(Value::is_number) {
                return Err(format!("{key} must be [x, y, z] numbers"));
            }
            Ok(Some(json!({
                "x": items[0], "y": items[1], "z": items[2],
            })))
        }
        Some(value @ Value::Object(map)) => {
            for axis in ["x", "y", "z"] {
                if !map.get(axis).is_some_and(Value::is_number) {
                    return Err(format!("{key}.{axis} must be a number"));
                }
            }
            Ok(Some(value.clone()))
        }
        Some(_) => Err(format!("{key} must be [x, y, z] numbers")),
    }
}

/// Plan one of the six undo-groupable editor writes. Pure: no
/// bridge traffic, so batches validate every member up front.
fn plan_write(
    tool: &str,
    arguments: &Map<String, Value>,
    join_group: bool,
) -> Result<Planned, String> {
    let mut args = Map::new();
    if join_group {
        args.insert("joinGroup".to_string(), json!(true));
    }
    let planned = match tool {
        "tackle_component_add" | "tackle_component_remove" => {
            let path = editor_path_arg(arguments, "path")?;
            valid_hierarchy_path(&path)?;
            let component_type = editor_path_arg(arguments, "component_type")?;
            if component_type.is_empty() || component_type.len() > 512 {
                return Err("component_type must be non-empty and at most 512 bytes".to_string());
            }
            args.insert("path".to_string(), json!(path));
            args.insert("componentType".to_string(), json!(component_type));
            Planned {
                command: if tool == "tackle_component_add" {
                    "add_component"
                } else {
                    "remove_component"
                },
                args: Value::Object(args),
            }
        }
        "tackle_gameobject_create" => {
            let name = editor_path_arg(arguments, "name")?;
            valid_object_name(&name)?;
            args.insert("name".to_string(), json!(name));
            if let Some(parent) = optional_string(arguments, "parent_path")? {
                valid_hierarchy_path(&parent)?;
                args.insert("parentPath".to_string(), json!(parent));
            }
            Planned {
                command: "create_gameobject",
                args: Value::Object(args),
            }
        }
        "tackle_gameobject_delete" => {
            let path = editor_path_arg(arguments, "path")?;
            valid_hierarchy_path(&path)?;
            args.insert("path".to_string(), json!(path));
            Planned {
                command: "delete_gameobject",
                args: Value::Object(args),
            }
        }
        "tackle_gameobject_modify" => {
            let path = editor_path_arg(arguments, "path")?;
            valid_hierarchy_path(&path)?;
            args.insert("path".to_string(), json!(path));
            let mut changes = 0;
            if let Some(active) = arguments.get("active") {
                if !active.is_boolean() {
                    return Err("active must be a boolean".to_string());
                }
                args.insert("hasActive".to_string(), json!(true));
                args.insert("active".to_string(), active.clone());
                changes += 1;
            }
            if let Some(name) = optional_string(arguments, "name")? {
                valid_object_name(&name)?;
                args.insert("newName".to_string(), json!(name));
                changes += 1;
            }
            if let Some(tag) = optional_string(arguments, "tag")? {
                if tag.is_empty() || tag.len() > 128 {
                    return Err("tag must be non-empty and at most 128 bytes".to_string());
                }
                args.insert("tag".to_string(), json!(tag));
                changes += 1;
            }
            if let Some(layer) = optional_string(arguments, "layer_name")? {
                if layer.is_empty() || layer.len() > 128 {
                    return Err("layer_name must be non-empty and at most 128 bytes".to_string());
                }
                args.insert("layerName".to_string(), json!(layer));
                changes += 1;
            }
            for (key, flag, field) in [
                ("position", "hasPosition", "position"),
                ("rotation_euler", "hasRotation", "rotation"),
                ("scale", "hasScale", "scale"),
            ] {
                if let Some(vector) = vec3(arguments, key)? {
                    args.insert(flag.to_string(), json!(true));
                    args.insert(field.to_string(), vector);
                    changes += 1;
                }
            }
            if changes == 0 {
                return Err(
                    "tackle_gameobject_modify needs at least one field to change".to_string(),
                );
            }
            Planned {
                command: "modify_gameobject",
                args: Value::Object(args),
            }
        }
        "tackle_prefab_instantiate" => {
            let prefab_path = editor_path_arg(arguments, "prefab_path")?;
            if !prefab_path.starts_with("Assets/")
                || !prefab_path.ends_with(".prefab")
                || prefab_path.len() > 1024
                || prefab_path.split('/').any(|s| s.is_empty() || s == "..")
            {
                return Err(
                    "prefab_path must start with `Assets/`, end with `.prefab`, and contain no empty or `..` segments"
                        .to_string(),
                );
            }
            args.insert("prefabPath".to_string(), json!(prefab_path));
            if let Some(parent) = optional_string(arguments, "parent_path")? {
                valid_hierarchy_path(&parent)?;
                args.insert("parentPath".to_string(), json!(parent));
            }
            Planned {
                command: "instantiate_prefab",
                args: Value::Object(args),
            }
        }
        _ => return Err(format!("{tool}: not an undo-groupable editor write")),
    };
    Ok(planned)
}

/// Script paths: relative to `Assets/`, `.cs`, no empty, `.` or
/// `..` segments — the filesystem surface can never leave Assets.
fn valid_script_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 1024 {
        return Err("path must be non-empty and at most 1024 bytes".to_string());
    }
    if !path.ends_with(".cs") {
        return Err("path must end with `.cs`".to_string());
    }
    if path.starts_with('/') || path.contains('\\') {
        return Err("path must be relative to Assets/ with `/` separators".to_string());
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err("path must not contain empty, `.` or `..` segments".to_string());
    }
    Ok(())
}

/// The outcome of one write call, before result shaping.
pub enum Outcome {
    /// Not a write tool: the caller falls through.
    NotWrite,
    /// A finished tool result: (text, is_error).
    Done(String, bool),
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string())
}

/// Drive the write surface for one tools/call.
pub struct WriteCore {
    bridge: Bridge,
    store: Store,
}

impl WriteCore {
    pub fn new(bridge: Bridge) -> Self {
        Self {
            bridge,
            store: Store::new(),
        }
    }

    /// One bridge call with the typed error intact, logged once.
    /// Callers that must distinguish a dropped session (a domain
    /// reload re-making it) from a real failure use this directly.
    async fn editor_call(&self, command: &str, args: Option<Value>) -> Result<Value, CallError> {
        let result = match &args {
            Some(args) => self.bridge.call_with_args(command, args.clone()).await,
            None => self.bridge.call(command).await,
        };
        if let Err(error) = &result {
            log(format!("{command}: {error}"));
        }
        result
    }

    async fn editor(&self, command: &str, args: Option<Value>) -> Result<Value, String> {
        self.editor_call(command, args)
            .await
            .map_err(|error| error.to_string())
    }

    /// Handle one tools/call if it names a write tool.
    pub async fn dispatch(&self, name: &str, arguments: &Map<String, Value>) -> Outcome {
        let Some(class) = decide::classify(name) else {
            return Outcome::NotWrite;
        };
        if class == Class::Read {
            return Outcome::NotWrite;
        }
        // The decide() gate judges key presence by class; the shape
        // check then judges the key itself.
        let key = arguments.get("idempotency_key").and_then(Value::as_str);
        if let Decision::Refuse(reason) = decide::decide(name, class, key) {
            return Outcome::Done(reason, true);
        }
        let key = key.unwrap_or("");
        if let Err(message) = idempotency::validate_key(key) {
            return Outcome::Done(message, true);
        }
        let fingerprint = idempotency::fingerprint(name, arguments);
        match self.store.lookup(key, &fingerprint) {
            Lookup::Hit(text) => {
                log(format!("{name}: idempotent replay of key {key}"));
                return Outcome::Done(text, false);
            }
            Lookup::Conflict => {
                return Outcome::Done(
                    format!(
                        "idempotency_key `{key}` was already applied with different arguments; refusing to reuse it"
                    ),
                    true,
                );
            }
            Lookup::Miss => {}
        }
        let result = match name {
            "tackle_batch_execute" => self.run_batch(arguments).await,
            "tackle_script_create" => self.run_script_create(arguments).await,
            "tackle_script_patch" => self.run_script_patch(arguments).await,
            "tackle_scene_save" => self.editor("save_scene", None).await,
            "tackle_undo" => self.editor("perform_undo", None).await,
            "tackle_redo" => self.editor("perform_redo", None).await,
            _ => match plan_write(name, arguments, false) {
                Ok(planned) => self.editor(planned.command, Some(planned.args)).await,
                Err(message) => Err(message),
            },
        };
        match result {
            Ok(payload) => {
                let text = pretty(&payload);
                self.store.record(key, fingerprint, text.clone());
                Outcome::Done(text, false)
            }
            Err(message) => Outcome::Done(message, true),
        }
    }

    /// tackle_batch_execute: validate all members, then
    /// begin → apply joined → commit, or revert on first failure.
    async fn run_batch(&self, arguments: &Map<String, Value>) -> Result<Value, String> {
        let Some(Value::Array(ops)) = arguments.get("operations") else {
            return Err("operations must be an array of {name, arguments}".to_string());
        };
        if ops.is_empty() || ops.len() > BATCH_OPS_MAX {
            return Err(format!(
                "operations must contain 1..={BATCH_OPS_MAX} entries"
            ));
        }
        // Validate every member before the editor is touched.
        let mut planned = Vec::with_capacity(ops.len());
        for (index, op) in ops.iter().enumerate() {
            let Some(member) = op.as_object() else {
                return Err(format!("batch op {index}: must be an object"));
            };
            let Some(member_name) = member.get("name").and_then(Value::as_str) else {
                return Err(format!("batch op {index}: name is required"));
            };
            if let Decision::Refuse(reason) = decide::decide_batch_member(member_name) {
                return Err(format!("batch op {index}: {reason}"));
            }
            let empty = Map::new();
            let member_args = match member.get("arguments") {
                None | Some(Value::Null) => &empty,
                Some(Value::Object(map)) => map,
                Some(_) => {
                    return Err(format!("batch op {index}: arguments must be an object"));
                }
            };
            let plan = plan_write(member_name, member_args, true)
                .map_err(|message| format!("batch op {index}: {message}"))?;
            planned.push((member_name.to_string(), plan));
        }
        let begin = self
            .editor(
                "batch_begin",
                Some(json!({"label": "tackle: batch_execute"})),
            )
            .await?;
        let Some(group) = begin.get("group").and_then(Value::as_i64) else {
            return Err("editor protocol violation: batch_begin returned no group".to_string());
        };
        let mut results = Vec::with_capacity(planned.len());
        for (index, (member_name, plan)) in planned.iter().enumerate() {
            match self.editor(plan.command, Some(plan.args.clone())).await {
                Ok(payload) => results.push(payload),
                Err(message) => {
                    let rollback = self
                        .editor("batch_revert", Some(json!({"group": group})))
                        .await;
                    return Err(match rollback {
                        Ok(_) => format!(
                            "batch failed at op {index} ({member_name}): {message}; the batch was rolled back"
                        ),
                        Err(rollback_error) => format!(
                            "batch failed at op {index} ({member_name}): {message}; ROLLBACK FAILED: {rollback_error}"
                        ),
                    });
                }
            }
        }
        self.editor("batch_commit", Some(json!({"group": group})))
            .await?;
        Ok(json!({
            "applied": results.len(),
            "group": group,
            "results": results,
        }))
    }

    /// The connected editor's project path, learned per call from
    /// the authenticated session (never cached across sessions).
    async fn project_path(&self) -> Result<PathBuf, String> {
        let state = self.editor("editor_state", None).await?;
        let Some(path) = state.get("projectPath").and_then(Value::as_str) else {
            return Err("editor protocol violation: editor_state returned no projectPath".into());
        };
        Ok(PathBuf::from(path))
    }

    async fn run_script_create(&self, arguments: &Map<String, Value>) -> Result<Value, String> {
        let path = editor_path_arg(arguments, "path")?;
        valid_script_path(&path)?;
        let contents = editor_path_arg(arguments, "contents")?;
        if contents.len() > SCRIPT_BYTES_MAX {
            return Err(format!("contents must be at most {SCRIPT_BYTES_MAX} bytes"));
        }
        let project = self.project_path().await?;
        let target = project.join("Assets").join(&path);
        if target.exists() {
            return Err(format!("script already exists: Assets/{path}"));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create script directories: {error}"))?;
        }
        // create_new: the existence check above is advisory; the
        // open itself is the race-safe refusal.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options
            .open(&target)
            .map_err(|error| format!("cannot create script Assets/{path}: {error}"))?;
        use std::io::Write;
        file.write_all(contents.as_bytes())
            .map_err(|error| format!("cannot write script Assets/{path}: {error}"))?;
        drop(file);
        self.finish_script_write(&path, "created").await
    }

    async fn run_script_patch(&self, arguments: &Map<String, Value>) -> Result<Value, String> {
        let path = editor_path_arg(arguments, "path")?;
        valid_script_path(&path)?;
        let old_text = editor_path_arg(arguments, "old_text")?;
        if old_text.is_empty() {
            return Err("old_text must be non-empty".to_string());
        }
        let new_text = editor_path_arg(arguments, "new_text")?;
        let project = self.project_path().await?;
        let target = project.join("Assets").join(&path);
        let metadata =
            std::fs::metadata(&target).map_err(|_| format!("script not found: Assets/{path}"))?;
        if metadata.len() > SCRIPT_READ_MAX {
            return Err(format!(
                "script exceeds the {SCRIPT_READ_MAX}-byte patch limit: Assets/{path}"
            ));
        }
        let original = std::fs::read_to_string(&target)
            .map_err(|error| format!("cannot read script Assets/{path}: {error}"))?;
        let matches = original.matches(&old_text).count();
        if matches != 1 {
            return Err(format!(
                "old_text must match exactly once in Assets/{path} (found {matches})"
            ));
        }
        let patched = original.replacen(&old_text, &new_text, 1);
        if patched.len() > SCRIPT_BYTES_MAX {
            return Err(format!(
                "patched script would exceed {SCRIPT_BYTES_MAX} bytes"
            ));
        }
        // Temp file + rename in the same directory: a crash mid-write
        // never leaves a truncated script behind.
        let temp = target.with_extension("cs.tackle-tmp");
        std::fs::write(&temp, patched.as_bytes())
            .map_err(|error| format!("cannot write script Assets/{path}: {error}"))?;
        std::fs::rename(&temp, &target)
            .map_err(|error| format!("cannot replace script Assets/{path}: {error}"))?;
        self.finish_script_write(&path, "patched").await
    }

    /// Refresh assets, wait for the compile to settle, and read the
    /// diagnostics back from the console.
    async fn finish_script_write(&self, path: &str, verb: &'static str) -> Result<Value, String> {
        // The refresh races the domain reload the write itself
        // triggers (the editor may also auto-refresh on detecting
        // the changed file), so a session that drops — or is
        // briefly absent — under the refresh is that reload already
        // in progress, not a failure of the write. compile_outcome
        // is the verdict: it waits the reload out and reads the
        // diagnostics back. Any other refresh error fails the call.
        match self.editor_call("asset_refresh", None).await {
            Ok(_) | Err(CallError::Disconnected | CallError::NoEditor) => {}
            Err(error) => return Err(error.to_string()),
        }
        let (settled, diagnostics) = self.compile_outcome().await;
        Ok(json!({
            "path": format!("Assets/{path}"),
            verb: true,
            "compiled": settled && diagnostics.is_empty(),
            "compileSettled": settled,
            "diagnostics": diagnostics,
        }))
    }

    /// Poll the editor until compilation settles, then collect the
    /// console's `error CS` entries. A dropped session mid-compile
    /// (domain reload) is waited out, not failed.
    async fn compile_outcome(&self) -> (bool, Vec<Value>) {
        let deadline = tokio::time::Instant::now() + COMPILE_DEADLINE;
        let started = tokio::time::Instant::now();
        let mut saw_compiling = false;
        let settled = loop {
            match self.editor("editor_state", None).await {
                Ok(state) => {
                    let compiling = state
                        .get("isCompiling")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if compiling {
                        saw_compiling = true;
                    } else if saw_compiling || started.elapsed() > Duration::from_secs(5) {
                        break true;
                    }
                }
                Err(_) => {
                    // Domain reload dropped the session; the editor
                    // reconnects against the same capability file, so
                    // an unreachable editor is waited out like any
                    // other unsettled state.
                }
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        let mut diagnostics = Vec::new();
        if let Ok(console) = self
            .editor("console", Some(json!({"maxEntries": 100})))
            .await
            && let Some(entries) = console.get("entries").and_then(Value::as_array)
        {
            for entry in entries {
                let message = entry.get("message").and_then(Value::as_str).unwrap_or("");
                if message.contains("error CS") {
                    diagnostics.push(json!({
                        "message": message,
                        "file": entry.get("file").cloned().unwrap_or(Value::Null),
                        "line": entry.get("line").cloned().unwrap_or(Value::Null),
                    }));
                }
            }
        }
        (settled, diagnostics)
    }
}
