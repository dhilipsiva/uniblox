//! M3 ★ / M4 — the REAL headless server App (bevy_app + bevy_time assembly):
//! fixed 64 Hz simulation in FixedUpdate, network pump in Update on its own
//! wall-clock cadence (network tick ≠ fixed tick). An external raw client
//! converges against it over real transport. Locked FIRST (TDD). ADR-0014.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use engine_core::{Owner, Position};
use matchbox_signaling::SignalingServer;
use protocol::PeerId;
use replication::Replication;
use server::{NET_INTERVAL, TickCount, build_server_app};
use transport::{PeerState, Transport};

const DEADLINE: Duration = Duration::from_secs(120); // generous: bounds hangs, not CPU contention (full parallel suite runs several e2e binaries)
const POLL: Duration = Duration::from_millis(5);

fn start_signaling() -> String {
    let mut server = SignalingServer::full_mesh_builder((Ipv4Addr::LOCALHOST, 0)).build();
    let addr = server.bind().expect("signaling server must bind");
    tokio::spawn(server.serve());
    format!("ws://{addr}/headless_app_test")
}

/// A raw test client (no App): world + replication + transport pump, counting
/// received state messages for the cadence assertion.
struct Client {
    world: World,
    repl: Replication,
    transport: Transport,
    received_state_msgs: usize,
}

impl Client {
    async fn connect(room: &str) -> Client {
        let (mut transport, loop_fut) = Transport::connect_hermetic(room);
        tokio::spawn(loop_fut);
        let deadline = tokio::time::Instant::now() + DEADLINE;
        let uuid = loop {
            if let Some(id) = transport.id() {
                break id;
            }
            assert!(tokio::time::Instant::now() < deadline, "no signaling id");
            tokio::time::sleep(POLL).await;
        };
        let id = PeerId::from_uuid_bytes(*uuid.0.as_bytes());
        let mut world = World::new();
        engine_core::insert_sim(&mut world, id, 1.0 / 64.0);
        let repl = Replication::new(&mut world);
        Client {
            world,
            repl,
            transport,
            received_state_msgs: 0,
        }
    }

    fn pump(&mut self) {
        let peers: Vec<_> = self.transport.poll_peers().expect("transport open");
        for (peer, state) in peers {
            if matches!(state, PeerState::Connected) {
                // This client owns nothing (Mode 3) — it only receives. Tracking
                // is inert here; kept for symmetry with the server-side pump.
                self.repl
                    .on_peer_connected(PeerId::from_uuid_bytes(*peer.0.as_bytes()));
            }
        }
        for (from, bytes) in self.transport.recv_events() {
            let from = PeerId::from_uuid_bytes(*from.0.as_bytes());
            self.repl.apply_events(&mut self.world, from, &bytes);
        }
        for (from, bytes) in self.transport.recv_state() {
            self.received_state_msgs += 1;
            let from = PeerId::from_uuid_bytes(*from.0.as_bytes());
            self.repl.apply_state(&mut self.world, from, &bytes);
        }
    }

    fn server_proxies(&mut self, server_id: PeerId) -> Vec<Position> {
        let mut found: Vec<(u32, Position)> = self
            .world
            .query::<(Entity, &Owner, &Position)>()
            .iter(&self.world)
            .filter(|(_, o, _)| o.0 == server_id)
            .map(|(e, _, p)| (e.index_u32(), *p))
            .collect();
        found.sort_by_key(|(idx, _)| *idx);
        found.into_iter().map(|(_, p)| p).collect()
    }
}

/// Boot a real server App against in-process signaling and return it with its
/// protocol id (the App is driven manually via `app.update()` — no blocking
/// `run()` in tests).
async fn boot_server(room: &str, entities: usize) -> (bevy_app::App, PeerId) {
    let (mut transport, loop_fut) = Transport::connect_hermetic(room);
    tokio::spawn(loop_fut);
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let uuid = loop {
        if let Some(id) = transport.id() {
            break id;
        }
        assert!(tokio::time::Instant::now() < deadline, "no signaling id");
        tokio::time::sleep(POLL).await;
    };
    let server_id = PeerId::from_uuid_bytes(*uuid.0.as_bytes());
    let app = build_server_app(transport, server_id, entities);
    (app, server_id)
}

