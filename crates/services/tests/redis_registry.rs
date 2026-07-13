//! Hermetic shared-registry tests (ADR-0040): spawn a REAL `redis-server` (the
//! flake devShell provides it, mirroring the coturn TURN-relay tests) and prove
//! that TWO stateless signaling nodes over one Redis share the session registry.
//!
//! Like the coturn test, this hard-requires `redis-server` on PATH (no skip
//! guard) — run it inside the flake devShell.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use matchbox_protocol::{JsonPeerEvent, PeerId};
use services::{RedisRegistryStore, RegistryStore, SessionRegistry, build_signaling_server};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A spawned `redis-server`, killed + reaped on drop (RAII, as in `turn_relay.rs`).
struct Redis(Child);

impl Drop for Redis {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn an ephemeral in-memory `redis-server`; return the guard + its url.
fn spawn_redis() -> (Redis, String) {
    // Reserve a free port then drop the probe socket (small race, as in coturn).
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve a port")
        .local_addr()
        .expect("local addr")
        .port();
    let child = Command::new("redis-server")
        .args([
            "--port",
            &port.to_string(),
            "--bind",
            "127.0.0.1",
            "--save",
            "", // no RDB snapshots
            "--appendonly",
            "no", // no AOF — purely in-memory
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("redis-server must be on PATH (the flake devShell provides redis)");
    (Redis(child), format!("redis://127.0.0.1:{port}"))
}

/// Connect a store, retrying until redis answers a real command (readiness — the
/// STUN-probe analogue: a TCP connect alone doesn't prove the server is serving).
async fn connect_store(url: &str) -> RedisRegistryStore {
    for _ in 0..100 {
        if let Ok(store) = RedisRegistryStore::connect(url).await
            && store.session_count().await.is_ok()
        {
            return store;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("redis never became ready at {url}");
}

async fn connect(addr: SocketAddr, room: &str, engine: u32) -> Ws {
    let (ws, _) = connect_async(format!("ws://{addr}/{room}?engine={engine}"))
        .await
        .expect("ws connect");
    ws
}

async fn assigned_id(ws: &mut Ws) -> PeerId {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("event timeout")
            .expect("stream closed")
            .expect("ws error");
        if let Message::Text(txt) = msg
            && let JsonPeerEvent::IdAssigned(id) = txt.as_str().parse().expect("parse event")
        {
            return id;
        }
    }
}

/// Poll a node's SHARED-registry (Redis) session count until it reaches `want`.
async fn wait_global(reg: &SessionRegistry, want: usize) {
    for _ in 0..150 {
        if reg.global_session_count().await.unwrap_or(usize::MAX) == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "global_session_count never reached {want} (got {:?})",
        reg.global_session_count().await
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_share_a_redis_registry() {
    let (_redis, url) = spawn_redis();
    // Two stateless nodes, each with its OWN Redis connection to the SAME server.
    let store_a: Arc<dyn RegistryStore> = Arc::new(connect_store(&url).await);
    let store_b: Arc<dyn RegistryStore> = Arc::new(connect_store(&url).await);
    let reg_a = SessionRegistry::with_store(store_a, "a");
    let reg_b = SessionRegistry::with_store(store_b, "b");

    let mut sa = build_signaling_server((Ipv4Addr::LOCALHOST, 0), reg_a.clone());
    let mut sb = build_signaling_server((Ipv4Addr::LOCALHOST, 0), reg_b.clone());
    let addr_a = sa.bind().expect("bind a");
    let addr_b = sb.bind().expect("bind b");
    tokio::spawn(sa.serve());
    tokio::spawn(sb.serve());

    // A peer on node A (scope X); a peer on node B (scope Y).
    let mut a = connect(addr_a, "m1~7.2~3~arena", 5).await;
    let _a = assigned_id(&mut a).await;
    let mut b = connect(addr_b, "m1~9.2~3~lobby", 5).await;
    let _b = assigned_id(&mut b).await;

    // Either node's shared-registry view aggregates BOTH nodes' sessions over the
    // one Redis ⇒ two nodes share the registry.
    wait_global(&reg_a, 2).await;
    wait_global(&reg_b, 2).await;
    let rooms: Vec<String> = reg_a
        .global_list()
        .await
        .expect("global_list")
        .into_iter()
        .map(|s| s.room)
        .collect();
    assert!(
        rooms.contains(&"m1~7.2~3~arena".to_string()),
        "got {rooms:?}"
    );
    assert!(
        rooms.contains(&"m1~9.2~3~lobby".to_string()),
        "got {rooms:?}"
    );

    // A departure prunes the shared registry: dropping node A's peer removes its
    // session from the listing every node reads.
    drop(a);
    wait_global(&reg_b, 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn redis_store_records_and_prunes() {
    let (_redis, url) = spawn_redis();
    let store = connect_store(&url).await;
    let p1 = PeerId(uuid::Uuid::new_v4());
    let p2 = PeerId(uuid::Uuid::new_v4());

    store.record_join("n", "sess", p1).await.expect("join p1");
    store.record_join("n", "sess", p2).await.expect("join p2");
    assert_eq!(store.peer_count("sess").await.unwrap(), 2);
    assert_eq!(store.session_count().await.unwrap(), 1);
    assert_eq!(store.list().await.unwrap().len(), 1);

    store.record_leave("n", "sess", p1).await.expect("leave p1");
    assert_eq!(store.peer_count("sess").await.unwrap(), 1);
    assert_eq!(store.session_count().await.unwrap(), 1); // still one member

    store.record_leave("n", "sess", p2).await.expect("leave p2");
    assert_eq!(store.peer_count("sess").await.unwrap(), 0);
    assert_eq!(store.session_count().await.unwrap(), 0); // de-indexed once empty
    assert!(store.list().await.unwrap().is_empty());
}
