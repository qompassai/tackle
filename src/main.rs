//! tackle — MCP stdio server binary. Transport and tool wiring only; the
//! logic lives in the `tackle` library crate.
//!
//! Stage U1 (read core): six read-only tools proxying to one authenticated
//! Unity Editor session, plus five read-only resources mirroring the tools
//! that take no path argument. `tackle_gameobject_properties` has no resource
//! mirror: its path argument contains `/` separators, which a single
//! URI-template variable cannot express.

#![forbid(unsafe_code)]

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        ErrorCode, ErrorData, Implementation, ListResourcesResult, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
        ServerConfig,
    },
    tool, tool_handler, tool_router,
    transport::io::stdio,
};
use tackle::bridge::Bridge;
use tackle::capability::{self, Token};
use tackle::config::{self, Environment};
use tackle::log;

/// Largest hierarchy page or console read the MCP layer will ask for.
/// The editor enforces the same ceilings; this is the outer gate.
const PAGE_MAX: i64 = 500;
#[derive(Clone)]
pub struct Tackle {
    tool_router: ToolRouter<Self>,
    bridge: Bridge,
}

/// Paginated hierarchy arguments. Both fields are optional; omitting them
/// returns the first page at the editor's default ceiling.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct HierarchyParams {
    /// Zero-based offset into the preorder node list. Must be >= 0.
    #[serde(default)]
    pub offset: Option<i64>,
    /// Nodes per page, 1..=500. Omit for the editor default (500).
    #[serde(default)]
    pub limit: Option<i64>,
}

/// One GameObject, addressed by its hierarchy path, e.g.
/// `Seraphine (factory)/Hips`. Segments are matched by name; `/` separates
/// levels, so a name containing `/` cannot be addressed this way.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GameObjectParams {
    /// Hierarchy path; non-empty, no empty segments, at most 1024 bytes.
    pub path: String,
}

/// Console arguments. Omit `max_entries` for the newest 100 entries.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ConsoleParams {
    /// Entries to return, newest first, 1..=500.
    #[serde(default)]
    pub max_entries: Option<i64>,
}

/// One static resource: the URI it answers and the editor command behind it.
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

fn checked_page(offset: Option<i64>, limit: Option<i64>) -> Result<serde_json::Value, String> {
    let mut args = serde_json::Map::new();
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
    Ok(serde_json::Value::Object(args))
}

fn checked_console(max_entries: Option<i64>) -> Result<serde_json::Value, String> {
    let mut args = serde_json::Map::new();
    if let Some(max_entries) = max_entries {
        if !(1..=PAGE_MAX).contains(&max_entries) {
            return Err(format!("max_entries must be between 1 and {PAGE_MAX}"));
        }
        args.insert("maxEntries".to_string(), max_entries.into());
    }
    Ok(serde_json::Value::Object(args))
}

fn checked_path(path: &str) -> Result<serde_json::Value, String> {
    if path.is_empty() || path.len() > 1024 {
        return Err("path must be non-empty and at most 1024 bytes".to_string());
    }
    if path.split('/').any(str::is_empty) {
        return Err("path must not contain empty segments".to_string());
    }
    let mut args = serde_json::Map::new();
    args.insert("path".to_string(), path.into());
    Ok(serde_json::Value::Object(args))
}

#[tool_router]
impl Tackle {
    pub fn new(bridge: Bridge) -> Self {
        Self {
            tool_router: Self::tool_router(),
            bridge,
        }
    }

    async fn proxy(&self, command: &str) -> Result<String, String> {
        self.proxy_with(command, None).await
    }

    async fn proxy_with(
        &self,
        command: &str,
        args: Option<serde_json::Value>,
    ) -> Result<String, String> {
        let result = match &args {
            Some(args) => self.bridge.call_with_args(command, args.clone()).await,
            None => self.bridge.call(command).await,
        }
        .map_err(|error| {
            log(format!("{command}: {error}"));
            error.to_string()
        })?;
        log(format!("{command}: ok"));
        serde_json::to_string_pretty(&result).map_err(|error| error.to_string())
    }

