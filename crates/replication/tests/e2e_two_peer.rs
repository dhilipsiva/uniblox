//! Tier C — T26 ★ end-to-end: two native peers over REAL transport (matchbox
//! WebRTC datachannels + in-process signaling), replicating and handing off.
//! Locked FIRST (TDD). Hermetic (loopback ICE), hard deadline, mirrors the
//! harness of `crates/transport/tests/two_peer.rs`.

use std::net::Ipv4Addr;
use std::time::Duration;

use bevy_ecs::prelude::*;
use engine_core::{Owner, Position, Velocity, insert_sim, simulate, spawn_owned};
use matchbox_signaling::SignalingServer;
use protocol::PeerId;
use replication::Replication;
use transport::{PeerState, Transport};

const DT: f32 = 0.5;
const TOL: f32 = 0.5 / 1024.0;
const DEADLINE: Duration = Duration::from_secs(120); // generous: bounds hangs, not CPU contention (full parallel suite runs several e2e binaries)
const POLL: Duration = Duration::from_millis(20);

fn start_signaling() -> String {
    let mut server = SignalingServer::full_mesh_builder((Ipv4Addr::LOCALHOST, 0)).build();
    let addr = server.bind().expect("signaling server must bind");
    tokio::spawn(server.serve());
    format!("ws://{addr}/replication_e2e")
}

struct Peer {
    world: World,
    schedule: Schedule,
    repl: Replication,
    transport: Transport,
    id: PeerId,
}

impl Peer {
    /// Connect transport, wait for the signaling-assigned id, then build the
    /// world around the derived protocol PeerId.
    async fn connect(room: &str) -> Peer {
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
        insert_sim(&mut world, id, DT);
        let mut schedule = Schedule::default();
        schedule.add_systems(simulate);
        let repl = Replication::new(&mut world);
        Peer {
            world,
            schedule,
            repl,
            transport,
            id,
        }
    }

    /// One full pump: peer bookkeeping, sim tick, collect+send, receive+apply.
    fn pump(&mut self) {
        let peers: Vec<_> = self.transport.poll_peers().expect("transport open");
        for (peer, state) in peers {
            if matches!(state, PeerState::Connected) {
                self.repl
                    .on_peer_connected(PeerId::from_uuid_bytes(*peer.0.as_bytes()));
            }
        }
        self.schedule.run(&mut self.world);
        // Per-peer collect (ADR-0021): route each peer's AOI-gated outbox back
        // to its transport peer.
        let connected: Vec<_> = self.transport.connected_peers().collect();
        for (target, out) in self.repl.collect_all(&mut self.world) {
            let Some(peer) = connected
                .iter()
                .find(|p| PeerId::from_uuid_bytes(*p.0.as_bytes()) == target)
            else {
                continue;
            };
            if let Some(state) = out.state {
                let _ = self.transport.send_state(*peer, state);
            }
            for ev in out.events {
                let _ = self.transport.send_event(*peer, ev);
            }
        }
        for (from, bytes) in self.transport.recv_events() {
            let from = PeerId::from_uuid_bytes(*from.0.as_bytes());
            self.repl.apply_events(&mut self.world, from, &bytes);
        }
        for (from, bytes) in self.transport.recv_state() {
            let from = PeerId::from_uuid_bytes(*from.0.as_bytes());
            self.repl.apply_state(&mut self.world, from, &bytes);
        }
    }

    fn entity_owned_by(&mut self, owner: PeerId) -> Option<(Entity, Position)> {
        let found: Vec<_> = self
            .world
            .query::<(Entity, &Owner, &Position)>()
            .iter(&self.world)
            .filter(|(_, o, _)| o.0 == owner)
            .map(|(e, _, p)| (e, *p))
            .collect();
        found.first().copied()
    }
}

/// T26 ★ spawn → replicate → converge (two advancing observations) → handoff
/// completes end-to-end; reliable events survive an unreliable state channel.
#[tokio::test(flavor = "multi_thread")]
async fn e2e_two_peer_replication_and_handoff() {
    let room = start_signaling();
    let mut a = Peer::connect(&room).await;
    let mut b = Peer::connect(&room).await;

    // A owns one moving entity.
    let e_a = spawn_owned(
        &mut a.world,
        a.id,
        Position { x: 0.0, y: 0.0 },
        Velocity { x: 2.0, y: 0.0 },
    );

    // Phase 1: B acquires a proxy that tracks A's truth at two advancing
    // observation points (proves continuous replication, not one lucky packet).
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut first_observation: Option<f32> = None;
    loop {
        a.pump();
        b.pump();
        if let Some((_, proxy_pos)) = b.entity_owned_by(a.id) {
            let truth = *a.world.get::<Position>(e_a).unwrap();
            // Observation windows: proxy must be within [truth - a few ticks, truth].
            if (truth.x - proxy_pos.x).abs() <= 2.0 * DT * 4.0 + TOL {
                match first_observation {
                    None => first_observation = Some(proxy_pos.x),
                    Some(first) if proxy_pos.x > first + 1.0 => break, // advanced
                    _ => {}
                }
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "proxy never converged/advanced"
        );
        tokio::time::sleep(POLL).await;
    }

    // Phase 2: A → B handoff over the wire.
    a.repl
        .transfer_ownership(&mut a.world, e_a, b.id)
        .expect("A owns e_a");
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        a.pump();
        b.pump();
        let a_says = a.world.get::<Owner>(e_a).map(|o| o.0);
        let b_proxy = b.entity_owned_by(b.id);
        if a_says == Some(b.id) && b_proxy.is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "handoff never completed end-to-end"
        );
        tokio::time::sleep(POLL).await;
    }

    // Post-handoff: B computes the entity (it advances under B's authority)
    // and replicates it BACK to A under the same identity — A's view of e_a
    // must ADVANCE too (A now applies B's state through the same gates).
    // NOTE: checking A's Owner view alone would be trivially true from A's
    // local flip at transfer time (auditor finding) — the position advance is
    // the real replicate-back evidence.
    let (proxy_e, before) = b.entity_owned_by(b.id).unwrap();
    let a_before = *a.world.get::<Position>(e_a).unwrap();
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        a.pump();
        b.pump();
        let now = *b.world.get::<Position>(proxy_e).unwrap();
        let a_now = *a.world.get::<Position>(e_a).unwrap();
        if now.x > before.x + 1.0 && a_now.x > a_before.x + 1.0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "adopted entity never advanced under B's authority AND replicated back to A"
        );
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        a.world.get::<Owner>(e_a).map(|o| o.0),
        Some(b.id),
        "A must regard B as the owner"
    );
}
