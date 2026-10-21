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
//! The secret is taken as raw bytes; use 32 random ones. `UNIBLOX_ISSUE_TOKENS=1` serves
//! `GET /_uniblox/token?room=<room>` (for a hosted demo; it needs the secret);
//! `UNIBLOX_TRUST_FORWARDED_FOR=N` takes the client IP for the limits from `X-Forwarded-For` behind
//! N trusted proxies. `services mint-token <room> [lifetime-secs]` prints a token (for an operator,
//! e.g. a Mode-3 server's `UNIBLOX_SIGNALING_URL`). Warnings are logged unless `RUST_LOG` says
//! otherwise.
//!
//! TURN (ADR-0050): with `UNIBLOX_TURN_SECRET` (coturn's `static-auth-secret`) and `UNIBLOX_TURN_URLS`
//! (comma-separated `turn:` URLs), `GET /_uniblox/turn?room=<room>&token=<join token>` hands a client
//! with a valid join token a credential valid for `UNIBLOX_TURN_TTL_SECS` (default 3600, at most a
//! day, and never past the join token's expiry). It needs the signaling secret (32 bytes or more, as
//! does the TURN secret). Combined with `UNIBLOX_ISSUE_TOKENS=1` TURN is open to anyone, so the
//! binary refuses it unless `UNIBLOX_TURN_FOR_OPEN_TOKENS=1`.
//!
//! Clients behind one NAT share the per-IP limits; `UNIBLOX_MAX_CONNECTIONS_PER_IP` (default 16) and
//! `UNIBLOX_CONNECTS_BURST` (default 30) raise them for large groups.
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
    ClientIp, JoinTokenKey, MAX_TOKEN_LIFETIME_SECS, MAX_TURN_TTL_SECS, MIN_KEY_BYTES,
    RedisRegistryStore, SessionRegistry, SignalingConfig, SignalingLimits, TurnConfig,
    build_signaling_server_with, unix_now, validate_room,
};

/// matchbox's conventional signaling port.
const DEFAULT_PORT: u16 = 3536;

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// The join-token key from `UNIBLOX_SIGNALING_SECRET` (raw bytes, UTF-8 or not), or exit with the
/// reason.
fn key_from_env() -> Option<JoinTokenKey> {
    let secret = std::env::var_os("UNIBLOX_SIGNALING_SECRET")?;
    match JoinTokenKey::new(secret.as_encoded_bytes()) {
        Ok(key) => Some(key),
        Err(err) => {
            eprintln!("[signaling] UNIBLOX_SIGNALING_SECRET: {err}");
            std::process::exit(2);
        }
    }
}

/// A positive whole number from the environment variable `name`, or exit with the reason. `None` when
/// unset.
fn env_number<T: std::str::FromStr + Default + PartialEq>(name: &str) -> Option<T> {
    let value = std::env::var(name).ok()?;
    match value.parse::<T>() {
        Ok(n) if n != T::default() => Some(n),
        _ => {
            eprintln!("[signaling] {name} must be a positive whole number");
            std::process::exit(2);
        }
    }
}

