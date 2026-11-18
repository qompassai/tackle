//! Bridge: handshake, session, serialization, and refusal paths.

mod common;

use std::time::Duration;

use common::{connect, connect_authorized, next_command, send_text, start, wait_connected};
use serde_json::json;
use tackle::bridge::{Bridge, CallError, MESSAGE_BYTES_MAX};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn ok_authenticated_round_trip() {
    let (bridge, address) = start().await;
    let mut editor = connect_authorized(address).await;
    wait_connected(&bridge, true).await;
    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.call("editor_state").await }
    });
    let command = next_command(&mut editor).await;
    assert_eq!(command, json!({"id": 1, "command": "editor_state"}));
    let reply = json!({"id": 1, "ok": true, "result": {"unityVersion": "6000.6.6f1"}});
    send_text(&mut editor, reply.to_string()).await;
    let result = call.await.expect("join").expect("call ok");
    assert_eq!(result["unityVersion"], "6000.6.6f1");
}

#[tokio::test]
async fn ok_editor_error_is_relayed_and_session_survives() {
    let (bridge, address) = start().await;
    let mut editor = connect_authorized(address).await;
    wait_connected(&bridge, true).await;
    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.call("nope").await }
    });
    next_command(&mut editor).await;
    let reply = json!({"id": 1, "ok": false, "error": "unknown command `nope`"});
    send_text(&mut editor, reply.to_string()).await;
    let error = call.await.expect("join").expect_err("editor error");
    assert_eq!(
        error,
        CallError::Editor("unknown command `nope`".to_string())
    );
    assert!(bridge.connected());
}

#[tokio::test]
async fn ok_commands_are_serialized_in_arrival_order() {
    let (bridge, address) = start().await;
    let mut editor = connect_authorized(address).await;
    wait_connected(&bridge, true).await;
    let first = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.call("editor_state").await }
    });
    let first_command = next_command(&mut editor).await;
    let second = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.call("scene_hierarchy").await }
    });
    // While request 1 is unanswered, request 2 must not be on the wire.
    let early = tokio::time::timeout(Duration::from_millis(300), next_command(&mut editor)).await;
    assert!(early.is_err(), "second command sent before first reply");
    assert_eq!(first_command["command"], "editor_state");
    send_text(
        &mut editor,
        json!({"id": 1, "ok": true, "result": 1}).to_string(),
    )
    .await;
    let second_command = next_command(&mut editor).await;
    assert_eq!(
        second_command,
        json!({"id": 2, "command": "scene_hierarchy"})
    );
    send_text(
        &mut editor,
        json!({"id": 2, "ok": true, "result": 2}).to_string(),
    )
    .await;
    assert_eq!(first.await.expect("join"), Ok(json!(1)));
    assert_eq!(second.await.expect("join"), Ok(json!(2)));
}

#[tokio::test]
async fn ok_reconnect_replaces_session() {
    let (bridge, address) = start().await;
    let _old = connect_authorized(address).await;
    wait_connected(&bridge, true).await;
    let mut new = connect_authorized(address).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.call("editor_state").await }
    });
    let command = next_command(&mut new).await;
    send_text(
        &mut new,
        json!({"id": command["id"], "ok": true, "result": "new"}).to_string(),
    )
    .await;
    assert_eq!(call.await.expect("join"), Ok(json!("new")));
}

#[tokio::test]
async fn ok_disconnect_clears_session() {
    let (bridge, address) = start().await;
    let editor = connect_authorized(address).await;
    wait_connected(&bridge, true).await;
    drop(editor);
    wait_connected(&bridge, false).await;
    assert_eq!(bridge.call("editor_state").await, Err(CallError::NoEditor));
}

#[tokio::test]
async fn adv_no_editor_connected() {
    let (bridge, _address) = start().await;
    assert!(!bridge.connected());
    let error = bridge.call("editor_state").await.expect_err("no editor");
    assert_eq!(error, CallError::NoEditor);
    assert_eq!(error.to_string(), "no editor connected");
}

#[tokio::test]
async fn adv_wrong_token_refused() {
    let (bridge, address) = start().await;
    let wrong = format!("Bearer {}", "b2".repeat(32));
    let status = connect(address, "/editor", &[("authorization", &wrong)]).await;
    assert_eq!(status.err(), Some(401));
    assert!(!bridge.connected());
}

#[tokio::test]
async fn adv_token_prefix_and_scheme_variants_refused() {
    let (bridge, address) = start().await;
    let full = common::token().expose().to_string();
    for value in [
        format!("Bearer {}", &full[..full.len() - 1]),
        format!("Bearer {full}x"),
        format!("bearer {full}"),
        full.clone(),
        "Bearer ".to_string(),
    ] {
        let status = connect(address, "/editor", &[("authorization", &value)]).await;
        assert_eq!(status.err(), Some(401), "accepted {value:?}");
    }
    assert!(!bridge.connected());
}

#[tokio::test]
async fn adv_missing_token_refused() {
    let (bridge, address) = start().await;
    assert_eq!(connect(address, "/editor", &[]).await.err(), Some(401));
    assert!(!bridge.connected());
}

