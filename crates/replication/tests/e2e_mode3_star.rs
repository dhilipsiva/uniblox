//! M2 ★ Mode-3 star over REAL transport: one authoritative server + two
//! clients through in-process signaling and real WebRTC datachannels. The
//! server owns everything; both clients converge and emit nothing. Locked
//! FIRST (TDD). Hermetic (loopback ICE), hard deadline. See ADR-0014.
//!
//! "Star" needs no special topology: the room is a full mesh, but authority
//! assignment (server spawns/owns all) makes the server the only sender —
//! the star IS the data, not a transport mode.

use std::net::Ipv4Addr;
use std::time::Duration;

use bevy_ecs::prelude::*;
use engine_core::{Owner, Position, Velocity, insert_sim, simulate, spawn_owned};
use matchbox_signaling::SignalingServer;
use protocol::PeerId;
use replication::Replication;
use transport::{PeerState, Transport};

const DT: f32 = 0.5;
const DEADLINE: Duration = Duration::from_secs(120); // generous: bounds hangs, not CPU contention (full parallel suite runs several e2e binaries)
const POLL: Duration = Duration::from_millis(20);

fn start_signaling() -> String {
    let mut server = SignalingServer::full_mesh_builder((Ipv4Addr::LOCALHOST, 0)).build();
    let addr = server.bind().expect("signaling server must bind");
    tokio::spawn(server.serve());
    format!("ws://{addr}/mode3_star_e2e")
}

struct Peer {
    world: World,
    schedule: Schedule,
    repl: Replication,
    transport: Transport,
    id: PeerId,
    state_msgs_sent: usize,
    events_sent: usize,
}

impl Peer {
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
            state_msgs_sent: 0,
            events_sent: 0,
        }
    }

    fn pump(&mut self) {
        let peers: Vec<_> = self.transport.poll_peers().expect("transport open");
        for (peer, state) in peers {
            if matches!(state, PeerState::Connected) {
                let replay = self.repl.on_peer_connected(
                    &mut self.world,
                    PeerId::from_uuid_bytes(*peer.0.as_bytes()),
                );
                for ev in replay {
                    self.events_sent += 1;
                    let _ = self.transport.send_event(peer, ev);
                }
            }
        }
        self.schedule.run(&mut self.world);
        let out = self.repl.collect(&mut self.world);
        self.state_msgs_sent += usize::from(out.state.is_some());
        self.events_sent += out.events.len();
        let connected: Vec<_> = self.transport.connected_peers().collect();
        for peer in &connected {
            if let Some(state) = &out.state {
                let _ = self.transport.send_state(*peer, state.clone());
            }
            for ev in &out.events {
                let _ = self.transport.send_event(*peer, ev.clone());
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

    /// Positions of proxies owned by `owner`, sorted for stable comparison.
    fn positions_owned_by(&mut self, owner: PeerId) -> Vec<Position> {
        let mut found: Vec<(u32, Position)> = self
            .world
            .query::<(Entity, &Owner, &Position)>()
            .iter(&self.world)
            .filter(|(_, o, _)| o.0 == owner)
            .map(|(e, _, p)| (e.index_u32(), *p))
            .collect();
        found.sort_by_key(|(idx, _)| *idx);
        found.into_iter().map(|(_, p)| p).collect()
    }
}

/// The Mode-3 star: server owns all, both clients converge at two advancing
/// observation points, clients send NOTHING end-to-end.
#[tokio::test(flavor = "multi_thread")]
async fn e2e_mode3_star_server_owns_all() {
    let room = start_signaling();
    let mut server = Peer::connect(&room).await;
    let mut c1 = Peer::connect(&room).await;
    let mut c2 = Peer::connect(&room).await;

    // Mode 3 expressed purely as data: the server spawns (owns) everything.
    // BOTH entities advance in +x: proxy ORDER on clients follows the
    // late-join replay's HashMap iteration (arbitrary per run), so the
    // convergence predicate below (p[0].x advancing) must hold for whichever
    // entity lands first (auditor finding F1 — a zero-x-velocity entity in
    // slot 0 hung the predicate forever on ~1/6 runs).
    spawn_owned(
        &mut server.world,
        server.id,
        Position { x: 0.0, y: 0.0 },
        Velocity { x: 2.0, y: 0.0 },
    );
    spawn_owned(
        &mut server.world,
        server.id,
        Position { x: 10.0, y: 10.0 },
        Velocity { x: 2.0, y: -2.0 },
    );

    let server_id = server.id;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut first_obs: Option<(f32, f32)> = None;
    loop {
        server.pump();
        c1.pump();
        c2.pump();

        let p1 = c1.positions_owned_by(server_id);
        let p2 = c2.positions_owned_by(server_id);
        if p1.len() == 2 && p2.len() == 2 {
            match first_obs {
                None => first_obs = Some((p1[0].x, p2[0].x)),
                // Both clients observed the FIRST entity advancing — continuous
                // replication on both spokes of the star.
                Some((f1, f2)) if p1[0].x > f1 + 1.0 && p2[0].x > f2 + 1.0 => break,
                _ => {}
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "clients never converged/advanced on the server's entities"
        );
        tokio::time::sleep(POLL).await;
    }

    // Mode-3 signature over REAL transport: the clients emitted nothing.
    assert_eq!(c1.state_msgs_sent, 0, "client 1 must never send state");
    assert_eq!(c2.state_msgs_sent, 0, "client 2 must never send state");
    assert_eq!(c1.events_sent, 0, "client 1 must never send events");
    assert_eq!(c2.events_sent, 0, "client 2 must never send events");
    assert!(server.state_msgs_sent > 0, "the server is the sender");

    // Every proxy on both clients is server-owned (no other authority exists).
    for c in [&mut c1, &mut c2] {
        let total = c.world.query::<&Position>().iter(&c.world).count();
        let server_owned = c.positions_owned_by(server_id).len();
        assert_eq!(total, 2);
        assert_eq!(server_owned, 2);
    }
}
