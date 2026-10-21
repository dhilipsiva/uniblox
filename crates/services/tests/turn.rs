//! Time-limited TURN credentials (ADR-0050): coturn's REST-API scheme (`use-auth-secret`), minted
//! only for a client holding a valid join token for its room. Written before the implementation.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use services::{
    JoinTokenKey, SessionRegistry, SignalingConfig, TURN_PATH, TurnConfig,
    build_signaling_server_with, turn_credential, unix_now,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
const TURN_SECRET: &[u8] = b"turn-shared-secret";
const ROOM: &str = "m2~1.0~0~arena";

/// Known answers, computed independently (HMAC-SHA1, then standard base64 with padding).
#[test]
fn credentials_follow_coturns_rest_scheme() {
    let c = turn_credential(b"north", "alice", 1_700_000_000);
    assert_eq!(c.username, "1700000000:alice");
    assert_eq!(c.credential, "Cd/49soE35ICqcJF/bCTn8Z4OyE=");
    let c = turn_credential(SECRET, "peer", 1_800_000_000);
    assert_eq!(c.credential, "Wc/xZkkDRmRIMv/rOsRQRffJ/9g=");
}

fn turn() -> TurnConfig {
    TurnConfig {
        secret: TURN_SECRET.to_vec(),
        urls: vec!["turn:turn.example:3478?transport=udp".to_string()],
        ttl: Duration::from_secs(600),
    }
}

async fn boot(config: SignalingConfig) -> SocketAddr {
    let mut server =
        build_signaling_server_with((Ipv4Addr::LOCALHOST, 0), SessionRegistry::new(), config);
    let addr = server.bind().expect("bind");
    tokio::spawn(server.serve());
    addr
}

/// A plain HTTP/1.1 GET: (status, lowercased headers, body).
async fn get(addr: SocketAddr, path_and_query: &str) -> (u16, String, String) {
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
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .map(|(h, b)| (h.to_lowercase(), b.to_string()))
        .unwrap_or_default();
    (status, headers, body)
}

fn configured() -> SignalingConfig {
    SignalingConfig {
        auth: Some(JoinTokenKey::new(SECRET).unwrap()),
        turn: Some(turn()),
        ..SignalingConfig::default()
    }
}

/// Only a client with a valid join token for its room gets a credential.
#[tokio::test]
async fn an_unauthenticated_request_gets_no_credential() {
    let addr = boot(configured()).await;
    let key = JoinTokenKey::new(SECRET).unwrap();
    let other_room = key.mint("m2~1.0~0~other", unix_now() + 60);
    for query in [
        format!("?room={ROOM}"),
        format!("?room={ROOM}&token=garbage"),
        format!("?room={ROOM}&token={other_room}"),
    ] {
        let (status, _, body) = get(addr, &format!("{TURN_PATH}{query}")).await;
        assert_eq!(status, 401, "{query}: {body}");
    }
}

#[tokio::test]
async fn an_authenticated_client_gets_a_time_limited_credential() {
    let addr = boot(configured()).await;
    let token = JoinTokenKey::new(SECRET)
        .unwrap()
        .mint(ROOM, unix_now() + 60);
    let (status, headers, body) =
        get(addr, &format!("{TURN_PATH}?room={ROOM}&token={token}")).await;
    assert_eq!(status, 200, "{body}");
    assert!(headers.contains("cache-control: no-store"), "{headers}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    let username = json["username"].as_str().expect("username");
    let credential = json["credential"].as_str().expect("credential");
    assert_eq!(json["urls"][0], "turn:turn.example:3478?transport=udp");
    assert_eq!(json["ttl"], 600);
    let (expiry, label) = username.split_once(':').expect("<expiry>:<label>");
    let expiry: u64 = expiry.parse().expect("a unix expiry");
    let now = unix_now();
    assert!(
        (now + 590..=now + 610).contains(&expiry),
        "expires in the TTL: {expiry}"
    );
    assert!(!label.is_empty());
    assert_eq!(
        credential,
        turn_credential(TURN_SECRET, label, expiry).credential
    );
    // Two requests get two distinct usernames.
    let (_, _, again) = get(addr, &format!("{TURN_PATH}?room={ROOM}&token={token}")).await;
    let again: serde_json::Value = serde_json::from_str(&again).unwrap();
    assert_ne!(again["username"], json["username"]);
}

#[tokio::test]
async fn the_turn_endpoint_is_off_without_turn_or_auth() {
    let token = JoinTokenKey::new(SECRET)
        .unwrap()
        .mint(ROOM, unix_now() + 60);
    for config in [
        SignalingConfig {
            auth: Some(JoinTokenKey::new(SECRET).unwrap()),
            ..SignalingConfig::default()
        },
        SignalingConfig {
            turn: Some(turn()),
            ..SignalingConfig::default()
        },
    ] {
        let addr = boot(config).await;
        let (status, _, _) = get(addr, &format!("{TURN_PATH}?room={ROOM}&token={token}")).await;
        assert_eq!(status, 404);
    }
}
