//! Registry liveness (ADR-0048): a node that stops heartbeating (it crashed) drops out of the shared
//! listing once its liveness TTL lapses, and a sweep removes what it left. The in-memory store runs
//! here; the same scenario runs against a real Redis in `redis_registry.rs`.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

#[path = "support/ttl_scenario.rs"]
mod ttl_scenario;

use futures::StreamExt;
use matchbox_protocol::JsonPeerEvent;
use services::{MemoryRegistryStore, RegistryStore, SessionRegistry, build_signaling_server};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use ttl_scenario::{TTL, a_crashed_nodes_members_expire_and_are_swept};

#[tokio::test]
async fn memory_store_crash_cleanup() {
    let store = MemoryRegistryStore::new().with_node_ttl(TTL);
    a_crashed_nodes_members_expire_and_are_swept(&store).await;
}

/// A running server heartbeats its own node, so its sessions stay listed past the TTL.
#[tokio::test]
async fn a_running_server_keeps_its_node_alive() {
    let store: Arc<dyn RegistryStore> = Arc::new(MemoryRegistryStore::new().with_node_ttl(TTL));
    let reg = SessionRegistry::with_store(Arc::clone(&store), "node-a");
    let mut server = build_signaling_server((Ipv4Addr::LOCALHOST, 0), reg.clone());
    let addr = server.bind().expect("bind");
    tokio::spawn(server.serve());
    let (mut ws, _) = connect_async(format!("ws://{addr}/m1~1.0~0~arena?engine=1"))
        .await
        .expect("connect");
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(t) = msg
            && matches!(t.as_str().parse(), Ok(JsonPeerEvent::IdAssigned(_)))
        {
            break;
        }
    }
    tokio::time::sleep(TTL * 3).await;
    assert_eq!(reg.global_peer_count("m1~1.0~0~arena").await.unwrap(), 1);
}
