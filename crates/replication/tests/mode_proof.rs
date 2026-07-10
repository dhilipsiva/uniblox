//! M1 ★ THE AUTHORITY-SWAP PROOF — the documented side-by-side run.
//!
//! The thesis this platform rests on: the SAME simulation yields Mode 2 (P2P,
//! each peer authoritative over its own entities) and Mode 3 (authoritative
//! server owns everything) by changing ONLY authority assignment — no logic
//! fork.
//!
//! This file is the executable form of that claim. `run_session` is ONE
//! harness: the same `engine_core::simulate` system, the same
//! `replication::Replication` collect/apply, the same tick loop, the same
//! delivery. The ONLY input that differs between the two tests below is the
//! `spawns` DATA — which participant spawns (and therefore owns) which
//! entities. There is no mode enum, no mode flag, no branch: grep this file
//! (and the whole workspace) for "mode" and you will find only names and
//! comments.
//!
//! Side-by-side (the documented run, per the TODO acceptance):
//! - `mode2_two_peers_each_own_their_entity`: participants [1, 2]; spawns =
//!   [[entity], [entity]] — each owns its own. Both compute their own, apply
//!   the other's, both converge.
//! - `mode3_server_owns_all_clients_apply`: participants [0, 1, 2]; spawns =
//!   [[both entities], [], []] — the server owns ALL. The server computes
//!   everything; the clients emit NOTHING (zero state messages, zero events)
//!   for the entire session and converge to the server's truth.
//!
//! Locked FIRST (TDD). See ADR-0014.

use bevy_ecs::prelude::*;
use engine_core::{Owner, Position, Velocity, insert_sim, simulate, spawn_owned};
use protocol::PeerId;
use replication::Replication;

const DT: f32 = 0.5;
const TOL: f32 = 0.5 / 1024.0;
const ROUNDS: usize = 6;

/// One spawn assignment: initial position + velocity.
type Spawn = (f32, f32, f32, f32);

struct Participant {
    id: PeerId,
    world: World,
    schedule: Schedule,
    repl: Replication,
    /// Emission counters — the Mode-3 "clients send nothing" evidence.
    state_msgs_sent: usize,
    events_sent: usize,
}

/// The session outcome: participants after ROUNDS of identical tick loops.
struct Session {
    participants: Vec<Participant>,
}

/// THE parameterized harness. `ids[i]` is participant i's peer id; `spawns[i]`
/// is the DATA that assigns authority: which entities participant i spawns
/// (and therefore owns, by the default-ownership rule). Everything else —
/// systems, replication, delivery — is identical for every mode.
fn run_session(ids: &[u64], spawns: &[Vec<Spawn>]) -> Session {
    assert_eq!(ids.len(), spawns.len());

    let mut participants: Vec<Participant> = ids
        .iter()
        .map(|&id| {
            let mut world = World::new();
            insert_sim(&mut world, PeerId(id), DT);
            let mut schedule = Schedule::default();
            schedule.add_systems(simulate);
            let repl = Replication::new(&mut world);
            Participant {
                id: PeerId(id),
                world,
                schedule,
                repl,
                state_msgs_sent: 0,
                events_sent: 0,
            }
        })
        .collect();

    for (participant, spawn_list) in participants.iter_mut().zip(spawns) {
        for &(x, y, vx, vy) in spawn_list {
            spawn_owned(
                &mut participant.world,
                participant.id,
                Position { x, y },
                Velocity { x: vx, y: vy },
            );
        }
    }

    // The identical tick loop: everyone simulates, everyone collects,
    // everything is delivered to everyone else (events before state).
    for _ in 0..ROUNDS {
        for p in participants.iter_mut() {
            p.schedule.run(&mut p.world);
        }
        let outboxes: Vec<_> = participants
            .iter_mut()
            .map(|p| {
                let out = p.repl.collect(&mut p.world);
                p.state_msgs_sent += usize::from(out.state.is_some());
                p.events_sent += out.events.len();
                (p.id, out)
            })
            .collect();
        for p in participants.iter_mut() {
            for (from, out) in &outboxes {
                if *from == p.id {
                    continue;
                }
                for ev in &out.events {
                    p.repl.apply_events(&mut p.world, *from, ev);
                }
                if let Some(state) = &out.state {
                    p.repl.apply_state(&mut p.world, *from, state);
                }
            }
        }
    }

    Session { participants }
}

