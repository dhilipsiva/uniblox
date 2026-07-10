//! uniblox signaling server: room-based full-mesh WebRTC signaling (matchbox).
//!
//! Rooms are URL paths: peers connecting to `ws://host:3536/<room>` are
//! full-meshed within that room. Phase 5 extends this with mode/version
//! scoping and `?next=N` matchmaking via a custom `SignalingTopology`
//! (the plain full-mesh topology has no `?next=` handling).

use std::net::{Ipv4Addr, SocketAddr};

use matchbox_signaling::SignalingServer;

/// matchbox's conventional signaling port.
const DEFAULT_PORT: u16 = 3536;

#[tokio::main]
async fn main() {
    // RUST_LOG-controlled tracing (e.g. RUST_LOG=matchbox_signaling=debug).
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let port = std::env::var("UNIBLOX_SIGNALING_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr: SocketAddr = (Ipv4Addr::UNSPECIFIED, port).into();

    let server = SignalingServer::full_mesh_builder(addr)
        .on_peer_connected(|peer| println!("[signaling] peer connected: {peer}"))
        .on_peer_disconnected(|peer| println!("[signaling] peer disconnected: {peer}"))
        .cors()
        .build();

    println!("[signaling] uniblox full-mesh signaling on ws://{addr}/<room>");
    if let Err(err) = server.serve().await {
        eprintln!("[signaling] server error: {err}");
        std::process::exit(1);
    }
}