/// M3 ★ the headless App: client converges to the server's entities; the
/// fixed clock drives the sim at ≈64 Hz; every server entity is server-owned.
#[tokio::test(flavor = "multi_thread")]
async fn headless_app_converges_and_ticks_at_64hz() {
    let room = start_signaling();
    let (mut app, server_id) = boot_server(&room, 2).await;
    let mut client = Client::connect(&room).await;

    // Phase 1: converge (two advancing observation points on entity 0).
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut first_obs: Option<f32> = None;
    loop {
        app.update();
        client.pump();
        let proxies = client.server_proxies(server_id);
        if proxies.len() == 2 {
            match first_obs {
                None => first_obs = Some(proxies[0].x),
                Some(first) if proxies[0].x > first + 0.5 => break,
                _ => {}
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "client never converged against the headless app"
        );
        tokio::time::sleep(POLL).await;
    }

    // Phase 2: the fixed clock drives the sim — ≈64 FixedUpdate ticks per
    // wall-second (soft bounds; the manual update loop runs much faster than
    // 64/s, so Time<Fixed> must be doing the regulation).
    let t0 = app.world().resource::<TickCount>().0;
    let wall_start = Instant::now();
    while wall_start.elapsed() < Duration::from_secs(2) {
        // 2s window: halves variance vs 1s — Time<Virtual> max_delta (250ms) permanently drops ticks on stalls (auditor F3)
        app.update();
        client.pump();
        tokio::time::sleep(POLL).await;
    }
    let elapsed = wall_start.elapsed().as_secs_f64();
    let ticks = app.world().resource::<TickCount>().0 - t0;
    let rate = ticks as f64 / elapsed;
    assert!(
        (45.0..=90.0).contains(&rate),
        "FixedUpdate must self-regulate to ~64 Hz; measured {rate:.1} Hz ({ticks} ticks in {elapsed:.2}s)"
    );

    // Mode-3 shape: every simulated entity on the server is server-owned.
    let world = app.world_mut();
    let owners: Vec<PeerId> = world.query::<&Owner>().iter(world).map(|o| o.0).collect();
    assert_eq!(owners.len(), 2);
    assert!(owners.iter().all(|o| *o == server_id));
}

/// M4 — cadence decoupling: state messages arrive at the ~20 Hz network
/// interval, NOT the 64 Hz fixed tick (entities move every fixed tick, so an
/// undivided pump would send ~64/s).
#[tokio::test(flavor = "multi_thread")]
async fn network_cadence_decoupled_from_fixed_tick() {
    let room = start_signaling();
    let (mut app, server_id) = boot_server(&room, 1).await;
    let mut client = Client::connect(&room).await;

    // Wait until state is flowing at all.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while client.server_proxies(server_id).is_empty() {
        app.update();
        client.pump();
        assert!(
            tokio::time::Instant::now() < deadline,
            "no state ever flowed"
        );
        tokio::time::sleep(POLL).await;
    }

    // Measure received state messages over ~1 wall-second.
    client.received_state_msgs = 0;
    let wall_start = Instant::now();
    while wall_start.elapsed() < Duration::from_secs(2) {
        // 2s window: halves variance vs 1s — Time<Virtual> max_delta (250ms) permanently drops ticks on stalls (auditor F3)
        app.update();
        client.pump();
        tokio::time::sleep(POLL).await;
    }
    let elapsed = wall_start.elapsed().as_secs_f64();
    let rate = client.received_state_msgs as f64 / elapsed;
    let net_hz = 1.0 / NET_INTERVAL.as_secs_f64();
    assert!(
        rate >= net_hz * 0.5 && rate <= net_hz * 1.75,
        "state msgs must track the ~{net_hz:.0} Hz network interval, not the 64 Hz \
         fixed tick; measured {rate:.1}/s"
    );
    assert!(
        rate < 45.0,
        "state cadence must be decoupled from the 64 Hz fixed tick; measured {rate:.1}/s"
    );
}
