//! The U2 write core against the real binary: a scripted fake
//! editor (in-memory scene, snapshot undo) answers the bridge
//! commands, and MCP calls flow over stdio through the era guard,
//! the decide() gate, and the idempotency store.
//!
//! The fake editor mirrors the C# command contract (same command
//! names, same args, same error strings for the cases it models);
//! the C# semantics themselves are gated by the live Unity smoke.

mod common;

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::runtime::Runtime;
use tokio_tungstenite::tungstenite::Message;

use common::Editor;

const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq)]
struct Obj {
    name: String,
    active: bool,
    tag: String,
    layer: String,
    components: Vec<String>,
    position: [f64; 3],
}

#[derive(Clone, Default)]
struct Scene {
    /// (full path, object) pairs in creation order.
    objects: Vec<(String, Obj)>,
}

impl Scene {
    fn get(&self, path: &str) -> Option<&Obj> {
        self.objects.iter().find(|(p, _)| p == path).map(|(_, o)| o)
    }
    fn get_mut(&mut self, path: &str) -> Option<&mut Obj> {
        self.objects
            .iter_mut()
            .find(|(p, _)| p == path)
            .map(|(_, o)| o)
    }
}

#[derive(Default)]
struct Model {
    scene: Scene,
    undo: Vec<Scene>,
    redo: Vec<Scene>,
    project_path: String,
    compiling_polls: u32,
    console_error: bool,
    /// One-shot: the next asset_refresh drops the socket unanswered
    /// (a simulated domain reload), then the fake reconnects.
    drop_on_refresh: bool,
    prefabs: HashSet<String>,
    layers: HashSet<String>,
    known_types: HashSet<String>,
    commands: Vec<String>,
}

impl Model {
    fn step(&mut self, join: bool) {
        if !join {
            self.undo.push(self.scene.clone());
            self.redo.clear();
        }
    }

    fn properties(&self, path: &str) -> Value {
        let obj = self.scene.get(path).expect("object exists");
        let child_count = self
            .scene
            .objects
            .iter()
            .filter(|(p, _)| p.rsplit_once('/').map(|(parent, _)| parent) == Some(path))
            .count();
        json!({
            "path": path,
            "name": obj.name,
            "activeSelf": obj.active,
            "activeInHierarchy": obj.active,
            "tag": obj.tag,
            "layer": obj.layer,
            "localPosition": {"x": obj.position[0], "y": obj.position[1], "z": obj.position[2]},
            "componentTypes": obj.components,
            "childCount": child_count,
            "onMainThread": true,
        })
    }

