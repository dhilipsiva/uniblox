//! uniblox signaling server binary (ADR-0037/0038/0039/0040/0047): scoped room-based
//! WebRTC signaling. Rooms are URL paths; a scoped room is
//! `<mode>~<content>.<schema>~<min>~<lobby>` (content/schema/min/lobby isolate
//! structurally) and the client's own engine rides the `?engine=N` query, gated
//! `>= min` (the asymmetric filter). An optional `?next=N` caps session SIZE
//! (peers deal into sessions of at most N). A plain path is a legacy room. The
//! custom topology + matchmaking + session registry live in the `services`
//! library.
//!
//! Authentication (ADR-0047), secure by default: set `UNIBLOX_SIGNALING_SECRET` (at least 32
//! bytes) and every peer must carry a join token (`?token=`) for its room. Without a secret the
//! server refuses to start unless `UNIBLOX_SIGNALING_INSECURE=1` (local development only).
//! `UNIBLOX_ISSUE_TOKENS=1` serves `GET /_uniblox/token?room=<room>` (for a hosted demo);
//! `UNIBLOX_TRUST_FORWARDED_FOR=1` takes the client IP for the rate limits from the proxy's
//! `X-Forwarded-For`. `services mint-token <room> [lifetime-secs]` prints a token (for an operator,
//! e.g. a Mode-3 server's `UNIBLOX_SIGNALING_URL`).
//!
//! Horizontal scale (ADR-0040): set `UNIBLOX_REDIS_URL` to back the session
//! registry with a shared Redis so multiple stateless nodes share one listing
//! (peers of a session must land on the same node — sticky routing). Unset ⇒ a
//! single-node in-memory registry. `UNIBLOX_NODE_ID` names this node (default: a
//! random uuid).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use services::{
    ClientIp, JoinTokenKey, RedisRegistryStore, SessionRegistry, SignalingConfig,
    build_signaling_server_with, unix_now,
};

/// matchbox's conventional signaling port.
const DEFAULT_PORT: u16 = 3536;

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// The join-token key from `UNIBLOX_SIGNALING_SECRET`, or exit with the reason.
fn key_from_env() -> Option<JoinTokenKey> {
    let secret = std::env::var("UNIBLOX_SIGNALING_SECRET").ok()?;
    match JoinTokenKey::new(secret.as_bytes()) {
        Ok(key) => Some(key),
        Err(err) => {
            eprintln!("[signaling] UNIBLOX_SIGNALING_SECRET: {err}");
            std::process::exit(2);
        }
    }
}

/// `services mint-token <room> [lifetime-secs]`: print a join token for `room`.
fn mint_token(args: &[String]) -> ! {
    let Some(room) = args.first() else {
        eprintln!("usage: services mint-token <room> [lifetime-secs]");
        std::process::exit(2);
    };
    let lifetime = match args.get(1).map(|s| s.parse::<u64>()) {
        None => 3600,
        Some(Ok(secs)) => secs,
        Some(Err(_)) => {
            eprintln!("lifetime-secs must be a whole number of seconds");
            std::process::exit(2);
        }
    };
    let Some(key) = key_from_env() else {
        eprintln!("mint-token needs UNIBLOX_SIGNALING_SECRET");
        std::process::exit(2);
    };
    println!("{}", key.mint(room, unix_now() + lifetime));
    std::process::exit(0);
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("mint-token") {
        mint_token(&args[1..]);
    }

    // RUST_LOG-controlled tracing (e.g. RUST_LOG=matchbox_signaling=debug).
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let auth = key_from_env();
    if auth.is_none() {
        if flag("UNIBLOX_SIGNALING_INSECURE") {
            eprintln!(
                "[signaling] WARNING: no UNIBLOX_SIGNALING_SECRET; the gate is OPEN (development only)"
            );
        } else {
            eprintln!(
                "[signaling] refusing to start without UNIBLOX_SIGNALING_SECRET (at least 32 bytes); \
                 set UNIBLOX_SIGNALING_INSECURE=1 to run an open server for local development"
            );
            std::process::exit(2);
        }
    }
    let config = SignalingConfig {
        auth,
        issue_tokens: flag("UNIBLOX_ISSUE_TOKENS"),
        token_lifetime: Duration::from_secs(600),
        client_ip: if flag("UNIBLOX_TRUST_FORWARDED_FOR") {
            ClientIp::ForwardedFor
        } else {
            ClientIp::Peer
        },
        ..SignalingConfig::default()
    };

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
    let server = build_signaling_server_with(addr, registry, config);

    println!(
        "[signaling] uniblox scoped signaling on ws://{addr}/<mode>~<content>.<schema>~<min>~<lobby>?engine=N[&next=N]"
    );
    if let Err(err) = server.serve().await {
        eprintln!("[signaling] server error: {err}");
        std::process::exit(1);
    }
}