#[tokio::test]
async fn adv_browser_origin_refused_even_with_token() {
    let (bridge, address) = start().await;
    let bearer = format!("Bearer {}", common::token().expose());
    let headers = [
        ("authorization", bearer.as_str()),
        ("origin", "http://evil.example"),
    ];
    assert_eq!(connect(address, "/editor", &headers).await.err(), Some(403));
    assert!(!bridge.connected());
}

#[tokio::test]
async fn adv_wrong_path_refused() {
    let (bridge, address) = start().await;
    let bearer = format!("Bearer {}", common::token().expose());
    let status = connect(address, "/", &[("authorization", &bearer)]).await;
    assert_eq!(status.err(), Some(404));
    assert!(!bridge.connected());
}

#[tokio::test]
async fn adv_malformed_upgrade_bytes_refused() {
    let (bridge, address) = start().await;
    let mut stream = tokio::net::TcpStream::connect(address).await.expect("tcp");
    stream
        .write_all(b"\x16\x03\x01garbage not http\r\n\r\n")
        .await
        .expect("write");
    let mut buffer = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(6), stream.read_to_end(&mut buffer)).await;
    assert!(read.is_ok(), "server must close the socket");
    assert!(!bridge.connected());
}

#[tokio::test]
async fn adv_oversized_message_ends_session() {
    let (bridge, address) = start().await;
    let mut editor = connect_authorized(address).await;
    wait_connected(&bridge, true).await;
    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.call("editor_state").await }
    });
    next_command(&mut editor).await;
    let huge = format!(
        r#"{{"id":1,"ok":true,"result":"{}"}}"#,
        "x".repeat(MESSAGE_BYTES_MAX)
    );
    // The server may reset mid-send; the outcome is judged server-side.
    let _ = futures_util::SinkExt::send(
        &mut editor,
        tokio_tungstenite::tungstenite::Message::text(huge),
    )
    .await;
    assert_eq!(call.await.expect("join"), Err(CallError::Disconnected));
    wait_connected(&bridge, false).await;
}

#[tokio::test]
async fn adv_malformed_reply_is_protocol_error_and_ends_session() {
    let (bridge, address) = start().await;
    let mut editor = connect_authorized(address).await;
    wait_connected(&bridge, true).await;
    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.call("editor_state").await }
    });
    next_command(&mut editor).await;
    send_text(&mut editor, "{not json".to_string()).await;
    let error = call.await.expect("join").expect_err("protocol");
    assert!(matches!(error, CallError::Protocol(_)), "{error:?}");
    wait_connected(&bridge, false).await;
}

#[tokio::test]
async fn adv_reply_shape_violations_are_protocol_errors() {
    let replies = [
        json!({"id": 9, "ok": true, "result": 1}),
        json!({"id": 1, "ok": true}),
        json!({"id": 1, "ok": true, "result": 1, "error": "both"}),
        json!({"id": 1, "ok": true, "result": 1, "extra": true}),
    ];
    for reply in replies {
        let (bridge, address) = start().await;
        let mut editor = connect_authorized(address).await;
        wait_connected(&bridge, true).await;
        let call = tokio::spawn({
            let bridge: Bridge = bridge.clone();
            async move { bridge.call("editor_state").await }
        });
        next_command(&mut editor).await;
        send_text(&mut editor, reply.to_string()).await;
        let error = call.await.expect("join").expect_err("protocol");
        assert!(
            matches!(error, CallError::Protocol(_)),
            "{reply}: {error:?}"
        );
    }
}

#[tokio::test]
async fn ok_call_with_args_carries_the_args_object() {
    let (bridge, address) = start().await;
    let mut editor = connect_authorized(address).await;
    wait_connected(&bridge, true).await;
    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move {
            bridge
                .call_with_args("scene_hierarchy", json!({"offset": 2, "limit": 10}))
                .await
        }
    });
    let command = next_command(&mut editor).await;
    assert_eq!(
        command,
        json!({"id": 1, "command": "scene_hierarchy", "args": {"offset": 2, "limit": 10}})
    );
    send_text(
        &mut editor,
        json!({"id": 1, "ok": true, "result": {"nodeCount": 0}}).to_string(),
    )
    .await;
    assert_eq!(call.await.expect("join").expect("call ok")["nodeCount"], 0);
}

#[tokio::test]
async fn ok_call_without_args_keeps_the_u0_frame_shape() {
    let (bridge, address) = start().await;
    let mut editor = connect_authorized(address).await;
    wait_connected(&bridge, true).await;
    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.call("editor_state").await }
    });
    let command = next_command(&mut editor).await;
    assert_eq!(command, json!({"id": 1, "command": "editor_state"}));
    assert!(command.get("args").is_none(), "no args key: {command}");
    send_text(
        &mut editor,
        json!({"id": 1, "ok": true, "result": 1}).to_string(),
    )
    .await;
    assert_eq!(call.await.expect("join"), Ok(json!(1)));
}
