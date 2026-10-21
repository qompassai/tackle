//! Loopback WebSocket bridge to one editor session.
//!
//! Contract:
//! - Listens on 127.0.0.1 only. The editor connects to `ws://127.0.0.1:P/editor`
//!   presenting `Authorization: Bearer <token>`. A wrong or missing token, a
//!   wrong path, any `Origin` header (browser pages must never reach this
//!   socket), or a non-WebSocket request is refused at the HTTP upgrade and
//!   logged. There is no unauthenticated mode.
//! - One session at a time. A newly authenticated editor replaces the old
//!   session (domain reloads reconnect before the old socket is reaped);
//!   a generation number keeps the stale task from clearing the new slot.
//! - One command in flight per session, in arrival order: callers queue on a
//!   bounded channel ([`QUEUE_CAPACITY`]); a full queue is rejected, not
//!   blocked. Each call waits at most [`CALL_DEADLINE`].
//! - Frames above [`MESSAGE_BYTES_MAX`] end the session. A reply that is not
//!   the expected JSON shape is a protocol error and ends the session.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::capability::Token;
use crate::log;

/// Largest WebSocket message or frame accepted from the editor, in bytes.
pub const MESSAGE_BYTES_MAX: usize = 1024 * 1024;
/// Calls queued behind the one in flight before new calls are rejected.
pub const QUEUE_CAPACITY: usize = 8;
/// Longest wait for one editor reply.
pub const CALL_DEADLINE: Duration = Duration::from_secs(15);
/// Longest a TCP peer may take to finish the WebSocket upgrade.
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);
/// The only accepted upgrade path.
pub const EDITOR_PATH: &str = "/editor";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CallError {
    #[error("no editor connected")]
    NoEditor,
    #[error("editor busy: {QUEUE_CAPACITY} commands already queued")]
    Busy,
    #[error("editor disconnected before replying")]
    Disconnected,
    #[error("editor did not reply within {}s", CALL_DEADLINE.as_secs())]
    Timeout,
    #[error("editor error: {0}")]
    Editor(String),
    #[error("editor protocol violation: {0}")]
    Protocol(String),
}

struct Pending {
    command: String,
    reply: oneshot::Sender<Result<Value, CallError>>,
}

struct Session {
    generation: u64,
    sender: mpsc::Sender<Pending>,
}

struct Inner {
    token: Token,
    /// Protects only the slot swap; never held across `.await`.
    session: Mutex<Option<Session>>,
    next_generation: AtomicU64,
}

/// Handle shared by the MCP tools and the listener task.
#[derive(Clone)]
pub struct Bridge {
    inner: Arc<Inner>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    id: u64,
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

impl Bridge {
    pub fn new(token: Token) -> Bridge {
        Bridge {
            inner: Arc::new(Inner {
                token,
                session: Mutex::new(None),
                next_generation: AtomicU64::new(1),
            }),
        }
    }

    /// Bind the loopback listener. Never binds a non-loopback address.
    pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
        TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await
    }

    /// Accept connections until the runtime ends. The returned task owns
    /// the listener; each connection task owns its socket.
    pub fn spawn_listener(&self, listener: TcpListener) -> tokio::task::JoinHandle<()> {
        let bridge = self.clone();
        tokio::spawn(async move {
            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        log(format!("accept failed: {error}"));
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let bridge = bridge.clone();
                tokio::spawn(async move { bridge.serve_connection(stream, peer).await });
            }
        })
    }

    pub fn connected(&self) -> bool {
        self.slot().is_some()
    }

    /// Send one command to the connected editor and wait for its result.
    pub async fn call(&self, command: &str) -> Result<Value, CallError> {
        let sender = match self.slot().as_ref() {
            Some(session) => session.sender.clone(),
            None => return Err(CallError::NoEditor),
        };
        let (reply, receive) = oneshot::channel();
        let pending = Pending {
            command: command.to_string(),
            reply,
        };
        sender.try_send(pending).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => CallError::Busy,
            mpsc::error::TrySendError::Closed(_) => CallError::NoEditor,
        })?;
        receive.await.unwrap_or(Err(CallError::Disconnected))
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Session>> {
        // A poisoned slot only means a panic mid-swap; the Option is still valid.
        self.inner
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn serve_connection(self, stream: TcpStream, peer: SocketAddr) {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MESSAGE_BYTES_MAX))
            .max_frame_size(Some(MESSAGE_BYTES_MAX));
        let token = self.inner.token.clone();
        // tungstenite's Callback trait fixes the Err type to ErrorResponse.
        #[allow(clippy::result_large_err)]
        let callback = move |request: &Request, response: Response| {
            authorize(&token, request)
                .map(|()| response)
                .map_err(|(status, reason)| {
                    log(format!("refused editor connection from {peer}: {reason}"));
                    let mut refusal = ErrorResponse::new(Some(reason.to_string()));
                    *refusal.status_mut() = status;
                    refusal
                })
        };
        let upgrade =
            tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(config));
        let socket = match tokio::time::timeout(HANDSHAKE_DEADLINE, upgrade).await {
            Ok(Ok(socket)) => socket,
            Ok(Err(error)) => {
                log(format!("handshake with {peer} failed: {error}"));
                return;
            }
            Err(_) => {
                log(format!("handshake with {peer} timed out"));
                return;
            }
        };
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let replaced = self.slot().replace(Session { generation, sender });
        if let Some(old) = replaced {
            log(format!(
                "session {} replaced by {generation}",
                old.generation
            ));
        }
        log(format!("editor session {generation} connected from {peer}"));
        let reason = run_session(socket, receiver).await;
        let mut slot = self.slot();
        if slot
            .as_ref()
            .is_some_and(|session| session.generation == generation)
        {
            *slot = None;
        }
        drop(slot);
        log(format!("editor session {generation} ended: {reason}"));
    }
}

