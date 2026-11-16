//! The narrow broker surface the TUI drives.
//!
//! [`TackleFacade`] exposes exactly what the screens need: `status`,
//! `tools`, and `call`. Two implementations exist:
//!
//! - [`FakeFacade`] — in-memory, scripted, records every call. Used
//!   by every test; never touches a process or an editor.
//! - [`LiveFacade`] — the real wiring. Tool definitions and tool
//!   classification come from the `tackle` library crate where
//!   reachable (`tackle::server::tool_list`,
//!   `tackle::decide::classify`); calls go over an MCP stdio client
//!   that spawns the member's own `tackle` server binary and speaks
//!   the modern protocol era only (2026-07-28, fleet D-113): frames
//!   are newline-delimited JSON-RPC and every request carries its
//!   protocol version in `params._meta`, exactly as tackle's own
//!   stdio tests do. The library cannot serve calls itself — the
//!   bridge owns the one authenticated editor session inside the
//!   server process — so stdio is the honest transport, not a stub.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

/// Why a facade call failed. Typed, never a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FacadeError {
    /// The server process could not be spawned or died.
    Transport(String),
    /// The server answered with a JSON-RPC error object.
    Rpc(String),
    /// The tool itself reported an error (e.g. `no editor connected`).
    Tool(String),
    /// The reply was not the expected shape.
    Protocol(String),
}

impl std::fmt::Display for FacadeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FacadeError::Transport(m) => write!(f, "transport: {m}"),
            FacadeError::Rpc(m) => write!(f, "rpc: {m}"),
            FacadeError::Tool(m) => write!(f, "tool: {m}"),
            FacadeError::Protocol(m) => write!(f, "protocol: {m}"),
        }
    }
}

impl std::error::Error for FacadeError {}

/// The broker operations the TUI needs.
pub trait TackleFacade {
    /// Member identity + server/editor state as one JSON object.
    fn status(&mut self) -> Result<Value, FacadeError>;
    /// The member's own MCP tool definitions, as served.
    fn tools(&mut self) -> Result<Vec<Value>, FacadeError>;
    /// Call one tool by name with a JSON arguments object.
    fn call(&mut self, tool: &str, args: Value) -> Result<Value, FacadeError>;
    /// Whether the last known state has a live editor session. The
    /// Editor screen's honesty rule reads this, never a cache guess.
    fn editor_connected(&mut self) -> bool;
}

/// Longest single JSON-RPC line the stdio client will read, in bytes
/// (mirrors tackle's own 4 MiB frame bound in `tackle::guard`).
pub const RESPONSE_LINE_BYTES_MAX: usize = 4 * 1024 * 1024;

/// Response lines skipped while waiting for the matching id before
/// the read is declared wedged.
pub const RESPONSE_LINES_MAX: usize = 64;

/// Build one modern-era request frame. Pure; unit-tested.
pub fn request_frame(id: u64, method: &str, params: Value) -> Value {
    let mut params = params;
    if !params.is_object() {
        params = json!({});
    }
    if let Some(map) = params.as_object_mut() {
        map.insert(
            "_meta".to_string(),
            json!({
                "io.modelcontextprotocol/protocolVersion": tackle::guard::MODERN_VERSION,
                "io.modelcontextprotocol/clientInfo": {"name": "tackle-tui", "version": "0.1.0"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }),
        );
    }
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// Extract the text payload of a `tools/call` result: the first text
/// content block, parsed as JSON when it parses, else wrapped. An
/// `isError` result becomes [`FacadeError::Tool`].
pub fn unwrap_call_result(result: &Value) -> Result<Value, FacadeError> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(FacadeError::Tool(first_text(result)));
    }
    let text = first_text(result);
    if text.is_empty() {
        return Ok(json!({}));
    }
    Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!({"text": text})))
}

fn first_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The real facade: library data where reachable, MCP stdio client
/// for calls. Owns the spawned server child; Drop kills it.
pub struct LiveFacade {
    server_bin: PathBuf,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    next_id: u64,
    discovered: bool,
    connected: bool,
}

impl LiveFacade {
    /// A facade that will spawn `server_bin` on first use.
    pub fn new(server_bin: PathBuf) -> LiveFacade {
        LiveFacade {
            server_bin,
            child: None,
            stdin: None,
            stdout: None,
            next_id: 1,
            discovered: false,
            connected: false,
        }
    }

