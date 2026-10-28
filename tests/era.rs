//! Era gates (fleet D-113 standardization, Brief B stage B1): the
//! migrated server speaks the modern MCP era only (2026-07-28).
//! Legacy contact is refused with the designed typed errors and the
//! server survives every refusal; the served tool and resource
//! definitions are byte-identical to the U1 (rmcp 3.3) baseline
//! captured in tests/fixtures/. Frames are raw strings and
//! responses are correlated by JSON-RPC id.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const DEADLINE: Duration = Duration::from_secs(20);

const META: &str = r#""_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"era-test","version":"0"},"io.modelcontextprotocol/clientCapabilities":{}}"#;
const LEGACY_META: &str = r#""_meta":{"io.modelcontextprotocol/protocolVersion":"2025-06-18","io.modelcontextprotocol/clientInfo":{"name":"era-test","version":"0"},"io.modelcontextprotocol/clientCapabilities":{}}"#;

fn discover(id: u64) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"server/discover","params":{{{META}}}}}"#)
}

fn frame(id: u64, method: &str, params: &str) -> String {
    let params = if params.is_empty() {
        META.to_string()
    } else {
        format!("{params},{META}")
    };
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{{params}}}}}"#)
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("address").port()
}

fn spawn(state_dir: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_tackle"))
        .env("TACKLE_STATE_DIR", state_dir)
        .env("TACKLE_PORT", free_port().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tackle")
}

fn finish_raw(mut child: Child, requests: &[String]) -> Vec<String> {
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for request in requests {
            if writeln!(stdin, "{request}").is_err() {
                break;
            }
        }
    }
    let started = Instant::now();
    while child.try_wait().expect("poll").is_none() {
        if started.elapsed() > DEADLINE {
            child.kill().expect("kill");
            panic!("tackle did not exit within {DEADLINE:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output: Output = child.wait_with_output().expect("output");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn finish(child: Child, requests: &[String]) -> Vec<Value> {
    finish_raw(child, requests)
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).expect("stdout is JSON-RPC only"))
        .collect()
}

fn by_id(frames: &[Value], id: u64) -> &Value {
    frames
        .iter()
        .find(|response| response["id"] == id)
        .unwrap_or_else(|| panic!("no response with id {id}"))
}

#[test]
fn ok_discover_advertises_only_the_modern_era() {
    let state = tempfile::tempdir().expect("temp");
    let frames = finish(spawn(state.path()), &[discover(1)]);
    let result = &by_id(&frames, 1)["result"];
    assert_eq!(
        result["supportedVersions"],
        serde_json::json!(["2026-07-28"])
    );
    let info = &result["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(info["name"], "tackle");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn ok_tools_list_is_byte_identical_to_the_u1_baseline() {
    let state = tempfile::tempdir().expect("temp");
    let lines = finish_raw(
        spawn(state.path()),
        &[discover(1), frame(2, "tools/list", "")],
    );
    let line = lines
        .iter()
        .find(|line| line.contains("\"id\":2"))
        .expect("tools/list response line");
    // The baseline array bytes appear verbatim on the wire (a parsed
    // Value would reorder object keys, so the check runs on the raw
    // line), and the parsed surface deep-equals the baseline too.
    let baseline = include_str!("fixtures/tools-list-u1.json").trim();
    assert!(
        line.contains(baseline),
        "tools/list drifted from U1:\n{line}"
    );
    let frames: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).expect("json"))
        .collect();
    let expected: Value = serde_json::from_str(baseline).expect("fixture parses");
    assert_eq!(by_id(&frames, 2)["result"]["tools"], expected);
}

#[test]
fn ok_resources_list_is_byte_identical_to_the_u1_baseline() {
    let state = tempfile::tempdir().expect("temp");
    let lines = finish_raw(
        spawn(state.path()),
        &[discover(1), frame(2, "resources/list", "")],
    );
    let line = lines
        .iter()
        .find(|line| line.contains("\"id\":2"))
        .expect("resources/list response line");
    let baseline = include_str!("fixtures/resources-list-u1.json").trim();
    assert!(
        line.contains(baseline),
        "resources/list drifted from U1:\n{line}"
    );
    let frames: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).expect("json"))
        .collect();
    let expected: Value = serde_json::from_str(baseline).expect("fixture parses");
    assert_eq!(by_id(&frames, 2)["result"]["resources"], expected);
}

