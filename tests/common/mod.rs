//! Fake editor: a real WebSocket client against a real bridge listener.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tackle::bridge::Bridge;
use tackle::capability::Token;
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub type Editor = WebSocketStream<TcpStream>;

pub fn token() -> Token {
    Token::from_string("a1".repeat(32))
}

/// A bridge listening on an ephemeral loopback port.
pub async fn start() -> (Bridge, SocketAddr) {
    let listener = Bridge::bind(0).await.expect("bind");
    let address = listener.local_addr().expect("address");
    let bridge = Bridge::new(token());
    bridge.spawn_listener(listener);
    (bridge, address)
}

/// Upgrade with the given path and headers; Err carries the HTTP status.
pub async fn connect(
    address: SocketAddr,
    path: &str,
    headers: &[(&'static str, &str)],
) -> Result<Editor, u16> {
    let mut request = format!("ws://{address}{path}")
        .into_client_request()
        .expect("request");
    for (name, value) in headers {
        request
            .headers_mut()
            .insert(*name, HeaderValue::from_str(value).expect("header"));
    }
    let stream = TcpStream::connect(address).await.expect("tcp connect");
    match tokio_tungstenite::client_async(request, stream).await {
        Ok((socket, _)) => Ok(socket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(response.status().as_u16())
        }
        Err(other) => panic!("unexpected client error: {other}"),
    }
}

pub async fn connect_authorized(address: SocketAddr) -> Editor {
    let bearer = format!("Bearer {}", token().expose());
    connect(address, "/editor", &[("authorization", &bearer)])
        .await
        .expect("authorized upgrade")
}

/// Poll until the bridge's connected flag equals `want` (2 s budget).
pub async fn wait_connected(bridge: &Bridge, want: bool) {
    for _ in 0..200 {
        if bridge.connected() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("bridge.connected() never became {want}");
}

/// Read the next command frame the bridge sent.
pub async fn next_command(editor: &mut Editor) -> Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), editor.next())
            .await
            .expect("command within deadline")
            .expect("stream open")
            .expect("frame");
        if let Message::Text(text) = frame {
            return serde_json::from_str(text.as_str()).expect("command json");
        }
    }
}

pub async fn send_text(editor: &mut Editor, text: String) {
    editor.send(Message::text(text)).await.expect("send");
}
