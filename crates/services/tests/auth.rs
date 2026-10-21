//! Signaling authentication and rate limits (ADR-0047; HIGH). A signaling server configured with a
//! join-token key refuses a connection without a valid token for the room it asks for; every server
//! rate-limits connection attempts per client IP and signal messages per connection, caps a
//! signal's size, and caps how many peers share one session. Written before the implementation.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use matchbox_protocol::{JsonPeerEvent, JsonPeerRequest, PeerId};
use services::{
    JoinTokenKey, SessionRegistry, SignalingConfig, SignalingLimits, TOKEN_PATH, TokenError,
    build_signaling_server_with, unix_now,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
const ROOM: &str = "m2~1.0~0~arena";

fn key() -> JoinTokenKey {
    JoinTokenKey::new(SECRET).expect("a 32-byte key")
}

async fn boot_with(config: SignalingConfig) -> (SessionRegistry, SocketAddr) {
    let reg = SessionRegistry::new();
    let mut server = build_signaling_server_with((Ipv4Addr::LOCALHOST, 0), reg.clone(), config);
    let addr = server.bind().expect("bind");
    tokio::spawn(server.serve());
    (reg, addr)
}

fn authed() -> SignalingConfig {
    SignalingConfig {
        auth: Some(key()),
        ..SignalingConfig::default()
    }
}

async fn connect(addr: SocketAddr, path_and_query: &str) -> Ws {
    let (ws, _) = connect_async(format!("ws://{addr}/{path_and_query}"))
        .await
        .expect("ws connect");
    ws
}

/// The HTTP status and body of an upgrade the gate refused.
async fn refused(addr: SocketAddr, path_and_query: &str) -> (u16, String) {
    match connect_async(format!("ws://{addr}/{path_and_query}")).await {
        Err(WsError::Http(resp)) => {
            let status = resp.status().as_u16();
            let body =
                String::from_utf8_lossy(resp.body().as_deref().unwrap_or_default()).into_owned();
            (status, body)
        }
        Err(other) => panic!("expected an HTTP refusal, got {other:?}"),
        Ok(_) => panic!("expected a refusal, but the connection was accepted"),
    }
}

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

/// A plain HTTP/1.1 GET; returns the status and body.
async fn http_get(addr: SocketAddr, path_and_query: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.expect("tcp connect");
    let request =
        format!("GET {path_and_query} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("http timeout")
        .expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status line");
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/// Count the signaling events that arrive on `ws` within `dur`.
async fn count_signals(ws: &mut Ws, dur: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + dur;
    let mut n = 0;
    while let Ok(Some(Ok(msg))) = tokio::time::timeout_at(deadline, ws.next()).await {
        if let Message::Text(txt) = msg
            && matches!(txt.as_str().parse(), Ok(JsonPeerEvent::Signal { .. }))
        {
            n += 1;
        }
    }
    n
}

fn signal(to: PeerId, payload: &str) -> Message {
    Message::text(
        JsonPeerRequest::Signal {
            receiver: to,
            data: serde_json::json!({ "Offer": payload }),
        }
        .to_string(),
    )
}

/// Two peers in `room`, each having seen its id and the other's arrival.
async fn pair(addr: SocketAddr, room: &str) -> (Ws, Ws, PeerId) {
    let mut a = connect(addr, room).await;
    let _ = assigned_id(&mut a).await;
    let mut b = connect(addr, room).await;
    let b_id = assigned_id(&mut b).await;
    match next_event(&mut a).await {
        JsonPeerEvent::NewPeer(id) => assert_eq!(id, b_id),
        other => panic!("expected NewPeer, got {other:?}"),
    }
    (a, b, b_id)
}

// ── Join tokens ──────────────────────────────────────────────────────────────

#[test]
fn a_token_verifies_only_for_its_room_and_lifetime() {
    let key = key();
    let now = 1_800_000_000;
    let token = key.mint(ROOM, now + 60);
    assert_eq!(key.verify(&token, ROOM, now), Ok(()));
    assert_eq!(
        key.verify(&token, "m2~1.0~0~other", now),
        Err(TokenError::Invalid)
    );
    assert_eq!(key.verify(&token, ROOM, now + 61), Err(TokenError::Expired));
    // Another key's token, a tampered expiry and a tampered tag are all invalid.
    let other = JoinTokenKey::new(b"another-secret-that-is-32-bytes!").unwrap();
    assert_eq!(
        other.verify(&token, ROOM, now),
        Err(TokenError::Invalid)
    );
    let later = token.replacen(&(now + 60).to_string(), &(now + 120).to_string(), 1);
    assert_eq!(key.verify(&later, ROOM, now), Err(TokenError::Invalid));
    let mut flipped = token.clone().into_bytes();
    let last = flipped.len() - 1;
    flipped[last] = if flipped[last] == b'0' { b'1' } else { b'0' };
    let flipped = String::from_utf8(flipped).unwrap();
    assert_eq!(key.verify(&flipped, ROOM, now), Err(TokenError::Invalid));
    // A lifetime beyond the cap is refused even when the tag is right.
    let forever = key.mint(ROOM, now + 400 * 24 * 3600);
    assert_eq!(key.verify(&forever, ROOM, now), Err(TokenError::TooLong));
    for junk in ["", "v1", "v1.x.y", "v2.1.00", "v1.123.zz", "v1..", "v1.123"] {
        assert_eq!(key.verify(junk, ROOM, now), Err(TokenError::Malformed), "{junk:?}");
    }
}

#[test]
fn a_short_key_is_refused() {
    assert_eq!(
        JoinTokenKey::new(b"too short").err(),
        Some(TokenError::WeakKey)
    );
}

// ── The gate ─────────────────────────────────────────────────────────────────

/// The Done-when test: with a key configured, a connection without a valid token for its room is
/// refused with 401 and a reason; a valid one is admitted.
#[tokio::test]
async fn an_unauthenticated_signaling_connection_is_refused() {
    let (_reg, addr) = boot_with(authed()).await;
    let now = unix_now();
    let good = key().mint(ROOM, now + 60);
    let wrong_room = key().mint("m2~1.0~0~other", now + 60);
    let expired = key().mint(ROOM, now - 1);
    for query in [
        "?engine=1".to_string(),
        "?engine=1&token=garbage".to_string(),
        format!("?engine=1&token={wrong_room}"),
        format!("?engine=1&token={expired}"),
    ] {
        let (status, body) = refused(addr, &format!("{ROOM}{query}")).await;
        assert_eq!(status, 401, "{query}: {body}");
        assert!(body.contains("token"), "{query}: the reason names the token: {body}");
    }
    let mut ws = connect(addr, &format!("{ROOM}?engine=1&token={good}")).await;
    let _ = assigned_id(&mut ws).await;
}

#[tokio::test]
async fn without_a_key_the_gate_stays_open() {
    let (_reg, addr) = boot_with(SignalingConfig::default()).await;
    let mut ws = connect(addr, &format!("{ROOM}?engine=1")).await;
    let _ = assigned_id(&mut ws).await;
}

// ── The token endpoint ───────────────────────────────────────────────────────

#[tokio::test]
async fn the_token_endpoint_issues_working_tokens() {
    let (_reg, addr) = boot_with(SignalingConfig {
        issue_tokens: true,
        ..authed()
    })
    .await;
    let (status, body) = http_get(addr, &format!("{TOKEN_PATH}?room={ROOM}")).await;
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    let token = json["token"].as_str().expect("a token");
    let expires = json["expires"].as_u64().expect("an expiry");
    assert!(expires > unix_now());
    assert_eq!(key().verify(token, ROOM, unix_now()), Ok(()));
    let mut ws = connect(addr, &format!("{ROOM}?engine=1&token={token}")).await;
    let _ = assigned_id(&mut ws).await;

    let (status, _) = http_get(addr, TOKEN_PATH).await;
    assert_eq!(status, 400, "no room");
    let (status, _) = http_get(addr, &format!("{TOKEN_PATH}?room=a%23b")).await;
    assert_eq!(status, 400, "a reserved character");
}

#[tokio::test]
async fn the_token_endpoint_is_off_unless_enabled() {
    let (_reg, addr) = boot_with(authed()).await;
    let (status, _) = http_get(addr, &format!("{TOKEN_PATH}?room={ROOM}")).await;
    assert_eq!(status, 404);
}

// ── Rate limits and caps ─────────────────────────────────────────────────────

#[tokio::test]
async fn connection_attempts_are_rate_limited_per_ip() {
    let (_reg, addr) = boot_with(SignalingConfig {
        limits: SignalingLimits {
            connects_burst: 3,
            connect_refill: Duration::from_secs(3600),
            ..SignalingLimits::default()
        },
        ..SignalingConfig::default()
    })
    .await;
    let mut held = Vec::new();
    for _ in 0..3 {
        let mut ws = connect(addr, &format!("{ROOM}?engine=1")).await;
        let _ = assigned_id(&mut ws).await;
        held.push(ws);
    }
    let (status, _) = refused(addr, &format!("{ROOM}?engine=1")).await;
    assert_eq!(status, 429);
}

#[tokio::test]
async fn token_requests_share_the_per_ip_limit() {
    let (_reg, addr) = boot_with(SignalingConfig {
        issue_tokens: true,
        limits: SignalingLimits {
            connects_burst: 2,
            connect_refill: Duration::from_secs(3600),
            ..SignalingLimits::default()
        },
        ..authed()
    })
    .await;
    for _ in 0..2 {
        assert_eq!(http_get(addr, &format!("{TOKEN_PATH}?room={ROOM}")).await.0, 200);
    }
    assert_eq!(http_get(addr, &format!("{TOKEN_PATH}?room={ROOM}")).await.0, 429);
}

#[tokio::test]
async fn a_signal_flood_is_dropped_and_then_closed() {
    let (_reg, addr) = boot_with(SignalingConfig {
        limits: SignalingLimits {
            signals_burst: 5,
            signals_per_sec: 1,
            abuse_close_after: 10,
            ..SignalingLimits::default()
        },
        ..SignalingConfig::default()
    })
    .await;
    let (mut a, mut b, b_id) = pair(addr, &format!("{ROOM}?engine=1")).await;
    for i in 0..50 {
        if a.send(signal(b_id, &format!("sdp-{i}"))).await.is_err() {
            break; // the server already closed the flooding socket
        }
    }
    let got = count_signals(&mut b, Duration::from_millis(800)).await;
    assert!((1..=7).contains(&got), "the burst passes, the flood does not: {got}");
    // The flooding socket is closed by the server.
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match a.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the flooding socket is closed");
}

#[tokio::test]
async fn an_oversized_signal_is_dropped() {
    let (_reg, addr) = boot_with(SignalingConfig {
        limits: SignalingLimits {
            max_signal_bytes: 1024,
            ..SignalingLimits::default()
        },
        ..SignalingConfig::default()
    })
    .await;
    let (mut a, mut b, b_id) = pair(addr, &format!("{ROOM}?engine=1")).await;
    a.send(signal(b_id, &"x".repeat(2048))).await.unwrap();
    a.send(signal(b_id, "small")).await.unwrap();
    match next_event(&mut b).await {
        JsonPeerEvent::Signal { data, .. } => {
            assert_eq!(data, serde_json::json!({ "Offer": "small" }), "the big one was dropped");
        }
        other => panic!("expected the small signal, got {other:?}"),
    }
}

#[tokio::test]
async fn sessions_are_capped() {
    let (reg, addr) = boot_with(SignalingConfig {
        limits: SignalingLimits {
            max_session_peers: 2,
            ..SignalingLimits::default()
        },
        ..SignalingConfig::default()
    })
    .await;
    let path = format!("{ROOM}?engine=1");
    let (_a, _b, _) = pair(addr, &path).await;
    for _ in 0..100 {
        if reg.peer_count(ROOM) == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (status, body) = refused(addr, &path).await;
    assert_eq!(status, 503, "{body}");
    let (status, body) = refused(addr, &format!("{ROOM}?engine=1&next=3")).await;
    assert_eq!(status, 400, "a ?next above the cap: {body}");
}