#[test]
fn adv_legacy_initialize_is_refused_and_the_server_survives() {
    let state = tempfile::tempdir().expect("temp");
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"era-test","version":"0"}}}"#.to_string();
    let frames = finish(spawn(state.path()), &[initialize, discover(2)]);
    let refusal = &by_id(&frames, 1)["error"];
    assert_eq!(refusal["code"], -32022);
    assert_eq!(refusal["data"]["requested"], "2025-06-18");
    assert_eq!(
        refusal["data"]["supported"],
        serde_json::json!(["2026-07-28"])
    );
    // The refusal is not a crash: discovery still answers.
    assert_eq!(
        by_id(&frames, 2)["result"]["supportedVersions"],
        serde_json::json!(["2026-07-28"])
    );
}

#[test]
fn adv_legacy_meta_version_is_refused_by_the_sdk_gate() {
    let state = tempfile::tempdir().expect("temp");
    let legacy_call =
        format!(r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{{LEGACY_META}}}}}"#);
    let frames = finish(spawn(state.path()), &[legacy_call]);
    assert_eq!(by_id(&frames, 1)["error"]["code"], -32022);
}

#[test]
fn adv_missing_meta_is_refused_by_the_sdk_gate() {
    let state = tempfile::tempdir().expect("temp");
    let bare = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#.to_string();
    let frames = finish(spawn(state.path()), &[bare]);
    assert_eq!(by_id(&frames, 1)["error"]["code"], -32602);
}

#[test]
fn adv_unknown_method_is_refused_by_the_guard() {
    let state = tempfile::tempdir().expect("temp");
    let frames = finish(
        spawn(state.path()),
        &[frame(1, "prompts/list", ""), discover(2)],
    );
    let refusal = &by_id(&frames, 1)["error"];
    assert_eq!(refusal["code"], -32601);
    assert_eq!(
        refusal["message"],
        "method not supported by tackle: prompts/list"
    );
    assert!(by_id(&frames, 2)["result"].is_object());
}

#[test]
fn adv_malformed_frames_are_answered_at_the_byte_level() {
    let state = tempfile::tempdir().expect("temp");
    let frames = finish(
        spawn(state.path()),
        &[
            "this is not json".to_string(),
            "[1,2,3]".to_string(),
            discover(1),
        ],
    );
    let parse = frames
        .iter()
        .find(|f| f["error"]["code"] == -32700)
        .expect("parse error frame");
    assert_eq!(parse["id"], Value::Null);
    let invalid = frames
        .iter()
        .find(|f| f["error"]["code"] == -32600)
        .expect("invalid request frame");
    assert_eq!(invalid["id"], Value::Null);
    assert!(by_id(&frames, 1)["result"].is_object());
}

#[test]
fn adv_notifications_are_dropped_unexecuted() {
    let state = tempfile::tempdir().expect("temp");
    let notification =
        r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"tackle_editor_state","arguments":{}}}"#
            .to_string();
    let frames = finish(spawn(state.path()), &[notification, discover(1)]);
    // Exactly one frame comes back: the discover response. The
    // notification produced none.
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert!(frames[0]["result"].is_object());
}

#[test]
fn adv_oversized_frame_is_refused_and_the_server_survives() {
    let state = tempfile::tempdir().expect("temp");
    let oversized = format!(
        r#"{{"jsonrpc":"2.0","id":9,"method":"tools/list","params":{{"pad":"{}"}}}}"#,
        "x".repeat(4 * 1024 * 1024)
    );
    let frames = finish(spawn(state.path()), &[oversized, discover(1)]);
    let refusal = frames
        .iter()
        .find(|f| f["error"]["code"] == -32700)
        .expect("oversize refusal");
    assert!(
        refusal["error"]["message"]
            .as_str()
            .expect("message")
            .contains("MCP cap"),
        "{refusal}"
    );
    assert!(by_id(&frames, 1)["result"].is_object());
}
