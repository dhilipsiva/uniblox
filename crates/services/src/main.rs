//! uniblox signaling server binary (ADR-0037/0038): scoped room-based WebRTC
//! signaling. Rooms are URL paths; a scoped room is
//! `<mode>~<content>.<schema>~<min>~<lobby>` (content/schema/min/lobby isolate
//! structurally) and the client's own engine rides the `?engine=N` query, gated
//! `>= min` (the asymmetric filter). A plain path is a legacy room. The
//! matchmaking logic + session registry live in the `services` library.

use std::net::{Ipv4Addr, SocketAddr};

use services::{SessionRegistry, build_signaling_server};

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

    let registry = SessionRegistry::new();
    let server = build_signaling_server(addr, registry);

    println!(
        "[signaling] uniblox scoped signaling on ws://{addr}/<mode>~<engine>.<content>.<schema>~<lobby>"
    );
    if let Err(err) = server.serve().await {
        eprintln!("[signaling] server error: {err}");
        std::process::exit(1);
    }
}