/// The TURN relay from `UNIBLOX_TURN_SECRET`, `UNIBLOX_TURN_URLS` and `UNIBLOX_TURN_TTL_SECS`, or exit
/// with the reason. `None` without a TURN secret.
fn turn_from_env() -> Option<TurnConfig> {
    let secret = std::env::var_os("UNIBLOX_TURN_SECRET")?;
    if secret.len() < MIN_KEY_BYTES {
        // Anyone holding one credential can guess a weak shared secret offline.
        eprintln!("[signaling] UNIBLOX_TURN_SECRET must be at least {MIN_KEY_BYTES} bytes");
        std::process::exit(2);
    }
    let urls: Vec<String> = std::env::var("UNIBLOX_TURN_URLS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .collect();
    if urls.is_empty() {
        eprintln!(
            "[signaling] UNIBLOX_TURN_SECRET needs UNIBLOX_TURN_URLS (comma-separated turn: URLs)"
        );
        std::process::exit(2);
    }
    if let Some(bad) = urls.iter().find(|u| {
        !["turn:", "turns:", "stun:"]
            .iter()
            .any(|s| u.starts_with(s))
    }) {
        eprintln!("[signaling] UNIBLOX_TURN_URLS: {bad:?} is not a turn:, turns: or stun: URL");
        std::process::exit(2);
    }
    let ttl = match std::env::var("UNIBLOX_TURN_TTL_SECS")
        .ok()
        .map(|s| s.parse::<u64>())
    {
        None => 3600,
        Some(Ok(secs)) if (1..=MAX_TURN_TTL_SECS).contains(&secs) => secs,
        Some(_) => {
            eprintln!(
                "[signaling] UNIBLOX_TURN_TTL_SECS must be a whole number from 1 to {MAX_TURN_TTL_SECS}"
            );
            std::process::exit(2);
        }
    };
    Some(TurnConfig {
        secret: secret.as_encoded_bytes().to_vec(),
        urls,
        ttl: Duration::from_secs(ttl),
    })
}

/// `services mint-token <room> [lifetime-secs]`: print a join token for `room`.
fn mint_token(args: &[String]) -> ! {
    let Some(room) = args.first() else {
        eprintln!("usage: services mint-token <room> [lifetime-secs]");
        std::process::exit(2);
    };
    if let Err(reason) = validate_room(room) {
        eprintln!("not a room the gate accepts: {reason}");
        std::process::exit(2);
    }
    let lifetime = match args.get(1).map(|s| s.parse::<u64>()) {
        None => 3600,
        Some(Ok(secs)) if (1..=MAX_TOKEN_LIFETIME_SECS).contains(&secs) => secs,
        Some(_) => {
            eprintln!("lifetime-secs must be a whole number from 1 to {MAX_TOKEN_LIFETIME_SECS}");
            std::process::exit(2);
        }
    };
    let Some(key) = key_from_env() else {
        eprintln!("mint-token needs UNIBLOX_SIGNALING_SECRET");
        std::process::exit(2);
    };
    let Some(expires) = unix_now().checked_add(lifetime) else {
        eprintln!("the expiry overflows");
        std::process::exit(2);
    };
    println!("{}", key.mint(room, expires));
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
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
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
    let issue_tokens = flag("UNIBLOX_ISSUE_TOKENS");
    if issue_tokens && auth.is_none() {
        eprintln!("[signaling] UNIBLOX_ISSUE_TOKENS=1 needs UNIBLOX_SIGNALING_SECRET");
        std::process::exit(2);
    }
    let client_ip = match std::env::var("UNIBLOX_TRUST_FORWARDED_FOR").ok() {
        None => ClientIp::Peer,
        Some(v) => match v.parse::<usize>() {
            Ok(hops) if hops >= 1 => ClientIp::ForwardedFor(hops),
            _ => {
                eprintln!(
                    "[signaling] UNIBLOX_TRUST_FORWARDED_FOR must be the number of trusted proxies (>= 1)"
                );
                std::process::exit(2);
            }
        },
    };
    let turn = turn_from_env();
    if turn.is_some() && issue_tokens && !flag("UNIBLOX_TURN_FOR_OPEN_TOKENS") {
        // The open token endpoint admits anyone, so TURN behind it is an open relay.
        eprintln!(
            "[signaling] TURN with UNIBLOX_ISSUE_TOKENS=1 is an open relay (anyone can get a token); \
             set UNIBLOX_TURN_FOR_OPEN_TOKENS=1 to accept that (and cap it with coturn's quotas)"
        );
        std::process::exit(2);
    }
    if turn.is_some() && auth.is_none() {
        eprintln!(
            "[signaling] UNIBLOX_TURN_SECRET needs UNIBLOX_SIGNALING_SECRET (credentials go to authenticated clients only)"
        );
        std::process::exit(2);
    }
    // Clients behind one NAT share these per-IP limits: an operator expecting large groups raises them.
    let mut limits = SignalingLimits::default();
    if let Some(n) = env_number("UNIBLOX_MAX_CONNECTIONS_PER_IP") {
        limits.max_connections_per_ip = n;
    }
    if let Some(n) = env_number("UNIBLOX_CONNECTS_BURST") {
        limits.connects_burst = n;
    }
    let config = SignalingConfig {
        auth,
        issue_tokens,
        token_lifetime: Duration::from_secs(600),
        turn,
        limits,
        client_ip,
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