    #[tool(
        description = "Read the connected Unity Editor's state: Unity version, project name and path, active scene name and path, isPlaying, isCompiling, editor command queue depth. Errors with `no editor connected` when no editor session is live.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn tackle_editor_state(&self) -> Result<String, String> {
        self.proxy("editor_state").await
    }

    #[tool(
        description = "Read the active scene's GameObject hierarchy from the connected Unity Editor, preorder: name, active flag, child count, depth. Paginated: offset is zero-based, limit is 1..500 (omit both for the first page at the editor ceiling of depth 4 and 500 nodes); totalCount and truncation are reported. Errors with `no editor connected` when no editor session is live.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn tackle_scene_hierarchy(
        &self,
        Parameters(params): Parameters<HierarchyParams>,
    ) -> Result<String, String> {
        let args = checked_page(params.offset, params.limit)?;
        self.proxy_with("scene_hierarchy", Some(args)).await
    }

    #[tool(
        description = "Read one GameObject's properties from the connected Unity Editor by hierarchy path (e.g. `Seraphine (factory)/Hips`): active flags, tag, layer, local and world transform, component type names, child count. Errors with `no editor connected` when no editor session is live, and with a plain editor error when the path resolves to nothing.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn tackle_gameobject_properties(
        &self,
        Parameters(params): Parameters<GameObjectParams>,
    ) -> Result<String, String> {
        let args = checked_path(&params.path)?;
        self.proxy_with("gameobject_properties", Some(args)).await
    }

    #[tool(
        description = "Read the connected Unity Editor's console entries, newest first: message, file, line, raw Unity LogEntry mode, identifier. max_entries is 1..500 (omit for the newest 100). Errors with `no editor connected` when no editor session is live.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn tackle_console(
        &self,
        Parameters(params): Parameters<ConsoleParams>,
    ) -> Result<String, String> {
        let args = checked_console(params.max_entries)?;
        self.proxy_with("console", Some(args)).await
    }

    #[tool(
        description = "Read the connected Unity Editor's project info: product and company name, bundle version, Unity version, project name and path, active build target, active scene, build scenes with their enabled flags. Errors with `no editor connected` when no editor session is live.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn tackle_project_info(&self) -> Result<String, String> {
        self.proxy("project_info").await
    }

    #[tool(
        description = "Read the connected Unity Editor's current selection: selected GameObjects with name, hierarchy path and active flag, the active object name, and the count of selected non-GameObject objects. Errors with `no editor connected` when no editor session is live.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn tackle_selection(&self) -> Result<String, String> {
        self.proxy("selection").await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Tackle {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(
            Implementation::new("tackle", env!("CARGO_PKG_VERSION")).with_title("Tackle"),
        )
        .with_instructions("tackle — live game-editor broker. Stage U1 read core: six read-only tools and five read-only resources proxying to one authenticated Unity Editor session over a loopback WebSocket.")
    }

    fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, ErrorData>> + Send + '_ {
        let resources = RESOURCES
            .iter()
            .map(|resource| {
                Resource::new(resource.uri, resource.name)
                    .with_description(resource.description)
                    .with_mime_type("application/json")
            })
            .collect();
        std::future::ready(Ok(ListResourcesResult::with_all_items(resources)))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResponse, ErrorData>> + Send + '_
    {
        let bridge = self.bridge.clone();
        async move {
            let Some(resource) = RESOURCES.iter().find(|r| r.uri == request.uri) else {
                return Err(ErrorData::resource_not_found(
                    format!("unknown resource `{}`", request.uri),
                    None,
                ));
            };
            let result = bridge.call(resource.command).await.map_err(|error| {
                log(format!("{}: {error}", resource.command));
                ErrorData::new(ErrorCode::INTERNAL_ERROR, error.to_string(), None)
            })?;
            let text = serde_json::to_string_pretty(&result).map_err(|error| {
                ErrorData::new(ErrorCode::INTERNAL_ERROR, error.to_string(), None)
            })?;
            Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
                vec![ResourceContents::text(text, request.uri).with_mime_type("application/json")],
            )))
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Fail closed: no config, no listener, no capability file, no service.
    let port = std::env::var(config::PORT_ENV).ok();
    let state_dir = std::env::var(config::STATE_DIR_ENV).ok();
    let xdg_state_home = std::env::var("XDG_STATE_HOME").ok();
    let home = std::env::var("HOME").ok();
    let config = config::resolve(Environment {
        port: port.as_deref(),
        state_dir: state_dir.as_deref(),
        xdg_state_home: xdg_state_home.as_deref(),
        home: home.as_deref(),
    })?;
    let token = Token::generate()?;
    let listener = Bridge::bind(config.port)
        .await
        .map_err(|error| format!("cannot bind 127.0.0.1:{}: {error}", config.port))?;
    // Publish only after binding, so the file never names a port we lack.
    let file = capability::publish(&config.state_dir, config.port, &token)?;
    log(format!(
        "v{} listening on 127.0.0.1:{}; capability file {}",
        env!("CARGO_PKG_VERSION"),
        config.port,
        file.display()
    ));
    let bridge = Bridge::new(token);
    let _listener_task = bridge.spawn_listener(listener);
    let service = Tackle::new(bridge).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
