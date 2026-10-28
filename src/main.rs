//! tackle — MCP stdio server binary. Transport wiring only; the
//! logic lives in the `tackle` library crate.
//!
//! As of the B1 standardization (fleet D-113) the MCP framing runs
//! on rmcp 3.8 and speaks the modern protocol era only (2026-07-28):
//! sessions open with `server/discover` and every request carries
//! its protocol version in `_meta` — there is no `initialize`
//! handshake. Legacy contact is refused at the stdio front door
//! (see `tackle::guard`) with the typed `-32022` unsupported-version
//! error. The bridge's sessions, tools, and on-disk behavior are
//! unchanged from U1.
//!
//! Startup (unchanged): fail-closed environment config, bind the
//! loopback bridge listener, publish the per-run capability token
//! file (only after the bind, so the file never names a port we do
//! not hold), accept one authenticated editor session, then serve.

#![forbid(unsafe_code)]

use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::transport::async_rw::AsyncRwTransport;
use tackle::bridge::Bridge;
use tackle::capability::{self, Token};
use tackle::config::{self, Environment};
use tackle::guard;
use tackle::log;
use tackle::server::TackleServer;
use tokio::sync::Mutex;

/// Duplex buffer between the era guard and the SDK service. Request
/// lines can reach the 4 MiB line bound; the SDK consumes
/// incrementally, so this only needs to cover response bursts.
const DUPLEX_BUFFER_BYTES: usize = 1024 * 1024;

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
    serve(bridge).await?;
    Ok(())
}

/// Serve MCP until stdin EOF: the guard owns stdio, the SDK service
/// sits behind an in-process duplex, and a pump copies its responses
/// back to stdout through the same lock the guard writes with.
async fn serve(bridge: Bridge) -> std::io::Result<()> {
    let (client_side, server_side) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, client_write) = tokio::io::split(client_side);
    let stdout: guard::SharedStdout = Arc::new(Mutex::new(tokio::io::stdout()));

    let ordering = guard::Ordering::new();
    let handler = TackleServer::new(bridge);
    let serve = tokio::spawn(async move {
        let transport = AsyncRwTransport::new_server(server_read, server_write);
        match handler.serve(transport).await {
            Ok(running) => running
                .waiting()
                .await
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string())),
            // The client closed the connection before selecting a
            // lifecycle (a probe, a spawn check, a client that only
            // ever wanted the refusal): bare EOF is a clean end of
            // session.
            Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => Ok(()),
            Err(e) => Err(std::io::Error::other(e.to_string())),
        }
    });
    let mut pump = tokio::spawn(guard::pump(
        client_read,
        Arc::clone(&stdout),
        Arc::clone(&ordering),
    ));

    // When stdin reaches EOF the guard shuts the duplex write side
    // down, which ends the SDK service; the pump ends when the
    // service's transport drops. The pump wait is bounded as a
    // backstop: a response still in flight when the service ends
    // gets its chance to drain, but a wedged pump cannot hang exit.
    let guard_result = guard::run(tokio::io::stdin(), client_write, stdout, ordering).await;
    let serve_result = match serve.await {
        Ok(result) => result,
        Err(e) => Err(std::io::Error::other(e.to_string())),
    };
    if tokio::time::timeout(std::time::Duration::from_secs(2), &mut pump)
        .await
        .is_err()
    {
        pump.abort();
    }
    guard_result.and(serve_result)
}
