//! tackle — a live game-editor broker served over MCP stdio.
//!
//! Stage U1 (read core): fail-closed environment config ([`config`]), the
//! per-run capability token and its private file ([`capability`]), and the
//! loopback WebSocket bridge holding one authenticated editor session
//! ([`bridge`]). The MCP transport lives in the binary: a byte-level
//! protocol-era [`guard`] in front of the rmcp handler in [`server`].

#![forbid(unsafe_code)]

pub mod bridge;
pub mod capability;
pub mod config;
pub mod decide;
pub mod guard;
pub mod idempotency;
pub mod server;
pub mod writes;

/// Write one server log line to stderr (stdout carries MCP only). Callers
/// must never pass token material.
pub fn log(line: impl std::fmt::Display) {
    eprintln!("tackle: {line}");
}