fn positions_owned_by(p: &mut Participant, owner: PeerId) -> Vec<Position> {
    let mut found: Vec<(u32, Position)> = p
        .world
        .query::<(Entity, &Owner, &Position)>()
        .iter(&p.world)
        .filter(|(_, o, _)| o.0 == owner)
        .map(|(e, _, pos)| (e.index_u32(), *pos))
        .collect();
    found.sort_by_key(|(idx, _)| *idx);
    found.into_iter().map(|(_, pos)| pos).collect()
}

fn entity_count(p: &mut Participant) -> usize {
    p.world.query::<&Position>().iter(&p.world).count()
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() <= TOL
}

/// Mode 2: two peers, EACH owns its own entity. Authority assignment: the
/// spawn data gives participant 0 one entity and participant 1 the other.
#[test]
fn mode2_two_peers_each_own_their_entity() {
    let mut s = run_session(
        &[1, 2],
        &[vec![(0.0, 0.0, 2.0, 0.0)], vec![(10.0, 10.0, 0.0, -2.0)]],
    );

    // Both peers hold both entities.
    assert_eq!(entity_count(&mut s.participants[0]), 2);
    assert_eq!(entity_count(&mut s.participants[1]), 2);

    // Each peer's own entity advanced under ITS authority; the proxy on the
    // other side converged to it (within quantization tolerance).
    let (a_id, b_id) = (s.participants[0].id, s.participants[1].id);
    let truth_a = positions_owned_by(&mut s.participants[0], a_id)[0];
    let proxy_a = positions_owned_by(&mut s.participants[1], a_id)[0];
    assert!(truth_a.x > 0.0, "A's entity must have moved under A");
    assert!(approx(proxy_a.x, truth_a.x) && approx(proxy_a.y, truth_a.y));

    let truth_b = positions_owned_by(&mut s.participants[1], b_id)[0];
    let proxy_b = positions_owned_by(&mut s.participants[0], b_id)[0];
    assert!(truth_b.y < 10.0, "B's entity must have moved under B");
    assert!(approx(proxy_b.x, truth_b.x) && approx(proxy_b.y, truth_b.y));

    // Both peers are senders in Mode 2.
    assert!(s.participants[0].state_msgs_sent > 0);
    assert!(s.participants[1].state_msgs_sent > 0);
}

/// Mode 3: the SAME session with ONE data change — participant 0 (the server)
/// spawns ALL entities. The server computes everything; clients apply
/// everything and never send.
#[test]
fn mode3_server_owns_all_clients_apply() {
    let mut s = run_session(
        &[0, 1, 2],
        &[
            vec![(0.0, 0.0, 2.0, 0.0), (10.0, 10.0, 0.0, -2.0)],
            vec![], // client 1 spawns (owns) nothing
            vec![], // client 2 spawns (owns) nothing
        ],
    );
    let server_id = s.participants[0].id;

    // The server computed all entities (they moved under its authority).
    let truths = positions_owned_by(&mut s.participants[0], server_id);
    assert_eq!(truths.len(), 2);
    assert!(truths[0].x > 0.0 && truths[1].y < 10.0);

    // Both clients hold exactly the server's entities, all Owner == server,
    // converged to the server's truth.
    for client in &mut s.participants[1..] {
        assert_eq!(entity_count(client), 2, "client must hold both proxies");
        let proxies = positions_owned_by(client, server_id);
        assert_eq!(proxies.len(), 2, "every proxy must be server-owned");
        for (proxy, truth) in proxies.iter().zip(&truths) {
            assert!(approx(proxy.x, truth.x) && approx(proxy.y, truth.y));
        }
    }

    // THE Mode-3 signature: clients emitted NOTHING for the whole session.
    for client in &s.participants[1..] {
        assert_eq!(client.state_msgs_sent, 0, "clients must never send state");
        assert_eq!(client.events_sent, 0, "clients must never send events");
    }
    assert!(
        s.participants[0].state_msgs_sent > 0,
        "the server is the sender"
    );
}
