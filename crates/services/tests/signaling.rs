//! Integration tests for the scoped signaling service (ADR-0037/0038/0039): raw
//! WebSocket peers (mirroring matchbox's own tests) prove the SDP/ICE relay, the
//! ASYMMETRIC version filter (compatible engines share a room; a too-old engine
//! is rejected WITH A REASON), content/schema scope isolation, `?next=N`
//! session-SIZE grouping (cap + spill-to-new-session + no-backfill + cross-session
//! relay isolation), legacy-room passthrough, and the session registry.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use matchbox_protocol::{JsonPeerEvent, JsonPeerRequest, PeerId};
use services::{SessionInfo, SessionRegistry, build_signaling_server};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
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

/// Connect a peer. `engine` is the client's own version → `?engine=N`; `None`
/// sends no engine (for legacy plain rooms, which have no engine gate).
async fn connect(addr: SocketAddr, room: &str, engine: Option<u32>) -> Ws {
    connect_next(addr, room, engine, None).await
}

/// Connect a peer with an optional `?engine=N` AND an optional `?next=N`
/// session-SIZE cap.
async fn connect_next(
    addr: SocketAddr,
    room: &str,
    engine: Option<u32>,
    next: Option<usize>,
) -> Ws {
    let mut params = Vec::new();
    if let Some(e) = engine {
        params.push(format!("engine={e}"));
    }
    if let Some(n) = next {
        params.push(format!("next={n}"));
    }
    let query = if params.is_empty() {
        String::new()
    } else {
        format!("?{}", params.join("&"))
    };
    let (ws, _resp) = connect_async(format!("ws://{addr}/{room}{query}"))
        .await
        .expect("ws connect");
    ws
}

/// Assert NO signaling event arrives on `ws` for `dur` (ping/pong frames are
/// tolerated). Used to prove cross-session relay isolation.
async fn expect_silence(ws: &mut Ws, dur: Duration) {
    let deadline = tokio::time::Instant::now() + dur;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => return, // deadline reached with no signaling event → silent
            Ok(Some(Ok(Message::Text(txt)))) => panic!("expected silence, got {txt}"),
            Ok(Some(Ok(_))) => continue, // ping/pong/binary — keep waiting
            Ok(Some(Err(e))) => panic!("ws error during silence wait: {e:?}"),
            Ok(None) => panic!("stream closed during silence wait"),
        }
    }
}

