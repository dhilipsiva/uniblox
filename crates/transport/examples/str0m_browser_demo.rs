//! A runnable native `Str0mPeer` that joins the `uniblox-demo` signaling room
//! and exchanges greetings on both channels — the native counterpart of the
//! wasm transport demo (`crates/client/src/main.rs`), used to verify str0m ↔
//! BROWSER matchbox interop on a desktop browser (ADR-0015).
//!
//! Run (inside the flake env):
//!   cargo run -p services                                    # signaling :3536
//!   cargo run -p transport --example str0m_browser_demo      # this peer
//! then open the wasm demo (scripts/serve.sh, http://localhost:8080/) in a
//! desktop browser tab. Each side should log `[STATE]` and `[EVENT]` receipts
//! from the other.

// Str0mPeer is native-only (cfg-gated off wasm); keep the example off wasm too
// so a future `--all-targets` wasm build cannot try to compile it.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashSet;
use std::time::Duration;

use transport::{PeerState, Str0mPeer};

const ROOM_URL: &str = "ws://127.0.0.1:3536/uniblox-demo";
const TICK: Duration = Duration::from_millis(100);

fn main() {
    // Surface Str0mPeer's internal `log` diagnostics (dropped packets, channel
    // write failures, ICE disconnects) when RUST_LOG is set — invaluable for
    // interop debugging; silent when RUST_LOG is unset.
    let _ = env_logger::try_init();
    let mut peer = Str0mPeer::connect(ROOM_URL);
    println!("[str0m-demo] connecting to {ROOM_URL}");

    // Peers we've already sent our one-shot reliable greeting to.
    let mut greeted: HashSet<_> = HashSet::new();
    // Peers currently connected (we re-send the unreliable state each tick).
    let mut connected: HashSet<_> = HashSet::new();

    // Print the connection telemetry (ADR-0018) every ~2 s.
    const TELEMETRY_EVERY: u32 = 20;
    let mut ticks: u32 = 0;

    loop {
        match peer.poll_peers() {
            Ok(updates) => {
                for (id, state) in updates {
                    println!("[str0m-demo] peer {id}: {state:?}");
                    match state {
                        PeerState::Connected => {
                            connected.insert(id);
                        }
                        PeerState::Disconnected => {
                            connected.remove(&id);
                            greeted.remove(&id);
                        }
                    }
                }
            }
            Err(err) => {
                println!("[str0m-demo] transport closed, stopping: {err}");
                break;
            }
        }

        for &id in &connected {
            // State channel (0) is unreliable/unordered — re-send every tick so
            // a dropped greeting still lands.
            if peer.send_state(id, (*b"state-hello").into()).is_err() {
                continue;
            }
            // Event channel (1) is reliable — send our greeting exactly once.
            if greeted.insert(id) {
                let _ = peer.send_event(id, (*b"event-hello").into());
            }
        }

        for (id, pkt) in peer.recv_state() {
            println!(
                "[str0m-demo][STATE] from {id}: {}",
                String::from_utf8_lossy(&pkt)
            );
        }
        for (id, pkt) in peer.recv_events() {
            println!(
                "[str0m-demo][EVENT] from {id}: {}",
                String::from_utf8_lossy(&pkt)
            );
        }

        ticks += 1;
        if ticks.is_multiple_of(TELEMETRY_EVERY) {
            for (id, t) in peer.telemetry() {
                let rtt = t
                    .rtt_mean
                    .map(|d| format!("{:.1}ms", d.as_secs_f64() * 1e3))
                    .unwrap_or_else(|| "-".to_string());
                let jitter = t
                    .rtt_jitter
                    .map(|d| format!("{:.1}ms", d.as_secs_f64() * 1e3))
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "[str0m-demo][TELEMETRY] {id}: outcome={:?} local={:?} rtt={rtt} jitter={jitter} samples={}",
                    t.outcome, t.local_candidate, t.rtt_samples
                );
            }
        }

        std::thread::sleep(TICK);
    }
}
