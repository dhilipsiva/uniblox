//! Slice instrumentation harness (the Phase-1 Instrumentation item).
//!
//! Measures what the running slice can measure natively TODAY and writes
//! `target/slice-metrics.json` for `scripts/slice-check.sh` to print:
//! - replication bandwidth/peer at the ~20 Hz network tick (real wire bytes —
//!   platform-independent),
//! - peer RTT + jitter via DataChannel ping/echo (loopback; precision bounded
//!   by the ~1 ms harness poll),
//! - ed25519 sign/verify cost, NATIVE (the in-browser variant needs a desktop
//!   browser — WSL2 headless Chrome cannot complete the matchbox handshake,
//!   ADR-0012).
//!
//! Run: `direnv exec . cargo run -p replication --example slice_metrics`

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use engine_core::{Position, Velocity, insert_sim, simulate, spawn_owned};
use matchbox_signaling::SignalingServer;
use protocol::PeerId;
use replication::Replication;
use transport::{PeerState, Transport};

const NET_TICK: Duration = Duration::from_millis(50); // ~20 Hz, the assumed net tick
const BANDWIDTH_WINDOW: Duration = Duration::from_secs(3);
const PING_COUNT: usize = 50;
const CONNECT_DEADLINE: Duration = Duration::from_secs(30);

fn start_signaling(room: &str) -> String {
    let mut server = SignalingServer::full_mesh_builder((Ipv4Addr::LOCALHOST, 0)).build();
    let addr = server.bind().expect("signaling server must bind");
    tokio::spawn(server.serve());
    format!("ws://{addr}/{room}")
}

