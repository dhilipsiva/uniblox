//! uniblox signaling server binary (ADR-0037/0038/0039/0040): scoped room-based
//! WebRTC signaling. Rooms are URL paths; a scoped room is
//! `<mode>~<content>.<schema>~<min>~<lobby>` (content/schema/min/lobby isolate
//! structurally) and the client's own engine rides the `?engine=N` query, gated
//! `>= min` (the asymmetric filter). An optional `?next=N` caps session SIZE
//! (peers deal into sessions of at most N). A plain path is a legacy room. The
//! custom topology + matchmaking + session registry live in the `services`
//! library.
//!
//! Horizontal scale (ADR-0040): set `UNIBLOX_REDIS_URL` to back the session
//! registry with a shared Redis so multiple stateless nodes share one listing
//! (peers of a session must land on the same node — sticky routing). Unset ⇒ a
//! single-node in-memory registry. `UNIBLOX_NODE_ID` names this node (default: a
//! random uuid).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use services::{RedisRegistryStore, SessionRegistry, build_signaling_server};

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

    // Opt-in shared Redis registry (else an in-memory single-node registry).
    let registry = match std::env::var("UNIBLOX_REDIS_URL").ok() {
        Some(url) => match RedisRegistryStore::connect(&url).await {
            Ok(store) => {
                let node_id = std::env::var("UNIBLOX_NODE_ID")
                    .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
                println!("[signaling] shared Redis registry at {url} (node {node_id})");
                SessionRegistry::with_store(Arc::new(store), node_id)
            }
            Err(err) => {
                eprintln!("[signaling] cannot connect to UNIBLOX_REDIS_URL ({url}): {err}");
                std::process::exit(1);
            }
        },
        None => SessionRegistry::new(),
    };
    let server = build_signaling_server(addr, registry);

    println!(
        "[signaling] uniblox scoped signaling on ws://{addr}/<mode>~<content>.<schema>~<min>~<lobby>?engine=N[&next=N]"
    );
    if let Err(err) = server.serve().await {
        eprintln!("[signaling] server error: {err}");
        std::process::exit(1);
    }
}
