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
    // A token that outlives the TURN TTL, so the TTL is what bounds the credential.
    let token = JoinTokenKey::new(SECRET)
        .unwrap()
        .mint(ROOM, unix_now() + 3600);
    let (status, headers, body) =
        get(addr, &format!("{TURN_PATH}?room={ROOM}&token={token}")).await;
    assert_eq!(status, 200, "{body}");
    assert!(headers.contains("cache-control: no-store"), "{headers}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    let username = json["username"].as_str().expect("username");
    let credential = json["credential"].as_str().expect("credential");
    assert_eq!(json["urls"][0], "turn:turn.example:3478?transport=udp");
    // The expiry is rounded down to a grid of half the TTL (see the next test).
    let ttl = json["ttl"].as_u64().expect("ttl");
    assert!(
        (301..=600).contains(&ttl),
        "between half the TTL and the TTL: {ttl}"
    );
    let (expiry, label) = username.split_once(':').expect("<expiry>:<label>");
    let expiry: u64 = expiry.parse().expect("a unix expiry");
    let now = unix_now();
    assert!(
        (now + 299..=now + 600).contains(&expiry),
        "expires within the TTL: {expiry}"
    );
    assert!(!label.is_empty());
    assert_eq!(
        credential,
        turn_credential(TURN_SECRET, label, expiry).credential
    );
}

/// A token that outlives the TTL keeps its username too: the expiry is rounded down to a grid of half
/// the TTL, so two fetches a second apart get the same username (or, across a grid line, usernames one
/// step apart), and a token has at most two valid usernames at a time.
#[tokio::test]
async fn a_long_lived_token_keeps_its_username() {
    let addr = boot(configured()).await;
    let token = JoinTokenKey::new(SECRET)
        .unwrap()
        .mint(ROOM, unix_now() + 3600);
    let fetch = || {
        let token = token.clone();
        async move {
            let (status, _, body) =
                get(addr, &format!("{TURN_PATH}?room={ROOM}&token={token}")).await;
            assert_eq!(status, 200, "{body}");
            let json: serde_json::Value = serde_json::from_str(&body).unwrap();
            json["username"].as_str().unwrap().to_string()
        }
    };
    let expiry = |u: &str| u.split_once(':').unwrap().0.parse::<u64>().unwrap();
    let first = fetch().await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let second = fetch().await;
    assert_eq!(expiry(&first) % 300, 0, "on the grid: {first}");
    assert!(
        second == first || expiry(&second) == expiry(&first) + 300,
        "{first} then {second}"
    );
}

/// A credential never outlives the join token it was issued for, and one token always gets the same
/// username, so coturn's per-user quota binds per token; another token gets another username.
#[tokio::test]
async fn a_credential_is_bound_to_its_token() {
    let addr = boot(configured()).await;
    let key = JoinTokenKey::new(SECRET).unwrap();
    let token_expiry = unix_now() + 60;
    let token = key.mint(ROOM, token_expiry);
    let fetch = |token: String| async move {
        let (status, _, body) = get(addr, &format!("{TURN_PATH}?room={ROOM}&token={token}")).await;
        assert_eq!(status, 200, "{body}");
        serde_json::from_str::<serde_json::Value>(&body).unwrap()
    };
    let first = fetch(token.clone()).await;
    let username = first["username"].as_str().unwrap().to_string();
    let (expiry, label) = username.split_once(':').unwrap();
    assert!(
        expiry.parse::<u64>().unwrap() <= token_expiry,
        "capped at the token's expiry"
    );
    assert!(first["ttl"].as_u64().unwrap() <= 60);
    let again = fetch(token.clone()).await;
    assert_eq!(
        again["username"], first["username"],
        "the same token, the same username"
    );
    let other = fetch(key.mint(ROOM, unix_now() + 61)).await;
    let other_label = other["username"]
        .as_str()
        .unwrap()
        .split_once(':')
        .unwrap()
        .1
        .to_string();
    assert_ne!(other_label, label, "another token, another username");
}

#[tokio::test]
async fn turn_refusals_carry_cors() {
    let addr = boot(configured()).await;
    let (status, headers, _) = get(addr, &format!("{TURN_PATH}?room={ROOM}")).await;
    assert_eq!(status, 401);
    assert!(
        headers.contains("access-control-allow-origin: *"),
        "{headers}"
    );
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
