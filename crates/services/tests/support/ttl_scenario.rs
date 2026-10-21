//! The registry-liveness crash scenario (ADR-0048), shared by the in-memory and Redis store tests.

use std::time::Duration;

use matchbox_protocol::PeerId;
use services::RegistryStore;

pub const TTL: Duration = Duration::from_millis(600);

/// The crash scenario, for any store whose node TTL is [`TTL`].
pub async fn a_crashed_nodes_members_expire_and_are_swept(store: &dyn RegistryStore) {
    let p1 = PeerId(uuid::Uuid::new_v4());
    let p2 = PeerId(uuid::Uuid::new_v4());
    store.record_join("live", "room", p1).await.unwrap();
    store.record_join("dead", "room", p2).await.unwrap();
    store.record_join("dead", "other", p2).await.unwrap();
    assert_eq!(store.peer_count("room").await.unwrap(), 2);
    assert_eq!(store.session_count().await.unwrap(), 3);

    // "live" keeps heartbeating; "dead" never does again.
    for _ in 0..8 {
        store.heartbeat("live").await.unwrap();
        tokio::time::sleep(TTL / 4).await;
    }
    assert_eq!(
        store.peer_count("room").await.unwrap(),
        1,
        "the dead node's peer is not counted"
    );
    assert_eq!(store.peer_count("other").await.unwrap(), 0);
    assert_eq!(store.session_count().await.unwrap(), 1);
    let listed: Vec<String> = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.room)
        .collect();
    assert_eq!(listed, vec!["room".to_string()]);

    assert_eq!(
        store.sweep().await.unwrap(),
        2,
        "both of the dead node's sessions are removed"
    );
    assert_eq!(store.sweep().await.unwrap(), 0, "nothing left to sweep");
    assert_eq!(store.peer_count("room").await.unwrap(), 1);

    // The live node's leave still prunes its session.
    store.record_leave("live", "room", p1).await.unwrap();
    assert_eq!(store.session_count().await.unwrap(), 0);
}
