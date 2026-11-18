//! The real binary over stdio: identity, tool surface, honest no-editor
//! errors, refusal logging, fail-closed start.
//!
//! B1 (fleet D-113 standardization): sessions open with the modern
//! `server/discover` handshake and every request carries its
//! protocol version in `params._meta`; the legacy `initialize`
//! handshake is refused by the era guard (tests/era.rs pins that).

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const DEADLINE: Duration = Duration::from_secs(20);

/// The modern per-request metadata every frame carries.
const META: &str = r#""_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"test","version":"0"},"io.modelcontextprotocol/clientCapabilities":{}}"#;

fn discover(id: u64) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"server/discover","params":{{{META}}}}}"#)
}

fn request(id: u64, method: &str, params: &str) -> String {
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

fn spawn(state_dir: &Path, port: u16) -> Child {
    Command::new(env!("CARGO_BIN_EXE_tackle"))
        .env("TACKLE_STATE_DIR", state_dir)
        .env("TACKLE_PORT", port.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tackle")
}

fn finish(mut child: Child, requests: &[String]) -> Output {
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
    child.wait_with_output().expect("output")
}

fn by_id(output: &Output, id: u64) -> Value {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("stdout is JSON-RPC only"))
        .find(|response| response["id"] == id)
        .unwrap_or_else(|| panic!("no response with id {id}"))
}

#[test]
fn ok_identity_and_exactly_six_read_only_tools() {
    // B1 asserted the whole surface here; since B2 the full-surface
    // gate lives in tests/era.rs (18 tools, U1 entries byte-pinned).
    // This test keeps the identity check and the read tools' shape.
    let state = tempfile::tempdir().expect("temp");
    let output = finish(
        spawn(state.path(), free_port()),
        &[discover(1), request(2, "tools/list", "")],
    );
    let info = &by_id(&output, 1)["result"]["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(info["name"], "tackle");
    assert_eq!(info["title"], "Tackle");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    let tools = by_id(&output, 2)["result"]["tools"]
        .as_array()
        .expect("tools")
        .clone();
    let mut names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect();
    names.sort_unstable();
    assert_eq!(names.len(), 18, "six reads plus the twelve U2 writes");
    for read in [
        "tackle_console",
        "tackle_editor_state",
        "tackle_gameobject_properties",
        "tackle_project_info",
        "tackle_scene_hierarchy",
        "tackle_selection",
    ] {
        assert!(names.contains(&read), "{read} missing");
    }
    for read in [
        "tackle_console",
        "tackle_editor_state",
        "tackle_gameobject_properties",
        "tackle_project_info",
        "tackle_scene_hierarchy",
        "tackle_selection",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == read)
            .expect("read tool present");
        assert_eq!(
            tool["annotations"]["readOnlyHint"], true,
            "{read} stays read-only"
        );
    }
    // Parameterized tools carry real schemas: hierarchy pagination is
    // optional, the gameobject path is required.
    let hierarchy = tools
        .iter()
        .find(|tool| tool["name"] == "tackle_scene_hierarchy")
        .expect("hierarchy tool");
    // Option<i64> renders as ["integer", "null"] under schemars.
    for field in ["offset", "limit"] {
        let rendered = hierarchy["inputSchema"]["properties"][field]["type"].clone();
        let accepts_integer = rendered == "integer"
            || rendered
                .as_array()
                .is_some_and(|types| types.iter().any(|type_| type_ == "integer"));
        assert!(accepts_integer, "{field} schema type: {rendered}");
    }
    assert!(
        !hierarchy["inputSchema"]["required"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|required| required == "offset" || required == "limit"),
        "pagination must be optional: {}",
        hierarchy["inputSchema"]
    );
    let properties = tools
        .iter()
        .find(|tool| tool["name"] == "tackle_gameobject_properties")
        .expect("properties tool");
    assert!(
        properties["inputSchema"]["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|required| required == "path"),
        "gameobject path must be required: {}",
        properties["inputSchema"]
    );
}

#[test]
fn ok_resources_list_five_read_only_mirrors() {
    let state = tempfile::tempdir().expect("temp");
    let output = finish(
        spawn(state.path(), free_port()),
        &[discover(1), request(2, "resources/list", "")],
    );
    let resources = by_id(&output, 2)["result"]["resources"]
        .as_array()
        .expect("resources")
        .clone();
    let mut uris: Vec<&str> = resources
        .iter()
        .map(|resource| resource["uri"].as_str().expect("uri"))
        .collect();
    uris.sort_unstable();
    assert_eq!(
        uris,
        [
            "tackle://editor/console",
            "tackle://editor/project-info",
            "tackle://editor/scene-hierarchy",
            "tackle://editor/selection",
            "tackle://editor/state",
        ]
    );
    assert!(
        resources
            .iter()
            .all(|resource| resource["mimeType"] == "application/json")
    );
}

#[test]
fn ok_startup_publishes_capability_and_logs_without_token() {
    let state = tempfile::tempdir().expect("temp");
    let port = free_port();
    let output = finish(spawn(state.path(), port), &[discover(1)]);
    let file = state.path().join("capability/editor.json");
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(file).expect("file")).expect("json");
    assert_eq!(body["port"], port);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("listening on 127.0.0.1:{port}")),
        "{stderr}"
    );
    assert!(!stderr.contains(body["token"].as_str().expect("token")));
}

