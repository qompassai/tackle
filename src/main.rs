//! tackle — MCP stdio server binary. Transport and tool wiring only; the
//! logic lives in the `tackle` library crate.

#![forbid(unsafe_code)]

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
    transport::io::stdio,
};
use tackle::bridge::Bridge;
use tackle::capability::{self, Token};
use tackle::config::{self, Environment};
use tackle::log;

#[derive(Clone)]
pub struct Tackle {
    tool_router: ToolRouter<Self>,
    bridge: Bridge,
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
        let result = self.bridge.call(command).await.map_err(|error| {
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
        description = "Read the active scene's GameObject hierarchy from the connected Unity Editor, preorder: name, active flag, child count, depth. Capped at depth 4 and 500 nodes; truncation is reported. Errors with `no editor connected` when no editor session is live.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn tackle_scene_hierarchy(&self) -> Result<String, String> {
        self.proxy("scene_hierarchy").await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Tackle {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("tackle", env!("CARGO_PKG_VERSION")).with_title("Tackle"),
            )
            .with_instructions("tackle — live game-editor broker. Stage U0 spike: tackle_editor_state and tackle_scene_hierarchy proxy read-only to one authenticated Unity Editor session over a loopback WebSocket.")
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
