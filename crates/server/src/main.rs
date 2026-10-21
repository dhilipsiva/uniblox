//! uniblox Mode-3 authoritative server binary: connects to signaling, then
//! runs the headless fixed-tick App (see lib.rs / ADR-0014).
//!
//! Env: `UNIBLOX_SIGNALING_URL` (default `ws://127.0.0.1:3536/uniblox-demo`; a join token for an
//! authenticated signaling server rides its query, `services mint-token`),
//! `UNIBLOX_SERVER_ENTITIES` (default 2), `UNIBLOX_BUNDLE` (optional: a content bundle's manifest,
//! whose script then drives the server-owned entities, ADR-0045).
//!
//! Operations (ADR-0052): `UNIBLOX_SERVER_HTTP_PORT` serves `/healthz` and `/metrics` (off if
//! unset); `UNIBLOX_IDLE_SHUTDOWN_SECS` (default 300, 0 = never) exits cleanly once no client has been
//! connected that long, so an orchestrator reclaims the session; logs go to stderr, filtered by
//! `RUST_LOG` (default `info`), as JSON lines with `UNIBLOX_LOG_FORMAT=json`.

use std::time::{Duration, Instant};

use bevy_app::AppExit;
use protocol::PeerId;
use server::build_server_app;
use transport::Transport;

const ID_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Logging (ADR-0052): `RUST_LOG`-filtered (default `info`), as text or as JSON lines.
fn init_logging() {
    use std::io::Write;
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if std::env::var("UNIBLOX_LOG_FORMAT").is_ok_and(|v| v == "json") {
        builder.format(|buf, record| {
            let line = serde_json::json!({
                "ts_ms": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64),
                "level": record.level().as_str(),
                "target": record.target(),
                "msg": record.args().to_string(),
            });
            writeln!(buf, "{line}")
        });
    }
    builder.init();
}

/// A whole number from the environment, or exit with the reason; `None` when unset.
fn env_secs(name: &str) -> Option<u64> {
    let value = std::env::var(name).ok()?;
    match value.parse::<u64>() {
        Ok(n) => Some(n),
        Err(_) => {
            log::error!("[server] {name}={value:?} is not a whole number");
            std::process::exit(2);
        }
    }
}

fn main() {
    init_logging();
    let url = std::env::var("UNIBLOX_SIGNALING_URL")
        .unwrap_or_else(|_| "ws://127.0.0.1:3536/uniblox-demo".to_string());
    let entities = match std::env::var("UNIBLOX_SERVER_ENTITIES") {
        Ok(v) => v.parse::<usize>().unwrap_or_else(|_| {
            log::warn!("[server] UNIBLOX_SERVER_ENTITIES={v:?} is not a number — using 2");
            2
        }),
        Err(_) => 2,
    };

    log::info!("[server] connecting to signaling at {url}");
    let (mut transport, loop_fut) = Transport::connect(&url);
    // The message loop is executor-agnostic (async-compat supplies a tokio
    // context internally, ADR-0012); a plain blocked thread drives it.
    std::thread::spawn(move || {
        if let Err(err) = futures::executor::block_on(loop_fut) {
            log::error!("[server] transport message loop ended: {err:?}");
        }
    });

    // Wait for the signaling-assigned id (our protocol identity derives from it).
    let started = Instant::now();
    let uuid = loop {
        if let Some(id) = transport.id() {
            break id;
        }
        if started.elapsed() > ID_WAIT_TIMEOUT {
            log::error!(
                "[server] no signaling id within {ID_WAIT_TIMEOUT:?} — is the signaling server up?"
            );
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let local = PeerId::from_uuid_bytes(*uuid.0.as_bytes());
    log::info!("[server] up as {local:?}; owning {entities} entities; fixed tick 64 Hz");

    let mut app = build_server_app(transport, local, entities);
    let idle = match env_secs("UNIBLOX_IDLE_SHUTDOWN_SECS").unwrap_or(300) {
        0 => None,
        secs => Some(Duration::from_secs(secs)),
    };
    let ops = server::enable_ops(&mut app, idle);
    if let Some(port) = env_secs("UNIBLOX_SERVER_HTTP_PORT") {
        let Ok(port) = u16::try_from(port) else {
            log::error!("[server] UNIBLOX_SERVER_HTTP_PORT={port} is not a port");
            std::process::exit(2);
        };
        match std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)) {
            Ok(listener) => {
                log::info!("[server] /healthz and /metrics on port {port}");
                std::thread::spawn(move || server::serve_ops(listener, ops));
            }
            Err(err) => {
                log::error!("[server] cannot listen on port {port}: {err}");
                std::process::exit(1);
            }
        }
    }
    if let Ok(manifest) = std::env::var("UNIBLOX_BUNDLE") {
        if let Err(err) = server::load_content(&mut app, std::path::Path::new(&manifest)) {
            log::error!("[server] cannot load the bundle {manifest}: {err}");
            std::process::exit(1);
        }
        log::info!("[server] content loaded from {manifest}");
    }
    match app.run() {
        AppExit::Success => {}
        AppExit::Error(code) => std::process::exit(code.get() as i32),
    }
}