    fn execute(&mut self, command: &str, args: &Value) -> Result<Value, String> {
        self.commands.push(command.to_string());
        let join = args
            .get("joinGroup")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match command {
            "editor_state" => {
                let compiling = self.compiling_polls > 0;
                if compiling {
                    self.compiling_polls -= 1;
                }
                Ok(json!({
                    "unityVersion": "6000.6.6f1",
                    "projectName": "spike",
                    "projectPath": self.project_path,
                    "activeScene": {"name": "TestScene", "path": "Assets/Scenes/TestScene.unity"},
                    "isPlaying": false,
                    "isCompiling": compiling,
                    "queueDepth": 0,
                    "onMainThread": true,
                }))
            }
            "console" => {
                let entries = if self.console_error {
                    vec![json!({
                        "message": "Assets/Broken.cs(1,1): error CS1002: ; expected",
                        "file": "Assets/Broken.cs", "line": 1, "mode": 0,
                        "identifier": 0, "messageTruncated": false,
                    })]
                } else {
                    vec![]
                };
                Ok(
                    json!({"totalCount": entries.len(), "returnedCount": entries.len(),
                          "truncated": false, "onMainThread": true, "entries": entries}),
                )
            }
            "scene_hierarchy" => {
                let nodes: Vec<Value> = self
                    .scene
                    .objects
                    .iter()
                    .map(|(path, obj)| {
                        json!({"name": obj.name, "path": path, "active": obj.active,
                               "childCount": 0, "depth": path.matches('/').count()})
                    })
                    .collect();
                Ok(json!({"totalCount": nodes.len(), "nodes": nodes, "onMainThread": true}))
            }
            "gameobject_properties" => {
                let path = args["path"].as_str().unwrap_or("");
                if self.scene.get(path).is_some() {
                    Ok(self.properties(path))
                } else {
                    Err(format!("gameobject not found: {path}"))
                }
            }
            "create_gameobject" => {
                let name = args["name"].as_str().unwrap_or("");
                let path = match args.get("parentPath").and_then(Value::as_str) {
                    Some(parent) => {
                        if self.scene.get(parent).is_none() {
                            return Err(format!("gameobject not found: {parent}"));
                        }
                        format!("{parent}/{name}")
                    }
                    None => name.to_string(),
                };
                self.step(join);
                self.scene.objects.push((
                    path.clone(),
                    Obj {
                        name: name.to_string(),
                        active: true,
                        tag: "Untagged".to_string(),
                        layer: "Default".to_string(),
                        components: vec!["UnityEngine.Transform".to_string()],
                        position: [0.0, 0.0, 0.0],
                    },
                ));
                Ok(self.properties(&path))
            }
            "modify_gameobject" => {
                let path = args["path"].as_str().unwrap_or("").to_string();
                if self.scene.get(&path).is_none() {
                    return Err(format!("gameobject not found: {path}"));
                }
                if let Some(layer) = args.get("layerName").and_then(Value::as_str)
                    && !self.layers.contains(layer)
                {
                    return Err(format!("unknown layer: {layer}"));
                }
                self.step(join);
                let mut new_path = path.clone();
                {
                    let obj = self.scene.get_mut(&path).expect("checked");
                    if args
                        .get("hasActive")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        obj.active = args["active"].as_bool().unwrap_or(true);
                    }
                    if let Some(tag) = args.get("tag").and_then(Value::as_str) {
                        obj.tag = tag.to_string();
                    }
                    if let Some(layer) = args.get("layerName").and_then(Value::as_str) {
                        obj.layer = layer.to_string();
                    }
                    if let Some(name) = args.get("newName").and_then(Value::as_str) {
                        obj.name = name.to_string();
                    }
                    if args
                        .get("hasPosition")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        let position = &args["position"];
                        obj.position = [
                            position["x"].as_f64().unwrap_or(0.0),
                            position["y"].as_f64().unwrap_or(0.0),
                            position["z"].as_f64().unwrap_or(0.0),
                        ];
                    }
                }
                if let Some(name) = args.get("newName").and_then(Value::as_str) {
                    let parent_prefix = path
                        .rsplit_once('/')
                        .map(|(parent, _)| format!("{parent}/"))
                        .unwrap_or_default();
                    new_path = format!("{parent_prefix}{name}");
                    let old_prefix = format!("{path}/");
                    for (p, _) in self.scene.objects.iter_mut() {
                        if *p == path {
                            *p = new_path.clone();
                        } else if let Some(rest) = p.strip_prefix(&old_prefix) {
                            *p = format!("{new_path}/{rest}");
                        }
                    }
                }
                Ok(self.properties(&new_path))
            }
            "delete_gameobject" => {
                let path = args["path"].as_str().unwrap_or("");
                if self.scene.get(path).is_none() {
                    return Err(format!("gameobject not found: {path}"));
                }
                self.step(join);
                let prefix = format!("{path}/");
                self.scene
                    .objects
                    .retain(|(p, _)| p != path && !p.starts_with(&prefix));
                Ok(json!({"deleted": path, "onMainThread": true}))
            }
            "add_component" | "remove_component" => {
                let path = args["path"].as_str().unwrap_or("");
                let ty = args["componentType"].as_str().unwrap_or("");
                if self.scene.get(path).is_none() {
                    return Err(format!("gameobject not found: {path}"));
                }
                if ty == "UnityEngine.Transform" {
                    return Err(if command == "add_component" {
                        "cannot add the Transform component".to_string()
                    } else {
                        "cannot remove the Transform component".to_string()
                    });
                }
                if !self.known_types.contains(ty) {
                    return Err(format!("component type not found: {ty}"));
                }
                if command == "remove_component"
                    && !self
                        .scene
                        .get(path)
                        .expect("checked")
                        .components
                        .iter()
                        .any(|c| c == ty)
                {
                    return Err(format!("component not present: {ty}"));
                }
                self.step(join);
                let obj = self.scene.get_mut(path).expect("checked");
                if command == "add_component" {
                    obj.components.push(ty.to_string());
                } else {
                    obj.components.retain(|c| c != ty);
                }
                Ok(self.properties(path))
            }
            "instantiate_prefab" => {
                let prefab = args["prefabPath"].as_str().unwrap_or("");
                if !self.prefabs.contains(prefab) {
                    return Err(format!("prefab not found: {prefab}"));
                }
                let stem = Path::new(prefab)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Prefab")
                    .to_string();
                self.step(join);
                self.scene.objects.push((
                    stem.clone(),
                    Obj {
                        name: stem.clone(),
                        active: true,
                        tag: "Untagged".to_string(),
                        layer: "Default".to_string(),
                        components: vec!["UnityEngine.Transform".to_string()],
                        position: [0.0, 0.0, 0.0],
                    },
                ));
                Ok(self.properties(&stem))
            }
            "save_scene" => Ok(json!({
                "path": "Assets/Scenes/TestScene.unity", "name": "TestScene",
                "saved": true, "onMainThread": true,
            })),
            "perform_undo" => {
                let before = self.undo.len();
                let performed = if let Some(previous) = self.undo.pop() {
                    self.redo.push(self.scene.clone());
                    self.scene = previous;
                    true
                } else {
                    false
                };
                Ok(json!({"performed": performed, "groupBefore": before,
                          "groupAfter": self.undo.len(), "onMainThread": true}))
            }
            "perform_redo" => {
                let before = self.undo.len();
                let performed = if let Some(next) = self.redo.pop() {
                    self.undo.push(self.scene.clone());
                    self.scene = next;
                    true
                } else {
                    false
                };
                Ok(json!({"performed": performed, "groupBefore": before,
                          "groupAfter": self.undo.len(), "onMainThread": true}))
            }
            "batch_begin" => {
                self.undo.push(self.scene.clone());
                self.redo.clear();
                Ok(json!({"group": self.undo.len(), "onMainThread": true}))
            }
            "batch_commit" => Ok(json!({"group": args["group"], "collapsed": true,
                                        "onMainThread": true})),
            "batch_revert" => {
                let group = args["group"].as_u64().unwrap_or(0) as usize;
                if group >= 1 && group <= self.undo.len() {
                    self.scene = self.undo[group - 1].clone();
                    self.undo.truncate(group - 1);
                }
                Ok(json!({"group": group, "reverted": true, "onMainThread": true}))
            }
            "asset_refresh" => {
                self.compiling_polls = 2;
                // The fake "compiler": a script containing the
                // marker fails with a CS diagnostic.
                let assets = Path::new(&self.project_path).join("Assets");
                self.console_error = dir_contains(&assets, "SYNTAX_ERROR");
                Ok(json!({"refreshed": true, "onMainThread": true}))
            }
            other => Err(format!("unknown command `{other}`")),
        }
    }
}

