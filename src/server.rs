//! tackle's MCP handler on the raw rmcp `ServerHandler` surface.
//!
//! The tool definitions are data, not macro output: `tools.json`
//! carries the definitions exactly as served, parsed into rmcp
//! `Tool` values at list time (the fleet's raw-tools pattern, proven
//! in ache), so the served surface is a reviewed artifact rather
//! than a codegen side effect. Dispatch is hand-written per tool:
//! every argument is validated in this layer before any editor call
//! (the U0/U1 contract), reads proxy to the one authenticated editor
//! session over the bridge, and writes (U2) additionally pass the
//! decide() gate and the idempotency store before the bridge.
//!
//! Result shaping reproduces U1 exactly: a successful call's text is
//! the editor payload pretty-printed; a failure's text is the
//! bridge's error string; argument refusals carry the U1 validator
//! messages verbatim.

use std::borrow::Cow;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode, ErrorData,
    Implementation, InitializeResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::bridge::Bridge;
use crate::log;

/// The served tool definitions, as data (see the module docs).
const TOOLS_JSON: &str = include_str!("tools.json");

/// Largest hierarchy page or console read the MCP layer will ask
/// for. The editor enforces the same ceilings; this is the outer
/// gate.
pub(crate) const PAGE_MAX: i64 = 500;

/// The tool definitions exactly as served, parsed from the data
/// table. A parse failure yields an empty surface rather than a
/// panic (the table is a compile-time artifact; the era tests pin
/// its shape).
pub fn tool_list() -> Value {
    serde_json::from_str(TOOLS_JSON).unwrap_or(Value::Null)
}

fn build_tools() -> Vec<Tool> {
    let Value::Array(definitions) = tool_list() else {
        return Vec::new();
    };
    definitions
        .iter()
        .filter_map(|definition| {
            let name = definition.get("name")?.as_str()?.to_string();
            let description = definition
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let schema = definition.get("inputSchema")?.as_object()?.clone();
            let mut tool = Tool::new(name, description, schema);
            if let Some(annotations) = definition.get("annotations") {
                let text = |key: &str| {
                    annotations
                        .get(key)
                        .and_then(Value::as_str)
                        .map(str::to_string)
                };
                let flag = |key: &str| annotations.get(key).and_then(Value::as_bool);
                tool.annotations = Some(ToolAnnotations::from_raw(
                    text("title"),
                    flag("readOnlyHint"),
                    flag("destructiveHint"),
                    flag("idempotentHint"),
                    flag("openWorldHint"),
                ));
            }
            Some(tool)
        })
        .collect()
}

/// One static resource: the URI it answers and the editor command
/// behind it. Names and descriptions are the U1 served surface.
struct StaticResource {
    uri: &'static str,
    name: &'static str,
    description: &'static str,
    command: &'static str,
}

const RESOURCES: [StaticResource; 5] = [
    StaticResource {
        uri: "tackle://editor/state",
        name: "editor-state",
        description: "Connected Unity Editor state (same data as tackle_editor_state).",
        command: "editor_state",
    },
    StaticResource {
        uri: "tackle://editor/scene-hierarchy",
        name: "scene-hierarchy",
        description: "Active scene hierarchy, first page at the default ceiling (same data as tackle_scene_hierarchy with no arguments).",
        command: "scene_hierarchy",
    },
    StaticResource {
        uri: "tackle://editor/project-info",
        name: "project-info",
        description: "Project identity, build target and build scenes (same data as tackle_project_info).",
        command: "project_info",
    },
    StaticResource {
        uri: "tackle://editor/console",
        name: "console",
        description: "Newest 100 editor console entries (same data as tackle_console with no arguments).",
        command: "console",
    },
    StaticResource {
        uri: "tackle://editor/selection",
        name: "selection",
        description: "Current editor selection (same data as tackle_selection).",
        command: "selection",
    },
];

fn text_result(text: String, is_error: bool) -> CallToolResult {
    if is_error {
        CallToolResult::error(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::success(vec![ContentBlock::text(text)])
    }
}

/// Deserialize tool arguments the way the U1 macro wrapper did: a
/// shape failure is a tool error carrying the serde detail.
fn parse_args<T: for<'de> Deserialize<'de>>(arguments: &Map<String, Value>) -> Result<T, String> {
    serde_json::from_value(Value::Object(arguments.clone()))
        .map_err(|error| format!("invalid arguments: {error}"))
}

#[derive(Deserialize)]
struct HierarchyParams {
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Deserialize)]
struct GameObjectParams {
    path: String,
}

#[derive(Deserialize)]
struct ConsoleParams {
    #[serde(default)]
    max_entries: Option<i64>,
}

/// The U1 pagination validator: builds the editor args object,
/// omitting unset fields.
fn checked_page(offset: Option<i64>, limit: Option<i64>) -> Result<Value, String> {
    let mut args = Map::new();
    if let Some(offset) = offset {
        if offset < 0 {
            return Err("offset must be >= 0".to_string());
        }
        args.insert("offset".to_string(), offset.into());
    }
    if let Some(limit) = limit {
        if !(1..=PAGE_MAX).contains(&limit) {
            return Err(format!("limit must be between 1 and {PAGE_MAX}"));
        }
        args.insert("limit".to_string(), limit.into());
    }
    Ok(Value::Object(args))
}

/// The U1 console validator: builds the editor args object with the
/// editor's camelCase field name.
fn checked_console(max_entries: Option<i64>) -> Result<Value, String> {
    let mut args = Map::new();
    if let Some(max_entries) = max_entries {
        if !(1..=PAGE_MAX).contains(&max_entries) {
            return Err(format!("max_entries must be between 1 and {PAGE_MAX}"));
        }
        args.insert("maxEntries".to_string(), max_entries.into());
    }
    Ok(Value::Object(args))
}