    fn ensure_started(&mut self) -> Result<(), FacadeError> {
        if self.child.is_some() {
            return Ok(());
        }
        let mut child = Command::new(&self.server_bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                FacadeError::Transport(format!("spawn {}: {e}", self.server_bin.display()))
            })?;
        self.stdin = child.stdin.take();
        self.stdout = child.stdout.take().map(BufReader::new);
        self.child = Some(child);
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, FacadeError> {
        self.ensure_started()?;
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let frame = request_frame(id, method, params);
        let line =
            serde_json::to_string(&frame).map_err(|e| FacadeError::Protocol(e.to_string()))?;
        {
            let stdin = self
                .stdin
                .as_mut()
                .ok_or_else(|| FacadeError::Transport("server stdin closed".into()))?;
            writeln!(stdin, "{line}")
                .and_then(|()| stdin.flush())
                .map_err(|e| FacadeError::Transport(format!("write to server: {e}")))?;
        }
        let stdout = self
            .stdout
            .as_mut()
            .ok_or_else(|| FacadeError::Transport("server stdout closed".into()))?;
        for _ in 0..RESPONSE_LINES_MAX {
            let mut buf = String::new();
            let read = stdout
                .read_line(&mut buf)
                .map_err(|e| FacadeError::Transport(format!("read from server: {e}")))?;
            if read == 0 {
                return Err(FacadeError::Transport("server closed stdout".into()));
            }
            if buf.len() > RESPONSE_LINE_BYTES_MAX {
                return Err(FacadeError::Protocol("oversized response line".into()));
            }
            let Ok(value) = serde_json::from_str::<Value>(buf.trim()) else {
                continue;
            };
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(FacadeError::Rpc(error.to_string()));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
        Err(FacadeError::Protocol("no matching response".into()))
    }

    fn discover(&mut self) -> Result<Value, FacadeError> {
        if !self.discovered {
            let result = self.request("server/discover", json!({}))?;
            self.discovered = true;
            return Ok(result);
        }
        self.request("ping", json!({}))
    }
}

impl TackleFacade for LiveFacade {
    fn status(&mut self) -> Result<Value, FacadeError> {
        let discovered = self.discover()?;
        let info = discovered
            .pointer("/_meta/io.modelcontextprotocol~1serverInfo")
            .cloned()
            .unwrap_or(Value::Null);
        // Editor state decides connectedness; its honest error text
        // (`no editor connected`) is a state, not a TUI failure.
        let (connected, editor) = match self.call("tackle_editor_state", json!({})) {
            Ok(state) => (true, state),
            Err(FacadeError::Tool(message)) if message.contains("no editor connected") => {
                (false, Value::Null)
            }
            Err(other) => return Err(other),
        };
        self.connected = connected;
        Ok(json!({
            "member": "tackle",
            "version": info.get("version").cloned().unwrap_or(Value::Null),
            "server": "up",
            "editor_connected": connected,
            "editor": editor,
        }))
    }