async fn connect(room_url: &str) -> (Transport, PeerId) {
    let (mut transport, loop_fut) = Transport::connect_hermetic(room_url);
    tokio::spawn(loop_fut);
    let deadline = tokio::time::Instant::now() + CONNECT_DEADLINE;
    let uuid = loop {
        if let Some(id) = transport.id() {
            break id;
        }
        assert!(tokio::time::Instant::now() < deadline, "no signaling id");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let id = PeerId::from_uuid_bytes(*uuid.0.as_bytes());
    (transport, id)
}

/// Wait until the transport reports one Connected peer; return its transport id.
async fn wait_peer(t: &mut Transport) -> transport::PeerId {
    let deadline = tokio::time::Instant::now() + CONNECT_DEADLINE;
    loop {
        for (peer, state) in t.poll_peers().expect("transport open") {
            if matches!(state, PeerState::Connected) {
                return peer;
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "no peer connected");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

struct BandwidthMetrics {
    state_bytes_per_sec: f64,
    events_bytes_per_sec: f64,
    state_msgs_per_sec: f64,
    entities: usize,
}

/// A real two-peer replication session: A owns `entities` moving entities,
/// B applies. Measure what A broadcasts per second at the net tick.
async fn measure_bandwidth(entities: usize) -> BandwidthMetrics {
    let room = start_signaling("metrics_bandwidth");
    let (mut ta, a_id) = connect(&room).await;
    let (mut tb, b_id) = connect(&room).await;
    let peer_b = wait_peer(&mut ta).await;
    let _peer_a = wait_peer(&mut tb).await;

    let mut world_a = World::new();
    insert_sim(&mut world_a, a_id, 1.0 / 64.0);
    let mut sched_a = Schedule::default();
    sched_a.add_systems(simulate);
    let mut repl_a = Replication::new(&mut world_a);
    for i in 0..entities {
        spawn_owned(
            &mut world_a,
            a_id,
            Position {
                x: 0.0,
                y: i as f32,
            },
            Velocity {
                x: 2.0,
                y: 0.5 * i as f32,
            },
        );
    }

    let mut world_b = World::new();
    insert_sim(&mut world_b, b_id, 1.0 / 64.0);
    let mut repl_b = Replication::new(&mut world_b);

    let mut state_bytes = 0usize;
    let mut events_bytes = 0usize;
    let mut state_msgs = 0usize;
    let started = Instant::now();
    let mut measuring = false;
    let mut measure_start = Instant::now();

    // First second is warm-up (spawn events, first snapshot); then measure.
    while started.elapsed() < BANDWIDTH_WINDOW + Duration::from_secs(1) {
        sched_a.run(&mut world_a);
        let out = repl_a.collect(&mut world_a);
        if !measuring && started.elapsed() >= Duration::from_secs(1) {
            measuring = true;
            measure_start = Instant::now();
        }
        if let Some(state) = &out.state {
            if measuring {
                state_bytes += state.len();
                state_msgs += 1;
            }
            let _ = ta.send_state(peer_b, state.clone());
        }
        for ev in &out.events {
            if measuring {
                events_bytes += ev.len();
            }
            let _ = ta.send_event(peer_b, ev.clone());
        }
        for (from, bytes) in tb.recv_events() {
            let from = PeerId::from_uuid_bytes(*from.0.as_bytes());
            repl_b.apply_events(&mut world_b, from, &bytes);
        }
        for (from, bytes) in tb.recv_state() {
            let from = PeerId::from_uuid_bytes(*from.0.as_bytes());
            repl_b.apply_state(&mut world_b, from, &bytes);
        }
        let _ = tb.poll_peers();
        let _ = ta.poll_peers();
        tokio::time::sleep(NET_TICK).await;
    }
    let secs = measure_start.elapsed().as_secs_f64();

    BandwidthMetrics {
        state_bytes_per_sec: state_bytes as f64 / secs,
        events_bytes_per_sec: events_bytes as f64 / secs,
        state_msgs_per_sec: state_msgs as f64 / secs,
        entities,
    }
}

/// Raw-transport ping/echo on the reliable channel: RTT mean + jitter (stddev).
async fn measure_rtt() -> (f64, f64) {
    let room = start_signaling("metrics_ping");
    let (mut ta, _) = connect(&room).await;
    let (mut tb, _) = connect(&room).await;
    let peer_b = wait_peer(&mut ta).await;
    let peer_a = wait_peer(&mut tb).await;

    let mut samples_us: Vec<f64> = Vec::with_capacity(PING_COUNT);
    for seq in 0..PING_COUNT as u8 {
        let payload: Box<[u8]> = Box::new([seq]);
        let t0 = Instant::now();
        ta.send_event(peer_b, payload).expect("ping send");
        'echo: loop {
            // B echoes anything it receives.
            for (_, bytes) in tb.recv_events() {
                let _ = tb.send_event(peer_a, bytes);
            }
            for (_, bytes) in ta.recv_events() {
                if bytes.first() == Some(&seq) {
                    samples_us.push(t0.elapsed().as_secs_f64() * 1e6);
                    break 'echo;
                }
            }
            let _ = ta.poll_peers();
            let _ = tb.poll_peers();
            assert!(
                t0.elapsed() < Duration::from_secs(5),
                "ping {seq} never echoed"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mean = samples_us.iter().sum::<f64>() / samples_us.len() as f64;
    let var = samples_us.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / samples_us.len() as f64;
    (mean, var.sqrt())
}

/// ed25519 sign/verify micro-bench (native). Fixed key — no RNG needed.
fn measure_ed25519() -> (f64, f64) {
    use ed25519_dalek::{Signer, SigningKey, Verifier};
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let vk = key.verifying_key();
    let msg = [0xABu8; 64]; // a typical event-sized payload

    // Warm up.
    for _ in 0..100 {
        let sig = key.sign(&msg);
        vk.verify(&msg, &sig).expect("verify");
    }

    const ITERS: u32 = 1000;
    let t0 = Instant::now();
    let mut last_sig = key.sign(&msg);
    for _ in 0..ITERS {
        last_sig = key.sign(&msg);
    }
    let sign_us = t0.elapsed().as_secs_f64() * 1e6 / ITERS as f64;

    let t0 = Instant::now();
    for _ in 0..ITERS {
        vk.verify(&msg, &last_sig).expect("verify");
    }
    let verify_us = t0.elapsed().as_secs_f64() * 1e6 / ITERS as f64;

    (sign_us, verify_us)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!(
        "[slice-metrics] measuring replication bandwidth (2 entities, ~20 Hz net tick, 3 s window)..."
    );
    let bw = measure_bandwidth(2).await;
    println!(
        "  state:  {:.0} B/s ({:.1} msg/s)   events: {:.0} B/s",
        bw.state_bytes_per_sec, bw.state_msgs_per_sec, bw.events_bytes_per_sec
    );

    println!(
        "[slice-metrics] measuring RTT/jitter ({PING_COUNT} pings, reliable channel, loopback)..."
    );
    let (rtt_us, jitter_us) = measure_rtt().await;
    println!(
        "  rtt: {rtt_us:.0} us   jitter: {jitter_us:.0} us   (loopback; ~1 ms poll granularity)"
    );

    println!("[slice-metrics] measuring ed25519 sign/verify (native, 64 B msg, 1000 iters)...");
    let (sign_us, verify_us) = measure_ed25519();
    println!("  sign: {sign_us:.1} us   verify: {verify_us:.1} us");

    let json = serde_json::json!({
        "environment": "native loopback (WSL2); wire sizes are platform-independent",
        "net_tick_hz": 1.0 / NET_TICK.as_secs_f64(),
        "bandwidth": {
            "entities": bw.entities,
            "state_bytes_per_sec": bw.state_bytes_per_sec,
            "state_msgs_per_sec": bw.state_msgs_per_sec,
            "events_bytes_per_sec": bw.events_bytes_per_sec,
        },
        "rtt": {
            "mean_us": rtt_us,
            "jitter_us": jitter_us,
            "note": "loopback; precision bounded by ~1 ms harness poll",
        },
        "ed25519": {
            "sign_us": sign_us,
            "verify_us": verify_us,
            "note": "native; in-browser measurement needs a desktop browser (ADR-0012)",
        },
    });
    let path = "target/slice-metrics.json";
    match serde_json::to_string_pretty(&json) {
        Ok(body) => {
            if let Err(err) = std::fs::write(path, body) {
                eprintln!("[slice-metrics] could not write {path}: {err}");
                std::process::exit(1);
            }
            println!("[slice-metrics] wrote {path} — run /slice-check to print the table");
        }
        Err(err) => {
            eprintln!("[slice-metrics] serialization failed: {err}");
            std::process::exit(1);
        }
    }
}
