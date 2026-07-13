//! Integration tests for the scoped signaling service (ADR-0037): raw WebSocket
//! peers (mirroring matchbox's own tests) prove the SDP/ICE relay, `{mode,
//! version}` scope isolation, malformed-scope rejection, legacy-room passthrough,
//! and the session registry.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use matchbox_protocol::{JsonPeerEvent, JsonPeerRequest, PeerId};
use services::{SessionInfo, SessionRegistry, build_signaling_server};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Boot a scoped signaling server on an ephemeral port; return the registry
/// handle (for assertions) + the bound address.
async fn boot() -> (SessionRegistry, SocketAddr) {
    let reg = SessionRegistry::new();
    let mut server = build_signaling_server((Ipv4Addr::LOCALHOST, 0), reg.clone());
    let addr = server.bind().expect("bind");
    tokio::spawn(server.serve());
    (reg, addr)
}

async fn connect(addr: SocketAddr, room: &str) -> Ws {
    let (ws, _resp) = connect_async(format!("ws://{addr}/{room}"))
        .await
        .expect("ws connect");
    ws
}

/// Next signaling event (ignoring ping/pong), with a timeout so a hang fails fast.
async fn next_event(ws: &mut Ws) -> JsonPeerEvent {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("event timeout")
            .expect("stream closed")
            .expect("ws error");
        if let Message::Text(txt) = msg {
            return txt.as_str().parse::<JsonPeerEvent>().expect("parse event");
        }
    }
}

async fn assigned_id(ws: &mut Ws) -> PeerId {
    match next_event(ws).await {
        JsonPeerEvent::IdAssigned(id) => id,
        other => panic!("expected IdAssigned, got {other:?}"),
    }
}

/// Poll the registry until `room` holds `want` peers (robust to async timing).
async fn wait_peer_count(reg: &SessionRegistry, room: &str, want: usize) {
    for _ in 0..100 {
        if reg.peer_count(room) == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "peer_count({room}) never reached {want} (got {})",
        reg.peer_count(room)
    );
}

async fn wait_session_count(reg: &SessionRegistry, want: usize) {
    for _ in 0..100 {
        if reg.session_count() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "session_count never reached {want} (got {})",
        reg.session_count()
    );
}

#[tokio::test]
async fn same_scope_peers_match_and_relay_offers() {
    let (reg, addr) = boot().await;
    let room = "m1~1.2.3~arena";

    let mut a = connect(addr, room).await;
    let a_id = assigned_id(&mut a).await;
    let mut b = connect(addr, room).await;
    let b_id = assigned_id(&mut b).await;

    // A sees B join (same room).
    match next_event(&mut a).await {
        JsonPeerEvent::NewPeer(id) => assert_eq!(id, b_id),
        other => panic!("expected NewPeer(b), got {other:?}"),
    }

    // A → B offer relay (the SDP shuttling this whole service exists for).
    let offer = JsonPeerRequest::Signal {
        receiver: b_id,
        data: serde_json::json!({ "Offer": "sdp-blob" }),
    };
    a.send(Message::text(offer.to_string()))
        .await
        .expect("send offer");
    match next_event(&mut b).await {
        JsonPeerEvent::Signal { sender, data } => {
            assert_eq!(sender, a_id);
            assert_eq!(data, serde_json::json!({ "Offer": "sdp-blob" }));
        }
        other => panic!("expected relayed Signal from a, got {other:?}"),
    }

    wait_peer_count(&reg, room, 2).await;
    wait_session_count(&reg, 1).await;
}

#[tokio::test]
async fn distinct_scopes_are_isolated() {
    let (reg, addr) = boot().await;

    // Same version + lobby, DIFFERENT mode ⇒ different room ⇒ never matched.
    let mut a = connect(addr, "m1~1.2.3~arena").await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect(addr, "m2~1.2.3~arena").await;
    let _b = assigned_id(&mut b).await;

    // Two separate single-peer sessions — FullMesh cannot relay across rooms.
    wait_session_count(&reg, 2).await;
    assert_eq!(reg.peer_count("m1~1.2.3~arena"), 1);
    assert_eq!(reg.peer_count("m2~1.2.3~arena"), 1);
}

#[tokio::test]
async fn different_version_is_isolated() {
    let (reg, addr) = boot().await;
    let mut a = connect(addr, "m1~1.2.3~arena").await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect(addr, "m1~9.2.3~arena").await; // engine differs
    let _b = assigned_id(&mut b).await;
    wait_session_count(&reg, 2).await;
}

#[tokio::test]
async fn malformed_scope_is_rejected() {
    let (_reg, addr) = boot().await;
    // `~`-shaped but not a valid scope ⇒ the gate returns 401 ⇒ handshake fails.
    let result = connect_async(format!("ws://{addr}/m1~garbage")).await;
    assert!(result.is_err(), "malformed scope should be rejected");
}

#[tokio::test]
async fn legacy_plain_room_still_matches() {
    let (reg, addr) = boot().await;

    let mut a = connect(addr, "uniblox-demo").await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect(addr, "uniblox-demo").await;
    let b_id = assigned_id(&mut b).await;

    match next_event(&mut a).await {
        JsonPeerEvent::NewPeer(id) => assert_eq!(id, b_id),
        other => panic!("expected NewPeer, got {other:?}"),
    }
    wait_peer_count(&reg, "uniblox-demo", 2).await;
}

#[tokio::test]
async fn registry_lists_sessions() {
    let (reg, addr) = boot().await;

    let mut a = connect(addr, "m1~1.2.3~arena").await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect(addr, "m3~1.2.3~arena").await;
    let _b = assigned_id(&mut b).await;

    wait_session_count(&reg, 2).await;
    let list = reg.list();
    assert_eq!(
        list,
        vec![
            SessionInfo {
                room: "m1~1.2.3~arena".to_string(),
                peers: 1,
            },
            SessionInfo {
                room: "m3~1.2.3~arena".to_string(),
                peers: 1,
            },
        ]
    );
}

#[tokio::test]
async fn disconnect_prunes_the_session() {
    let (reg, addr) = boot().await;
    let room = "m1~1.2.3~arena";

    let mut a = connect(addr, room).await;
    let _a = assigned_id(&mut a).await;
    wait_peer_count(&reg, room, 1).await;

    drop(a); // close the socket
    wait_session_count(&reg, 0).await; // pruned once empty
}
