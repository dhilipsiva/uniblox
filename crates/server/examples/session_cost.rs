//! The measured inputs of the Mode-3 per-session cost model (ADR-0052, docs/cost-model.md): one
//! real `server` process per session, joined by K real clients (full replication: they apply state
//! and ack it, so delta baselines quiet down as they would in play), measured from /proc over a
//! steady window. Linux only (it reads /proc). Run it in release: `just session-cost`.
//!
//! Prints, per client count: the server's CPU (share of one core), its resident memory, and the
//! bytes each client received per second.

use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bevy_ecs::prelude::World;
use engine_core::insert_sim;
use protocol::PeerId;
use replication::{Replication, SigningKey};
use transport::{PeerState, Transport};

const ENTITIES: usize = 8;
const WARMUP: Duration = Duration::from_secs(5);
const WINDOW: Duration = Duration::from_secs(20);

/// utime + stime of a process, in clock ticks.
fn cpu_ticks(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    // The command name may contain spaces; the fields after it are space-separated.
    let after = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
    let fields: Vec<&str> = after.split_whitespace().collect();
    let field = |i: usize| {
        fields
            .get(i)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    // Fields 14 and 15 of /proc/<pid>/stat (utime, stime) are 11 and 12 after the name.
    field(11) + field(12)
}

/// A `/proc/<pid>/status` value in kB.
fn status_kb(pid: u32, key: &str) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// One client: connect, replicate (apply + ack), and count the bytes it receives.
fn client(url: String, bytes: Arc<AtomicU64>, stop: Arc<AtomicBool>, seed: u8) {
    let (mut transport, loop_fut) = Transport::connect(&url);
    std::thread::spawn(move || {
        let _ = futures::executor::block_on(loop_fut);
    });
    let uuid = loop {
        if let Some(id) = transport.id() {
            break id;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let local = PeerId::from_uuid_bytes(*uuid.0.as_bytes());
    let mut world = World::new();
    insert_sim(&mut world, local, 1.0 / 64.0);
    let mut repl = Replication::new(&mut world, SigningKey::from_bytes(&[seed; 32]));
    while !stop.load(Ordering::Relaxed) {
        if let Ok(peers) = transport.poll_peers() {
            for (peer, state) in peers {
                let id = PeerId::from_uuid_bytes(*peer.0.as_bytes());
                match state {
                    PeerState::Connected => repl.on_peer_connected(id),
                    PeerState::Disconnected => repl.untrack_peer(id),
                }
            }
        }
        for (from, packet) in transport.recv_events() {
            bytes.fetch_add(packet.len() as u64, Ordering::Relaxed);
            repl.apply_events(
                &mut world,
                PeerId::from_uuid_bytes(*from.0.as_bytes()),
                &packet,
            );
        }
        for (from, packet) in transport.recv_state() {
            bytes.fetch_add(packet.len() as u64, Ordering::Relaxed);
            repl.apply_state(
                &mut world,
                PeerId::from_uuid_bytes(*from.0.as_bytes()),
                &packet,
            );
        }
        let connected: Vec<_> = transport.connected_peers().collect();
        for (target, ack) in repl.drain_acks() {
            if let Some(peer) = connected
                .iter()
                .find(|p| PeerId::from_uuid_bytes(*p.0.as_bytes()) == target)
            {
                let _ = transport.send_event(*peer, ack);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn server_binary() -> std::path::PathBuf {
    // target/<profile>/examples/session_cost -> target/<profile>/server
    let exe = std::env::current_exe().expect("current exe");
    exe.parent()
        .and_then(|examples| examples.parent())
        .map(|profile| profile.join("server"))
        .expect("the server binary next to the examples dir")
}

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio");
    let signaling = rt.block_on(async {
        let mut server = services::build_signaling_server(
            (std::net::Ipv4Addr::LOCALHOST, 0),
            services::SessionRegistry::new(),
        );
        let addr = server.bind().expect("bind signaling");
        tokio::spawn(server.serve());
        addr
    });
    let ticks_per_sec = 100.0; // CLK_TCK on Linux
    println!(
        "server: {ENTITIES} entities, 64 Hz; window {}s after {}s warm-up",
        WINDOW.as_secs(),
        WARMUP.as_secs()
    );
    println!("clients | server CPU (% of a core) | server RSS (MiB) | per-client bytes/s");
    for clients in [1usize, 4, 8] {
        let url = format!("ws://{signaling}/m3~1.0~0~cost{clients}?engine=1");
        let mut server: Child = Command::new(server_binary())
            .env("UNIBLOX_SIGNALING_URL", &url)
            .env("UNIBLOX_SERVER_ENTITIES", ENTITIES.to_string())
            .env("UNIBLOX_IDLE_SHUTDOWN_SECS", "0")
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("run the server binary (build it first: cargo build --release -p server)");
        std::thread::sleep(Duration::from_secs(1));
        let stop = Arc::new(AtomicBool::new(false));
        let counters: Vec<Arc<AtomicU64>> =
            (0..clients).map(|_| Arc::new(AtomicU64::new(0))).collect();
        let handles: Vec<_> = counters
            .iter()
            .enumerate()
            .map(|(i, bytes)| {
                let (url, bytes, stop) = (url.clone(), Arc::clone(bytes), Arc::clone(&stop));
                std::thread::spawn(move || client(url, bytes, stop, i as u8 + 1))
            })
            .collect();
        std::thread::sleep(WARMUP);
        let cpu0 = cpu_ticks(server.id());
        let bytes0: u64 = counters.iter().map(|c| c.load(Ordering::Relaxed)).sum();
        let t0 = Instant::now();
        std::thread::sleep(WINDOW);
        let secs = t0.elapsed().as_secs_f64();
        let cpu = (cpu_ticks(server.id()) - cpu0) as f64 / ticks_per_sec / secs * 100.0;
        let rss = status_kb(server.id(), "VmRSS:") as f64 / 1024.0;
        let bytes1: u64 = counters.iter().map(|c| c.load(Ordering::Relaxed)).sum();
        let per_client = (bytes1 - bytes0) as f64 / secs / clients as f64;
        println!("{clients:>7} | {cpu:>24.1} | {rss:>16.1} | {per_client:>18.0}");
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            let _ = h.join();
        }
        let _ = server.kill();
        let _ = server.wait();
    }
}