/// Attempt a connection expected to be REJECTED at the WS upgrade; return the
/// HTTP status code + reason body the gate sent.
async fn connect_reject(addr: SocketAddr, path_and_query: &str) -> (u16, String) {
    match connect_async(format!("ws://{addr}/{path_and_query}")).await {
        Err(WsError::Http(resp)) => {
            let status = resp.status().as_u16();
            let body =
                String::from_utf8_lossy(resp.body().as_deref().unwrap_or_default()).into_owned();
            (status, body)
        }
        Err(other) => panic!("expected an HTTP rejection, got {other:?}"),
        Ok(_) => panic!("expected rejection, but the connection was accepted"),
    }
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
    let room = "m1~7.2~3~arena";

    let mut a = connect(addr, room, Some(5)).await;
    let a_id = assigned_id(&mut a).await;
    let mut b = connect(addr, room, Some(5)).await;
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
async fn compatible_engines_share_one_session() {
    // The headline of the asymmetric filter: same content/schema/lobby/min, but
    // DIFFERENT engine versions (both >= min 3) → same room → matched. An
    // "older-but-compatible" engine joins the same session as a newer one.
    let (reg, addr) = boot().await;
    let room = "m1~7.2~3~arena"; // min-engine 3

    let mut a = connect(addr, room, Some(5)).await; // newer engine
    let _a = assigned_id(&mut a).await;
    let mut b = connect(addr, room, Some(3)).await; // exactly the minimum
    let b_id = assigned_id(&mut b).await;

    match next_event(&mut a).await {
        JsonPeerEvent::NewPeer(id) => assert_eq!(id, b_id),
        other => {
            panic!("expected NewPeer(b) — compatible engines must share a room, got {other:?}")
        }
    }
    wait_peer_count(&reg, room, 2).await;
    wait_session_count(&reg, 1).await;
}

#[tokio::test]
async fn engine_below_minimum_is_rejected_with_reason() {
    let (reg, addr) = boot().await;
    // engine 2 < min 3 ⇒ 426 Upgrade Required + a reason naming the minimum.
    let (status, body) = connect_reject(addr, "m1~7.2~3~arena?engine=2").await;
    assert_eq!(status, 426, "engine below minimum must be 426");
    assert!(
        body.contains("minimum"),
        "reason should explain the minimum, got {body:?}"
    );
    // Nothing was admitted.
    wait_session_count(&reg, 0).await;
}

#[tokio::test]
async fn missing_or_non_numeric_engine_is_rejected() {
    let (_reg, addr) = boot().await;
    // Scoped room but no ?engine= ⇒ 400 with a reason.
    let (status, body) = connect_reject(addr, "m1~7.2~3~arena").await;
    assert_eq!(status, 400, "missing engine must be 400");
    assert!(
        body.contains("engine"),
        "reason should mention engine, got {body:?}"
    );
    // Non-numeric ?engine= ⇒ 400 as well.
    let (status, _) = connect_reject(addr, "m1~7.2~3~arena?engine=nope").await;
    assert_eq!(status, 400, "non-numeric engine must be 400");
}

#[tokio::test]
async fn distinct_modes_are_isolated() {
    let (reg, addr) = boot().await;

    // Same content/schema/min/lobby + valid engine, DIFFERENT mode ⇒ different
    // room ⇒ never matched.
    let mut a = connect(addr, "m1~7.2~3~arena", Some(5)).await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect(addr, "m2~7.2~3~arena", Some(5)).await;
    let _b = assigned_id(&mut b).await;

    wait_session_count(&reg, 2).await;
    assert_eq!(reg.peer_count("m1~7.2~3~arena"), 1);
    assert_eq!(reg.peer_count("m2~7.2~3~arena"), 1);
}

#[tokio::test]
async fn different_content_is_isolated() {
    let (reg, addr) = boot().await;
    // content differs (7 vs 9) — content must match EXACTLY ⇒ structural
    // isolation even though both engines clear the minimum.
    let mut a = connect(addr, "m1~7.2~3~arena", Some(5)).await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect(addr, "m1~9.2~3~arena", Some(5)).await;
    let _b = assigned_id(&mut b).await;
    wait_session_count(&reg, 2).await;
}

#[tokio::test]
async fn malformed_scope_is_rejected_with_reason() {
    let (_reg, addr) = boot().await;
    // `~`-shaped but not a valid scope ⇒ 400 with a reason (was a bare 401 pre-0038).
    let (status, body) = connect_reject(addr, "m1~garbage?engine=5").await;
    assert_eq!(status, 400, "malformed scope must be 400");
    assert!(!body.is_empty(), "malformed scope should carry a reason");
}

#[tokio::test]
async fn legacy_plain_room_still_matches() {
    let (reg, addr) = boot().await;

    // No `~`, no `?engine=` — a legacy room with no version gate.
    let mut a = connect(addr, "uniblox-demo", None).await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect(addr, "uniblox-demo", None).await;
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

    let mut a = connect(addr, "m1~7.2~3~arena", Some(5)).await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect(addr, "m3~7.2~3~arena", Some(5)).await;
    let _b = assigned_id(&mut b).await;

    wait_session_count(&reg, 2).await;
    let list = reg.list();
    assert_eq!(
        list,
        vec![
            SessionInfo {
                room: "m1~7.2~3~arena".to_string(),
                peers: 1,
            },
            SessionInfo {
                room: "m3~7.2~3~arena".to_string(),
                peers: 1,
            },
        ]
    );
}

#[tokio::test]
async fn disconnect_prunes_the_session() {
    let (reg, addr) = boot().await;
    let room = "m1~7.2~3~arena";

    let mut a = connect(addr, room, Some(5)).await;
    let _a = assigned_id(&mut a).await;
    wait_peer_count(&reg, room, 1).await;

    drop(a); // close the socket
    wait_session_count(&reg, 0).await; // pruned once empty
}

// ---- ?next=N session-SIZE grouping (ADR-0039) ----

#[tokio::test]
async fn next_caps_session_size_and_spills() {
    // The headline: ?next=4 deals peers into sessions of at most 4. Four peers on
    // one scope fill session #0; the 5th spills into a fresh session #1.
    let (reg, addr) = boot().await;
    let scope = "m1~7.2~3~arena";

    // Keep the sockets alive (dropping a Ws disconnects the peer).
    let mut first_four = Vec::new();
    for _ in 0..4 {
        let mut ws = connect_next(addr, scope, Some(5), Some(4)).await;
        let _ = assigned_id(&mut ws).await;
        first_four.push(ws);
    }
    // All four landed in the SAME (first) session, which then sealed at 4.
    wait_peer_count(&reg, "m1~7.2~3~arena#0", 4).await;
    wait_session_count(&reg, 1).await;

    // The 5th cannot join the sealed session → a new one opens.
    let mut fifth = connect_next(addr, scope, Some(5), Some(4)).await;
    let _ = assigned_id(&mut fifth).await;
    wait_session_count(&reg, 2).await;
    wait_peer_count(&reg, "m1~7.2~3~arena#1", 1).await;
    assert_eq!(
        reg.peer_count("m1~7.2~3~arena#0"),
        4,
        "the sealed session stays at its cap"
    );
}

#[tokio::test]
async fn sessions_are_relay_isolated() {
    // ?next=1 seals every peer into its own session. A Signal aimed across
    // sessions is dropped (the target isn't in the sender's session).
    let (reg, addr) = boot().await;
    let scope = "m1~7.2~3~arena";

    let mut a = connect_next(addr, scope, Some(5), Some(1)).await;
    let _a_id = assigned_id(&mut a).await;
    let mut b = connect_next(addr, scope, Some(5), Some(1)).await;
    let b_id = assigned_id(&mut b).await;

    wait_session_count(&reg, 2).await; // two singleton sessions

    // A → B, but B is in a different session ⇒ the relay must not deliver it.
    let offer = JsonPeerRequest::Signal {
        receiver: b_id,
        data: serde_json::json!({ "Offer": "cross-session" }),
    };
    a.send(Message::text(offer.to_string()))
        .await
        .expect("send offer");
    expect_silence(&mut b, Duration::from_millis(400)).await;
}

#[tokio::test]
async fn no_backfill_after_departure() {
    // A sealed session never refills: after a peer leaves a full ?next=2 session,
    // a new joiner opens a fresh session rather than backfilling the free slot.
    let (reg, addr) = boot().await;
    let scope = "m1~7.2~3~arena";

    let mut a = connect_next(addr, scope, Some(5), Some(2)).await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect_next(addr, scope, Some(5), Some(2)).await;
    let _b = assigned_id(&mut b).await;
    wait_peer_count(&reg, "m1~7.2~3~arena#0", 2).await; // sealed at 2
    wait_session_count(&reg, 1).await;

    drop(a); // #0 drops to 1, but stays sealed (no backfill)
    wait_peer_count(&reg, "m1~7.2~3~arena#0", 1).await;

    // A new joiner opens session #1 instead of filling #0's free slot.
    let mut c = connect_next(addr, scope, Some(5), Some(2)).await;
    let _c = assigned_id(&mut c).await;
    wait_session_count(&reg, 2).await;
    wait_peer_count(&reg, "m1~7.2~3~arena#1", 1).await;
    assert_eq!(
        reg.peer_count("m1~7.2~3~arena#0"),
        1,
        "the sealed session is not backfilled"
    );
}

#[tokio::test]
async fn invalid_next_is_rejected() {
    let (_reg, addr) = boot().await;
    // ?next=0 (not positive) and ?next=abc (non-numeric) ⇒ 400 with a reason.
    let (status, body) = connect_reject(addr, "m1~7.2~3~arena?engine=5&next=0").await;
    assert_eq!(status, 400, "next=0 must be 400");
    assert!(
        body.contains("next"),
        "reason should mention next, got {body:?}"
    );
    let (status, _) = connect_reject(addr, "m1~7.2~3~arena?engine=5&next=abc").await;
    assert_eq!(status, 400, "non-numeric next must be 400");
}

#[tokio::test]
async fn no_next_is_one_unbounded_session() {
    // No ?next ⇒ one unbounded session per room (the FullMesh-equivalent path).
    let (reg, addr) = boot().await;
    let room = "m1~7.2~3~arena";

    let mut peers = Vec::new();
    for _ in 0..3 {
        let mut ws = connect(addr, room, Some(5)).await;
        let _ = assigned_id(&mut ws).await;
        peers.push(ws);
    }
    wait_peer_count(&reg, room, 3).await; // session key == the room path
    wait_session_count(&reg, 1).await;
}

#[tokio::test]
async fn room_with_session_delimiter_is_rejected() {
    let (_reg, addr) = boot().await;
    // `%230` percent-decodes (axum) to `arena#0`, which would collide with a
    // `?next` sub-session key `<room>#<index>` and cross the isolation boundary —
    // the gate reserves `#` and rejects it (scoped path here; also legacy).
    let (status, body) = connect_reject(addr, "m1~7.2~3~arena%230?engine=5").await;
    assert_eq!(status, 400, "a '#' in the room must be 400");
    assert!(
        body.contains('#'),
        "reason should mention '#', got {body:?}"
    );
    // Also rejected on a legacy plain room.
    let (status, _) = connect_reject(addr, "demo%230").await;
    assert_eq!(status, 400, "a '#' in a legacy room must be 400");
}

#[tokio::test]
async fn next_groups_legacy_rooms_too() {
    // ?next is orthogonal to the scope — it also caps a legacy plain room.
    let (reg, addr) = boot().await;

    let mut a = connect_next(addr, "uniblox-demo", None, Some(1)).await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect_next(addr, "uniblox-demo", None, Some(1)).await;
    let _b = assigned_id(&mut b).await;

    wait_session_count(&reg, 2).await; // each sealed into its own singleton
}