/// Upgrade-time checks. Order matters only for the logged reason.
fn authorize(token: &Token, request: &Request) -> Result<(), (StatusCode, &'static str)> {
    if request.uri().path() != EDITOR_PATH {
        return Err((StatusCode::NOT_FOUND, "wrong path"));
    }
    if request.headers().contains_key("origin") {
        return Err((StatusCode::FORBIDDEN, "browser origin header present"));
    }
    let Some(value) = request.headers().get("authorization") else {
        return Err((StatusCode::UNAUTHORIZED, "missing token"));
    };
    let presented = value
        .to_str()
        .ok()
        .and_then(|text| text.strip_prefix("Bearer "));
    match presented {
        Some(presented) if token.matches(presented) => Ok(()),
        _ => Err((StatusCode::UNAUTHORIZED, "wrong token")),
    }
}

/// Drive one session until the editor leaves, the slot is replaced, or a
/// fatal error. Returns the reason for the log.
async fn run_session(
    mut socket: WebSocketStream<TcpStream>,
    mut receiver: mpsc::Receiver<Pending>,
) -> String {
    let mut request_id: u64 = 0;
    loop {
        tokio::select! {
            pending = receiver.recv() => {
                let Some(pending) = pending else {
                    // Best-effort close: the session is being replaced.
                    let _ = socket.close(None).await;
                    return "replaced".to_string();
                };
                request_id = request_id.checked_add(1).expect("u64 request ids do not wrap");
                let result = exchange(&mut socket, request_id, &pending.command).await;
                let fatal = match &result {
                    Err(CallError::Disconnected) => Some("disconnected mid-call".to_string()),
                    Err(CallError::Protocol(reason)) => Some(format!("protocol violation: {reason}")),
                    _ => None,
                };
                // The caller may have gone away; its result is then moot.
                let _ = pending.reply.send(result);
                if let Some(reason) = fatal {
                    return reason;
                }
            }
            frame = socket.next() => match frame {
                None => return "closed".to_string(),
                Some(Err(error)) => return format!("read error: {error}"),
                Some(Ok(Message::Close(_))) => return "closed by editor".to_string(),
                Some(Ok(Message::Text(_) | Message::Binary(_))) => {
                    log("ignored unsolicited editor frame");
                }
                Some(Ok(_)) => {}
            },
        }
    }
}

/// Send one command and read until its reply. Replies to earlier, timed-out
/// ids are discarded so a late answer can never be returned for a new call.
async fn exchange(
    socket: &mut WebSocketStream<TcpStream>,
    request_id: u64,
    command: &str,
) -> Result<Value, CallError> {
    let frame = serde_json::json!({ "id": request_id, "command": command }).to_string();
    if socket.send(Message::text(frame)).await.is_err() {
        return Err(CallError::Disconnected);
    }
    let deadline = tokio::time::Instant::now() + CALL_DEADLINE;
    loop {
        let frame = match tokio::time::timeout_at(deadline, socket.next()).await {
            Err(_) => return Err(CallError::Timeout),
            Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => {
                return Err(CallError::Disconnected);
            }
            Ok(Some(Ok(frame))) => frame,
        };
        let text = match frame {
            Message::Text(text) => text,
            Message::Binary(_) => return Err(CallError::Protocol("binary frame".to_string())),
            _ => continue,
        };
        let reply: Reply = serde_json::from_str(text.as_str())
            .map_err(|error| CallError::Protocol(format!("malformed reply: {error}")))?;
        if reply.id < request_id {
            log(format!("discarded stale reply for request {}", reply.id));
            continue;
        }
        if reply.id > request_id {
            return Err(CallError::Protocol(format!(
                "reply for unsent request {}",
                reply.id
            )));
        }
        return match (reply.ok, reply.result, reply.error) {
            (true, Some(result), None) => Ok(result),
            (false, None, Some(error)) => Err(CallError::Editor(error)),
            _ => Err(CallError::Protocol(
                "reply must carry exactly one of result/error".into(),
            )),
        };
    }
}