    fn tools(&mut self) -> Result<Vec<Value>, FacadeError> {
        // Library first: the served definitions are data in the lib.
        let from_lib = tackle::server::tool_list();
        if let Value::Array(definitions) = from_lib
            && !definitions.is_empty()
        {
            return Ok(definitions);
        }
        let result = self.request("tools/list", json!({}))?;
        match result.get("tools").and_then(Value::as_array) {
            Some(tools) => Ok(tools.clone()),
            None => Err(FacadeError::Protocol("tools/list shape".into())),
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, FacadeError> {
        let result = self.request("tools/call", json!({"name": tool, "arguments": args}))?;
        unwrap_call_result(&result)
    }

    fn editor_connected(&mut self) -> bool {
        self.connected
    }
}

impl Drop for LiveFacade {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// An in-memory [`TackleFacade`] for tests: scripted results and a
/// recorded call log. No process, no editor, no I/O.
#[derive(Debug, Default)]
pub struct FakeFacade {
    /// Editor-connected flag reported by `status`/`editor_connected`.
    pub connected: bool,
    /// Scripted `tools` result (defaults to tackle's served list).
    pub tools_result: Option<Vec<Value>>,
    /// Scripted per-tool results, consumed in order per tool.
    pub call_results: BTreeMap<String, VecDeque<Value>>,
    /// Scripted per-tool error message (wins over scripted results).
    pub call_error: Option<String>,
    /// Every `call` the facade received, in order: (tool, args).
    pub calls: Vec<(String, Value)>,
}

impl FakeFacade {
    /// A connected fake carrying tackle's own fixture-shaped data:
    /// editor state, a two-node hierarchy, one object's properties.
    pub fn sample() -> FakeFacade {
        let mut fake = FakeFacade {
            connected: true,
            ..FakeFacade::default()
        };
        fake.script(
            "tackle_editor_state",
            json!({
                "unityVersion": "6000.0.0f1", "projectName": "Sample",
                "activeScene": "Main", "isPlaying": false, "isCompiling": false
            }),
        );
        fake.script(
            "tackle_scene_hierarchy",
            json!({
                "scene": "Main", "totalCount": 2, "truncated": false,
                "nodes": [
                    {"name": "Root", "path": "Root", "active": true, "childCount": 1, "depth": 0},
                    {"name": "Child", "path": "Root/Child", "active": true, "childCount": 0, "depth": 1}
                ]
            }),
        );
        fake.script(
            "tackle_gameobject_properties",
            json!({
                "name": "Root", "path": "Root", "active": true,
                "components": ["UnityEngine.Transform"], "childCount": 1
            }),
        );
        fake.script("tackle_selection", json!({"selected": [], "count": 0}));
        fake.script("tackle_console", json!({"entries": []}));
        fake.script("tackle_project_info", json!({"productName": "Sample"}));
        fake
    }

    /// A disconnected fake: every call errors `no editor connected`.
    pub fn disconnected() -> FakeFacade {
        FakeFacade {
            connected: false,
            call_error: Some("no editor connected".to_string()),
            ..FakeFacade::default()
        }
    }

    /// Queue one scripted result for `tool`.
    pub fn script(&mut self, tool: &str, result: Value) {
        self.call_results
            .entry(tool.to_string())
            .or_default()
            .push_back(result);
    }
}

impl TackleFacade for FakeFacade {
    fn status(&mut self) -> Result<Value, FacadeError> {
        Ok(json!({
            "member": "tackle",
            "version": env!("CARGO_PKG_VERSION"),
            "server": "fake",
            "editor_connected": self.connected,
            "editor": Value::Null,
        }))
    }

    fn tools(&mut self) -> Result<Vec<Value>, FacadeError> {
        if let Some(tools) = &self.tools_result {
            return Ok(tools.clone());
        }
        match tackle::server::tool_list() {
            Value::Array(definitions) => Ok(definitions),
            _ => Ok(Vec::new()),
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, FacadeError> {
        self.calls.push((tool.to_string(), args));
        if let Some(message) = &self.call_error {
            return Err(FacadeError::Tool(message.clone()));
        }
        if let Some(queue) = self.call_results.get_mut(tool)
            && let Some(result) = queue.pop_front()
        {
            return Ok(result);
        }
        Ok(json!({"ok": true, "tool": tool}))
    }

    fn editor_connected(&mut self) -> bool {
        self.connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_frame_carries_modern_meta() {
        let frame = request_frame(7, "tools/list", json!({}));
        assert_eq!(frame["id"], 7);
        let meta = &frame["params"]["_meta"];
        assert_eq!(
            meta["io.modelcontextprotocol/protocolVersion"],
            tackle::guard::MODERN_VERSION
        );
    }

    #[test]
    fn ok_unwrap_parses_text_json() {
        let result = json!({"content": [{"type": "text", "text": "{\"a\": 1}"}]});
        assert_eq!(unwrap_call_result(&result).expect("ok"), json!({"a": 1}));
    }

    #[test]
    fn adv_unwrap_error_result_is_tool_error() {
        let result = json!({"isError": true, "content": [{"text": "no editor connected"}]});
        match unwrap_call_result(&result) {
            Err(FacadeError::Tool(m)) => assert!(m.contains("no editor connected")),
            other => panic!("expected tool error, got {other:?}"),
        }
    }

    #[test]
    fn adv_unwrap_non_json_text_is_wrapped_not_dropped() {
        let result = json!({"content": [{"text": "plain text"}]});
        assert_eq!(
            unwrap_call_result(&result).expect("ok"),
            json!({"text": "plain text"})
        );
    }

    #[test]
    fn ok_fake_records_calls_in_order() {
        let mut fake = FakeFacade::sample();
        let _ = fake.call("tackle_editor_state", json!({}));
        let _ = fake.call("tackle_selection", json!({}));
        assert_eq!(fake.calls.len(), 2);
        assert_eq!(fake.calls[0].0, "tackle_editor_state");
        assert_eq!(fake.calls[1].0, "tackle_selection");
    }

    #[test]
    fn adv_fake_disconnected_errors_every_call() {
        let mut fake = FakeFacade::disconnected();
        assert!(!fake.editor_connected());
        assert!(fake.call("tackle_editor_state", json!({})).is_err());
    }
}
