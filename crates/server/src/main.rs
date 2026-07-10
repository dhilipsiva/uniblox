//! uniblox Mode-3 authoritative server binary: connects to signaling, then
//! runs the headless fixed-tick App (see lib.rs / ADR-0014).
//!
//! Env: `UNIBLOX_SIGNALING_URL` (default `ws://127.0.0.1:3536/uniblox-demo`),
//! `UNIBLOX_SERVER_ENTITIES` (default 2).

use std::time::{Duration, Instant};

use bevy_app::AppExit;
use protocol::PeerId;
use server::build_server_app;
use transport::Transport;

const ID_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

fn main() {
    let url = std::env::var("UNIBLOX_SIGNALING_URL")
        .unwrap_or_else(|_| "ws://127.0.0.1:3536/uniblox-demo".to_string());
    let entities = match std::env::var("UNIBLOX_SERVER_ENTITIES") {
        Ok(v) => v.parse::<usize>().unwrap_or_else(|_| {
            eprintln!("[server] UNIBLOX_SERVER_ENTITIES={v:?} is not a number — using 2");
            2
        }),
        Err(_) => 2,
    };

    println!("[server] connecting to signaling at {url}");
    let (mut transport, loop_fut) = Transport::connect(&url);
    // The message loop is executor-agnostic (async-compat supplies a tokio
    // context internally, ADR-0012); a plain blocked thread drives it.
    std::thread::spawn(move || {
        if let Err(err) = futures::executor::block_on(loop_fut) {
            eprintln!("[server] transport message loop ended: {err:?}");
        }
    });

    // Wait for the signaling-assigned id (our protocol identity derives from it).
    let started = Instant::now();
    let uuid = loop {
        if let Some(id) = transport.id() {
            break id;
        }
        if started.elapsed() > ID_WAIT_TIMEOUT {
            eprintln!(
                "[server] no signaling id within {ID_WAIT_TIMEOUT:?} — is the signaling server up?"
            );
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let local = PeerId::from_uuid_bytes(*uuid.0.as_bytes());
    println!("[server] up as {local:?}; owning {entities} entities; fixed tick 64 Hz");

    match build_server_app(transport, local, entities).run() {
        AppExit::Success => {}
        AppExit::Error(code) => std::process::exit(code.get() as i32),
    }
}