#[test]
fn adv_all_tools_report_no_editor_connected() {
    let state = tempfile::tempdir().expect("temp");
    let mut requests = vec![discover(1)];
    for (id, name, arguments) in [
        (2, "tackle_editor_state", "{}"),
        (3, "tackle_scene_hierarchy", "{}"),
        (4, "tackle_gameobject_properties", r#"{"path":"Ground"}"#),
        (5, "tackle_console", "{}"),
        (6, "tackle_project_info", "{}"),
        (7, "tackle_selection", "{}"),
    ] {
        requests.push(request(
            id,
            "tools/call",
            &format!(r#""name":"{name}","arguments":{arguments}"#),
        ));
    }
    let output = finish(spawn(state.path(), free_port()), &requests);
    for id in 2..=7 {
        let result = &by_id(&output, id)["result"];
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "no editor connected");
    }
}

#[test]
fn adv_invalid_tool_arguments_rejected_before_any_editor_call() {
    let state = tempfile::tempdir().expect("temp");
    let mut requests = vec![discover(1)];
    for (id, name, arguments) in [
        (2, "tackle_scene_hierarchy", r#"{"limit":501}"#),
        (3, "tackle_scene_hierarchy", r#"{"offset":-1}"#),
        (4, "tackle_console", r#"{"max_entries":0}"#),
        (5, "tackle_gameobject_properties", r#"{"path":"Ground/"}"#),
    ] {
        requests.push(request(
            id,
            "tools/call",
            &format!(r#""name":"{name}","arguments":{arguments}"#),
        ));
    }
    let output = finish(spawn(state.path(), free_port()), &requests);
    for id in 2..=5 {
        let result = &by_id(&output, id)["result"];
        assert_eq!(result["isError"], true, "id {id}: {result}");
    }
    assert_eq!(
        by_id(&output, 2)["result"]["content"][0]["text"],
        "limit must be between 1 and 500"
    );
    assert_eq!(
        by_id(&output, 3)["result"]["content"][0]["text"],
        "offset must be >= 0"
    );
    assert_eq!(
        by_id(&output, 5)["result"]["content"][0]["text"],
        "path must not contain empty segments"
    );
}

#[test]
fn adv_resource_read_without_editor_is_a_protocol_error() {
    let state = tempfile::tempdir().expect("temp");
    let requests = vec![
        discover(1),
        request(2, "resources/read", r#""uri":"tackle://editor/state""#),
        request(3, "resources/read", r#""uri":"tackle://editor/nope""#),
    ];
    let output = finish(spawn(state.path(), free_port()), &requests);
    let no_editor = &by_id(&output, 2)["error"];
    assert_eq!(no_editor["code"], -32603);
    assert!(
        no_editor["message"]
            .as_str()
            .expect("message")
            .contains("no editor connected"),
        "{no_editor}"
    );
    // Era delta (B1): U1 answered unknown resources with -32002
    // (RESOURCE_NOT_FOUND); the modern era reassigns that condition
    // to -32602 (INVALID_PARAMS) and rmcp 3.8's handler performs the
    // rewrite for 2026-07-28 peers. The message text is unchanged.
    let not_found = &by_id(&output, 3)["error"];
    assert_eq!(not_found["code"], -32602);
    assert!(
        not_found["message"]
            .as_str()
            .expect("message")
            .contains("unknown resource"),
        "{not_found}"
    );
}

#[test]
fn adv_wrong_token_refused_and_logged_without_leaking() {
    let state = tempfile::tempdir().expect("temp");
    let port = free_port();
    let child = spawn(state.path(), port);
    let file = state.path().join("capability/editor.json");
    let started = Instant::now();
    while !file.exists() {
        assert!(
            started.elapsed() < DEADLINE,
            "capability file never appeared"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let real: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("json");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let wrong = format!("Bearer {}", "c3".repeat(32));
    let refused = runtime.block_on(async {
        (
            common::connect(address, "/editor", &[("authorization", &wrong)])
                .await
                .err(),
            common::connect(address, "/editor", &[]).await.err(),
        )
    });
    assert_eq!(refused, (Some(401), Some(401)));
    let output = finish(child, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refused editor connection") && stderr.contains("wrong token"),
        "{stderr}"
    );
    assert!(stderr.contains("missing token"), "{stderr}");
    assert!(!stderr.contains(real["token"].as_str().expect("token")));
}

#[test]
fn adv_port_in_use_refuses_to_start_and_publishes_nothing() {
    let state = tempfile::tempdir().expect("temp");
    let holder = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = holder.local_addr().expect("address").port();
    let output = finish(spawn(state.path(), port), &[discover(1)]);
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "no MCP traffic when the port is taken"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot bind"));
    assert!(!state.path().join("capability/editor.json").exists());
}