/// The U1 hierarchy-path validator.
fn checked_path(path: &str) -> Result<Value, String> {
    if path.is_empty() || path.len() > 1024 {
        return Err("path must be non-empty and at most 1024 bytes".to_string());
    }
    if path.split('/').any(str::is_empty) {
        return Err("path must not contain empty segments".to_string());
    }
    let mut args = Map::new();
    args.insert("path".to_string(), path.into());
    Ok(Value::Object(args))
}

pub struct TackleServer {
    bridge: Bridge,
    writes: crate::writes::WriteCore,
}

impl TackleServer {
    pub fn new(bridge: Bridge) -> Self {
        Self {
            writes: crate::writes::WriteCore::new(bridge.clone()),
            bridge,
        }
    }

    async fn proxy(&self, command: &str) -> CallToolResult {
        self.proxy_with(command, None).await
    }

    async fn proxy_with(&self, command: &str, args: Option<Value>) -> CallToolResult {
        let result = match &args {
            Some(args) => self.bridge.call_with_args(command, args.clone()).await,
            None => self.bridge.call(command).await,
        };
        match result {
            Ok(payload) => {
                log(format!("{command}: ok"));
                match serde_json::to_string_pretty(&payload) {
                    Ok(text) => text_result(text, false),
                    Err(error) => text_result(error.to_string(), true),
                }
            }
            Err(error) => {
                log(format!("{command}: {error}"));
                text_result(error.to_string(), true)
            }
        }
    }

    /// Dispatch one read tool by name. Returns `None` when the name
    /// is not a read tool (the caller tries the write surface next).
    async fn dispatch_read(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Option<CallToolResult> {
        let result = match name {
            "tackle_console" => {
                let parsed: ConsoleParams = match parse_args(arguments) {
                    Ok(parsed) => parsed,
                    Err(message) => return Some(text_result(message, true)),
                };
                match checked_console(parsed.max_entries) {
                    Ok(args) => self.proxy_with("console", Some(args)).await,
                    Err(message) => text_result(message, true),
                }
            }
            "tackle_editor_state" => self.proxy("editor_state").await,
            "tackle_gameobject_properties" => {
                let parsed: GameObjectParams = match parse_args(arguments) {
                    Ok(parsed) => parsed,
                    Err(message) => return Some(text_result(message, true)),
                };
                match checked_path(&parsed.path) {
                    Ok(args) => self.proxy_with("gameobject_properties", Some(args)).await,
                    Err(message) => text_result(message, true),
                }
            }
            "tackle_project_info" => self.proxy("project_info").await,
            "tackle_scene_hierarchy" => {
                let parsed: HierarchyParams = match parse_args(arguments) {
                    Ok(parsed) => parsed,
                    Err(message) => return Some(text_result(message, true)),
                };
                match checked_page(parsed.offset, parsed.limit) {
                    Ok(args) => self.proxy_with("scene_hierarchy", Some(args)).await,
                    Err(message) => text_result(message, true),
                }
            }
            "tackle_selection" => self.proxy("selection").await,
            _ => return None,
        };
        Some(result)
    }
}

impl ServerHandler for TackleServer {
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&[ProtocolVersion::V_2026_07_28])
    }

    fn get_info(&self) -> InitializeResult {
        let mut info = InitializeResult::default();
        info.protocol_version = ProtocolVersion::V_2026_07_28;
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.server_info =
            Implementation::new("tackle", env!("CARGO_PKG_VERSION")).with_title("Tackle");
        info.instructions = Some(
            "tackle — live game-editor broker. Read core: six read-only tools and five read-only \
             resources proxying to one authenticated Unity Editor session over a loopback \
             WebSocket. Write core (U2): twelve write tools — undo-grouped GameObject, component \
             and prefab mutations, atomic batch, scene save, undo/redo, and bounded-filesystem \
             script create/patch with compile read-back. Every write tool requires an \
             idempotency_key (1..=128 chars, [A-Za-z0-9._:-]): a retry with the same key and \
             the same other arguments replays the original result without re-applying the \
             mutation; the same key with different arguments is refused; failed calls are not \
             recorded and may be retried under the same key."
                .to_string(),
        );
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: build_tools(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = request.arguments.unwrap_or_default();
        if let Some(result) = self.dispatch_read(&request.name, &arguments).await {
            return Ok(result.into());
        }
        match self.writes.dispatch(&request.name, &arguments).await {
            crate::writes::Outcome::Done(text, is_error) => Ok(text_result(text, is_error).into()),
            crate::writes::Outcome::NotWrite => {
                Ok(text_result(format!("unknown tool: {}", request.name), true).into())
            }
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = RESOURCES
            .iter()
            .map(|resource| {
                Resource::new(resource.uri, resource.name)
                    .with_description(resource.description)
                    .with_mime_type("application/json")
            })
            .collect();
        Ok(ListResourcesResult {
            resources,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let Some(resource) = RESOURCES.iter().find(|r| r.uri == request.uri) else {
            return Err(ErrorData::resource_not_found(
                format!("unknown resource `{}`", request.uri),
                None,
            ));
        };
        let result = self.bridge.call(resource.command).await.map_err(|error| {
            log(format!("{}: {error}", resource.command));
            ErrorData::new(ErrorCode::INTERNAL_ERROR, error.to_string(), None)
        })?;
        let text = serde_json::to_string_pretty(&result)
            .map_err(|error| ErrorData::new(ErrorCode::INTERNAL_ERROR, error.to_string(), None))?;
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![ResourceContents::text(text, request.uri).with_mime_type("application/json")],
        )))
    }
}