fn dir_contains(dir: &Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if dir_contains(&path, needle) {
                return true;
            }
        } else if path.extension().is_some_and(|ext| ext == "cs")
            && std::fs::read_to_string(&path).is_ok_and(|text| text.contains(needle))
        {
            return true;
        }
    }
    false
}

struct Rig {
    _child: Child,
    stdin: ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
    next_id: u64,
    model: Arc<Mutex<Model>>,
    _runtime: Runtime,
    project: PathBuf,
    _state: tempfile::TempDir,
    _project_dir: tempfile::TempDir,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("address")
        .port()
}

fn rig() -> Rig {
    let state = tempfile::tempdir().expect("state dir");
    let project_dir = tempfile::tempdir().expect("project dir");
    let project = project_dir.path().join("project");
    std::fs::create_dir_all(project.join("Assets")).expect("assets dir");
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tackle"))
        .env("TACKLE_STATE_DIR", state.path())
        .env("TACKLE_PORT", port.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn tackle");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (sender, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    // Wait for the capability file, then read the per-run token.
    let file = state.path().join("capability/editor.json");
    let started = Instant::now();
    while !file.exists() {
        assert!(
            started.elapsed() < DEADLINE,
            "capability file never appeared"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("json");
    let token = body["token"].as_str().expect("token").to_string();

    let mut model = Model {
        project_path: project.to_string_lossy().to_string(),
        ..Default::default()
    };
    model.layers.insert("Default".to_string());
    model.layers.insert("Water".to_string());
    model
        .known_types
        .insert("UnityEngine.BoxCollider".to_string());
    model
        .known_types
        .insert("UnityEngine.Rigidbody".to_string());
    model
        .prefabs
        .insert("Assets/Prefabs/Seraphine.prefab".to_string());
    let model = Arc::new(Mutex::new(model));

    let runtime = Runtime::new().expect("runtime");
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let bearer = format!("Bearer {token}");
    let editor_model = Arc::clone(&model);
    runtime.spawn(async move {
        // Serve until the test ends, reconnecting after a simulated
        // domain reload: with drop_on_refresh armed, the next
        // asset_refresh closes the socket unanswered — the refresh
        // still landed editor-side, so the compile starts — and the
        // bridge fails the in-flight call as Disconnected.
        loop {
            let mut editor: Editor =
                common::connect(address, "/editor", &[("authorization", &bearer)])
                    .await
                    .expect("editor connects");
            while let Some(frame) = editor.next().await {
                let Ok(Message::Text(text)) = frame else {
                    continue;
                };
                let incoming: Value = serde_json::from_str(text.as_str()).expect("command json");
                let id = incoming["id"].clone();
                let command = incoming["command"].as_str().unwrap_or("").to_string();
                let args = incoming.get("args").cloned().unwrap_or(json!({}));
                let drop = {
                    let mut model = editor_model
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if command == "asset_refresh" && model.drop_on_refresh {
                        model.drop_on_refresh = false;
                        model.compiling_polls = 2;
                        model.commands.push(command.clone());
                        true
                    } else {
                        false
                    }
                };
                if drop {
                    break;
                }
                let reply = {
                    let mut model = editor_model
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match model.execute(&command, &args) {
                        Ok(result) => json!({"id": id, "ok": true, "result": result}),
                        Err(error) => json!({"id": id, "ok": false, "error": error}),
                    }
                };
                let _ = editor.send(Message::text(reply.to_string())).await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    let mut rig = Rig {
        _child: child,
        stdin,
        lines,
        next_id: 1,
        model,
        _runtime: runtime,
        project,
        _state: state,
        _project_dir: project_dir,
    };
    rig.request("server/discover", json!({}));
    // The session registers asynchronously; poll a read until it lands.
    let started = Instant::now();
    loop {
        let (error, _) = rig.tool("tackle_editor_state", json!({}));
        if !error {
            break;
        }
        assert!(started.elapsed() < DEADLINE, "editor session never landed");
        std::thread::sleep(Duration::from_millis(50));
    }
    rig
}

impl Rig {
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let mut params = params;
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": {"name": "writes-test", "version": "0"},
            "io.modelcontextprotocol/clientCapabilities": {},
        });
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{frame}").expect("write frame");
        self.stdin.flush().expect("flush");
        let deadline = Instant::now() + DEADLINE;
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("response within deadline");
            let value: Value = serde_json::from_str(&line).expect("response json");
            if value["id"] == id {
                return value;
            }
        }
    }

    /// Call one tool; returns (is_error, result text).
    fn tool(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &response["result"];
        (
            result["isError"].as_bool().unwrap_or(false),
            result["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        )
    }

    fn tool_json(&mut self, name: &str, arguments: Value) -> (bool, Value) {
        let (error, text) = self.tool(name, arguments);
        (error, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    fn exists(&mut self, path: &str) -> bool {
        let (error, _) = self.tool("tackle_gameobject_properties", json!({"path": path}));
        !error
    }

    fn commands(&self) -> Vec<String> {
        self.model
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .commands
            .clone()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self._child.kill();
    }
}

#[test]
fn ok_create_then_read_back() {
    let mut rig = rig();
    let (error, payload) = rig.tool_json(
        "tackle_gameobject_create",
        json!({"name": "Seraphine", "idempotency_key": "create-1"}),
    );
    assert!(!error, "{payload}");
    assert_eq!(payload["name"], "Seraphine");
    assert!(rig.exists("Seraphine"));
    let (error, payload) = rig.tool_json(
        "tackle_gameobject_create",
        json!({"name": "Hips", "parent_path": "Seraphine", "idempotency_key": "create-2"}),
    );
    assert!(!error, "{payload}");
    assert_eq!(payload["path"], "Seraphine/Hips");
    assert!(rig.exists("Seraphine/Hips"));
}

#[test]
fn ok_idempotent_retry_returns_original_without_reapplying() {
    let mut rig = rig();
    let (error1, text1) = rig.tool(
        "tackle_gameobject_create",
        json!({"name": "Solo", "idempotency_key": "dup-1"}),
    );
    let (error2, text2) = rig.tool(
        "tackle_gameobject_create",
        json!({"name": "Solo", "idempotency_key": "dup-1"}),
    );
    assert!(!error1 && !error2);
    assert_eq!(text1, text2, "a retry returns the original result verbatim");
    let (_, hierarchy) = rig.tool_json("tackle_scene_hierarchy", json!({}));
    let solos = hierarchy["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .filter(|node| node["name"] == "Solo")
        .count();
    assert_eq!(solos, 1, "the mutation applied exactly once");
}

#[test]
fn ok_modify_undo_redo_round_trip() {
    let mut rig = rig();
    rig.tool(
        "tackle_gameobject_create",
        json!({"name": "Rig", "idempotency_key": "m-create"}),
    );
    let (error, payload) = rig.tool_json(
        "tackle_gameobject_modify",
        json!({"path": "Rig", "active": false, "name": "Rig Prime",
               "position": [1.0, 2.0, 3.0],
               "idempotency_key": "m-modify"}),
    );
    assert!(!error, "{payload}");
    assert_eq!(payload["activeSelf"], false);
    assert_eq!(
        payload["localPosition"],
        json!({"x": 1.0, "y": 2.0, "z": 3.0})
    );
    assert!(rig.exists("Rig Prime"));
    let (error, payload) = rig.tool_json("tackle_undo", json!({"idempotency_key": "m-undo"}));
    assert!(!error, "{payload}");
    assert_eq!(payload["performed"], true);
    assert!(rig.exists("Rig"));
    assert!(!rig.exists("Rig Prime"));
    let (_, payload) = rig.tool_json("tackle_gameobject_properties", json!({"path": "Rig"}));
    assert_eq!(payload["activeSelf"], true);
    let (error, payload) = rig.tool_json("tackle_redo", json!({"idempotency_key": "m-redo"}));
    assert!(!error, "{payload}");
    assert_eq!(payload["performed"], true);
    assert!(rig.exists("Rig Prime"));
}

#[test]
fn ok_batch_is_a_single_undo_step() {
    let mut rig = rig();
    let (error, payload) = rig.tool_json(
        "tackle_batch_execute",
        json!({
            "idempotency_key": "batch-1",
            "operations": [
                {"name": "tackle_gameobject_create", "arguments": {"name": "A"}},
                {"name": "tackle_gameobject_create", "arguments": {"name": "B"}},
                {"name": "tackle_component_add",
                 "arguments": {"path": "A", "component_type": "UnityEngine.BoxCollider"}},
            ],
        }),
    );
    assert!(!error, "{payload}");
    assert_eq!(payload["applied"], 3);
    assert!(rig.exists("A") && rig.exists("B"));
    let (_, payload) = rig.tool_json("tackle_undo", json!({"idempotency_key": "batch-undo"}));
    assert_eq!(payload["performed"], true);
    assert!(
        !rig.exists("A") && !rig.exists("B"),
        "one undo reverts the whole batch"
    );
}

#[test]
fn ok_component_and_prefab_writes() {
    let mut rig = rig();
    rig.tool(
        "tackle_gameobject_create",
        json!({"name": "Host", "idempotency_key": "c-create"}),
    );
    let (error, payload) = rig.tool_json(
        "tackle_component_add",
        json!({"path": "Host", "component_type": "UnityEngine.Rigidbody",
               "idempotency_key": "c-add"}),
    );
    assert!(!error, "{payload}");
    assert!(
        payload["componentTypes"]
            .as_array()
            .expect("types")
            .iter()
            .any(|t| t == "UnityEngine.Rigidbody")
    );
    let (error, _) = rig.tool(
        "tackle_component_remove",
        json!({"path": "Host", "component_type": "UnityEngine.Rigidbody",
               "idempotency_key": "c-remove"}),
    );
    assert!(!error);
    let (error, payload) = rig.tool_json(
        "tackle_prefab_instantiate",
        json!({"prefab_path": "Assets/Prefabs/Seraphine.prefab",
               "idempotency_key": "c-prefab"}),
    );
    assert!(!error, "{payload}");
    assert!(rig.exists("Seraphine"));
    let (error, _) = rig.tool("tackle_scene_save", json!({"idempotency_key": "c-save"}));
    assert!(!error);
}

#[test]
fn ok_script_create_compiles_and_patch_follows() {
    let mut rig = rig();
    let (error, payload) = rig.tool_json(
        "tackle_script_create",
        json!({"path": "Scripts/Hello.cs",
               "contents": "public class Hello { public int Value = 1; }\n",
               "idempotency_key": "s-create"}),
    );
    assert!(!error, "{payload}");
    assert_eq!(payload["compiled"], true, "{payload}");
    let file = rig.project.join("Assets/Scripts/Hello.cs");
    assert!(file.exists(), "the script landed under Assets");
    let (error, payload) = rig.tool_json(
        "tackle_script_patch",
        json!({"path": "Scripts/Hello.cs",
               "old_text": "Value = 1", "new_text": "Value = 2",
               "idempotency_key": "s-patch"}),
    );
    assert!(!error, "{payload}");
    assert_eq!(payload["compiled"], true);
    assert!(
        std::fs::read_to_string(&file)
            .expect("read back")
            .contains("Value = 2")
    );
}

#[test]
fn adv_script_patch_survives_refresh_disconnect() {
    // The live-editor failure this pins (B2 smoke, first run): a
    // script write triggers a domain reload, the session drops
    // under the server's own asset_refresh, and the call failed
    // with `editor disconnected before replying` even though the
    // patch had applied. The refresh is a nudge; the settle poll
    // is the verdict, so the call must ride the reload out.
    let mut rig = rig();
    let (error, payload) = rig.tool_json(
        "tackle_script_create",
        json!({"path": "Scripts/Reload.cs",
               "contents": "public class Reload { public int Value = 1; }\n",
               "idempotency_key": "r-create"}),
    );
    assert!(!error, "{payload}");
    assert_eq!(payload["compiled"], true, "{payload}");
    rig.model
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .drop_on_refresh = true;
    let (error, payload) = rig.tool_json(
        "tackle_script_patch",
        json!({"path": "Scripts/Reload.cs",
               "old_text": "Value = 1", "new_text": "Value = 2",
               "idempotency_key": "r-patch"}),
    );
    assert!(!error, "{payload}");
    assert_eq!(payload["patched"], true, "{payload}");
    assert_eq!(payload["compileSettled"], true, "{payload}");
    assert_eq!(payload["compiled"], true, "{payload}");
}

#[test]
fn adv_write_without_key_is_refused_before_the_editor() {
    let mut rig = rig();
    let before = rig.commands().len();
    let (error, text) = rig.tool("tackle_gameobject_create", json!({"name": "Nope"}));
    assert!(error);
    assert!(text.contains("idempotency_key"), "{text}");
    assert_eq!(rig.commands().len(), before, "no editor command was sent");
    assert!(!rig.exists("Nope"));
}

#[test]
fn adv_key_reuse_with_different_arguments_conflicts() {
    let mut rig = rig();
    rig.tool(
        "tackle_gameobject_create",
        json!({"name": "First", "idempotency_key": "shared"}),
    );
    let (error, text) = rig.tool(
        "tackle_gameobject_create",
        json!({"name": "Second", "idempotency_key": "shared"}),
    );
    assert!(error);
    assert!(
        text.contains("already applied with different arguments"),
        "{text}"
    );
    assert!(!rig.exists("Second"));
}

#[test]
fn adv_failed_calls_are_not_recorded() {
    let mut rig = rig();
    let (error, _) = rig.tool(
        "tackle_gameobject_delete",
        json!({"path": "Ghost", "idempotency_key": "retry-me"}),
    );
    assert!(error, "deleting a missing object fails in the editor");
    // The same key with different arguments must NOT conflict: the
    // failure was never recorded.
    let (error, _) = rig.tool(
        "tackle_gameobject_create",
        json!({"name": "Ghost", "idempotency_key": "retry-me"}),
    );
    assert!(!error);
    assert!(rig.exists("Ghost"));
}

#[test]
fn adv_batch_rolls_back_on_first_error() {
    let mut rig = rig();
    rig.tool(
        "tackle_gameobject_create",
        json!({"name": "Anchor", "idempotency_key": "b-anchor"}),
    );
    // The second op passes MCP validation but fails in the editor
    // (missing parent), so the batch must roll the first op back.
    let (error, text) = rig.tool(
        "tackle_batch_execute",
        json!({
            "idempotency_key": "batch-fail",
            "operations": [
                {"name": "tackle_gameobject_create", "arguments": {"name": "RolledBack"}},
                {"name": "tackle_gameobject_create",
                 "arguments": {"name": "Orphan", "parent_path": "Missing"}},
            ],
        }),
    );
    assert!(error, "{text}");
    assert!(text.contains("rolled back"), "{text}");
    assert!(!rig.exists("RolledBack"));
    assert!(rig.exists("Anchor"));
    // The undo stack holds only the Anchor step: one undo removes
    // the anchor, a second reports nothing to undo.
    let (_, payload) = rig.tool_json("tackle_undo", json!({"idempotency_key": "b-undo-1"}));
    assert_eq!(payload["performed"], true);
    assert!(!rig.exists("Anchor"));
    let (_, payload) = rig.tool_json("tackle_undo", json!({"idempotency_key": "b-undo-2"}));
    assert_eq!(payload["performed"], false);
}

#[test]
fn adv_batch_rejects_non_batchable_members_up_front() {
    let mut rig = rig();
    let before = rig.commands().len();
    let (error, text) = rig.tool(
        "tackle_batch_execute",
        json!({
            "idempotency_key": "batch-bad",
            "operations": [
                {"name": "tackle_script_create",
                 "arguments": {"path": "X.cs", "contents": "class X {}"}},
            ],
        }),
    );
    assert!(error);
    assert!(text.contains("batch op 0"), "{text}");
    assert_eq!(
        rig.commands().len(),
        before,
        "validation precedes any editor call"
    );
    // A member whose arguments fail validation also stops the batch
    // before the editor is touched.
    let (error, text) = rig.tool(
        "tackle_batch_execute",
        json!({
            "idempotency_key": "batch-bad-args",
            "operations": [
                {"name": "tackle_gameobject_create", "arguments": {"name": "Good"}},
                {"name": "tackle_gameobject_create", "arguments": {"name": "Bad/Name"}},
            ],
        }),
    );
    assert!(error);
    assert!(text.contains("batch op 1"), "{text}");
    assert_eq!(
        rig.commands().len(),
        before,
        "validation precedes any editor call"
    );
    assert!(!rig.exists("Good"));
}

#[test]
fn adv_script_paths_and_patch_matching_are_strict() {
    let mut rig = rig();
    for (index, path) in [
        "../Escape.cs",
        "Sub/../../Escape.cs",
        "/Abs.cs",
        "NoExt.txt",
    ]
    .iter()
    .enumerate()
    {
        let (error, text) = rig.tool(
            "tackle_script_create",
            json!({"path": path, "contents": "class X {}",
                   "idempotency_key": format!("bad-path-{index}")}),
        );
        assert!(error, "{path} must be refused: {text}");
    }
    rig.tool(
        "tackle_script_create",
        json!({"path": "Dup.cs", "contents": "int a = 7; int b = 7;",
               "idempotency_key": "dup-create"}),
    );
    let (error, text) = rig.tool(
        "tackle_script_create",
        json!({"path": "Dup.cs", "contents": "class Other {}",
               "idempotency_key": "dup-again"}),
    );
    assert!(error && text.contains("already exists"), "{text}");
    let (error, text) = rig.tool(
        "tackle_script_patch",
        json!({"path": "Dup.cs", "old_text": "= 7", "new_text": "= 8",
               "idempotency_key": "dup-patch"}),
    );
    assert!(error && text.contains("exactly once"), "{text}");
    let file = rig.project.join("Assets/Dup.cs");
    assert_eq!(
        std::fs::read_to_string(&file).expect("read"),
        "int a = 7; int b = 7;",
        "a refused patch changes nothing"
    );
}

#[test]
fn adv_broken_script_reports_diagnostics_not_success() {
    let mut rig = rig();
    let (error, payload) = rig.tool_json(
        "tackle_script_create",
        json!({"path": "Broken.cs",
               "contents": "SYNTAX_ERROR class Broken {",
               "idempotency_key": "broken-1"}),
    );
    assert!(!error, "the write itself succeeded: {payload}");
    assert_eq!(payload["compiled"], false);
    let diagnostics = payload["diagnostics"].as_array().expect("diagnostics");
    assert_eq!(diagnostics.len(), 1);
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .expect("message")
            .contains("error CS1002")
    );
}
