//! tackle — a live game-editor broker served over MCP stdio.
//!
//! Stage U0 (spike): fail-closed environment config ([`config`]), the
//! per-run capability token and its private file ([`capability`]), and the
//! loopback WebSocket bridge holding one authenticated editor session
//! ([`bridge`]). The MCP transport lives in the binary.

#![forbid(unsafe_code)]

pub mod bridge;
pub mod capability;
pub mod config;

/// Write one server log line to stderr (stdout carries MCP only). Callers
/// must never pass token material.
pub fn log(line: impl std::fmt::Display) {
    eprintln!("tackle: {line}");
}
