//! The sample Rhai game runs unchanged in all three modes (ADR-0045; TODO Phases 5-6 "a sample game
//! that runs unchanged in Modes 1, 2 and 3"). Written before the implementation.
//!
//! The same bundle (`content/sample`) and the same systems run everywhere; only authority data
//! differs, as in `replication/tests/mode_proof.rs`:
//! - Mode 1: the real `standalone` app, one peer owning all three entities, stepped exactly one fixed
//!   tick per `App::update`.
//! - Mode 2: two peers, each owning some entities, replicating to each other.
//! - Mode 3: a server owning all three entities, two clients that only apply.
//!
//! Each entity's authoritative trajectory must be identical in all three modes (the owner runs the
//! same script on the same inputs), every replica must converge to its owner, and the arena script
//! must actually steer (without it the entities leave the arena).

use std::path::Path;

use bevy_ecs::prelude::*;
use bevy_time::TimeUpdateStrategy;
use engine_core::{
    Owner, Position, Tick, Velocity, advance_tick, enable_scripts, insert_sim, run_entity_scripts,
    simulate, spawn_owned,
};
use protocol::PeerId;
use protocol::bundle::load_bundle_dir;
use replication::Replication;
use scripting::ScriptEngine;

/// A deterministic per-peer event-signing key for tests (ADR-0049).
fn test_key(peer: PeerId) -> replication::SigningKey {
    let mut seed = [0x5A; 32];
    seed[..8].copy_from_slice(&peer.0.to_le_bytes());
    replication::SigningKey::from_bytes(&seed)
}

/// The standalone app's fixed rate is 64 Hz.
const DT: f32 = 1.0 / 64.0;
/// About 23 s of simulated time: every demo entity reaches a wall several times.
const TICKS: u64 = 1500;
/// Quantized replication tolerance (protocol: QUANT_SCALE = 1024).
const TOL: f32 = 1.0 / 1024.0;
/// The arena's half extents in the sample script, plus one tick of travel at the demo speeds.
const ARENA: (f32, f32) = (10.0 + 2.0 * DT, 6.0 + 1.0 * DT);

/// The standalone demo scene for 3 entities: entity i starts at (0, 2i) with velocity (2, 0.5i).
fn demo_spawns() -> Vec<(Position, Velocity)> {
    (0..3)
        .map(|i| {
            let i = i as f32;
            (
                Position { x: 0.0, y: 2.0 * i },
                Velocity { x: 2.0, y: 0.5 * i },
            )
        })
        .collect()
}

fn sample_engine() -> ScriptEngine {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/sample/manifest.json");
    let bundle = load_bundle_dir(&path).expect("the sample bundle loads");
    ScriptEngine::from_source(&bundle.entry_source).expect("the sample script compiles")
}

/// Mode 1: the real standalone app. Returns each demo entity's final position, in spawn order.
fn mode1() -> Vec<Position> {
    let local = PeerId(1);
    let mut app = standalone::build_standalone_app(local, 3);
    app.insert_resource(TimeUpdateStrategy::FixedTimesteps(1));
    let order: Vec<Entity> = {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &Position)>();
        let mut v: Vec<(Entity, f32)> = q.iter(world).map(|(e, p)| (e, p.y)).collect();
        v.sort_by(|a, b| a.1.total_cmp(&b.1));
        v.into_iter().map(|(e, _)| e).collect()
    };
    enable_scripts(app.world_mut(), sample_engine());
    let mut guard = 0;
    while app.world().resource::<Tick>().0 < TICKS {
        app.update();
        guard += 1;
        assert!(guard < TICKS * 4, "the fixed loop is not advancing");
    }
    assert_eq!(app.world().resource::<Tick>().0, TICKS);
    order
        .iter()
        .map(|&e| *app.world().get::<Position>(e).unwrap())
        .collect()
}

struct Participant {
    id: PeerId,
    world: World,
    schedule: Schedule,
    repl: Replication,
}

/// Modes 2 and 3: the mode_proof harness with the script system added. `owners[i]` is the peer that
/// spawns (and owns) demo entity i. Returns the participants after TICKS identical rounds, and for
/// each demo entity, in spawn order, its owner's index in the participant list and its entity there.
fn networked(ids: &[u64], owners: &[u64]) -> (Vec<Participant>, Vec<(usize, Entity)>) {
    let engine_src = {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/sample/manifest.json");
        load_bundle_dir(&path).unwrap().entry_source
    };
    let mut ps: Vec<Participant> = ids
        .iter()
        .map(|&id| {
            let mut world = World::new();
            insert_sim(&mut world, PeerId(id), DT);
            let mut schedule = Schedule::default();
            schedule.add_systems((advance_tick, run_entity_scripts, simulate).chain());
            let repl = Replication::new(&mut world, test_key(PeerId(id)));
            Participant {
                id: PeerId(id),
                world,
                schedule,
                repl,
            }
        })
        .collect();
    let mut spawned = Vec::new();
    for ((pos, vel), &owner) in demo_spawns().into_iter().zip(owners) {
        let i = ps.iter().position(|p| p.id == PeerId(owner)).unwrap();
        let p = &mut ps[i];
        spawned.push((i, spawn_owned(&mut p.world, p.id, pos, vel)));
    }
    for p in ps.iter_mut() {
        enable_scripts(
            &mut p.world,
            ScriptEngine::from_source(&engine_src).unwrap(),
        );
    }
    let all: Vec<PeerId> = ps.iter().map(|p| p.id).collect();
    for p in ps.iter_mut() {
        for &other in &all {
            if other != p.id {
                p.repl.track_peer(other);
            }
        }
    }
    for _ in 0..TICKS {
        for p in ps.iter_mut() {
            p.schedule.run(&mut p.world);
        }
        let outboxes: Vec<_> = ps
            .iter_mut()
            .map(|p| (p.id, p.repl.collect_all(&mut p.world)))
            .collect();
        for p in ps.iter_mut() {
            for (from, outs) in &outboxes {
                if *from == p.id {
                    continue;
                }
                let Some(out) = outs.iter().find(|(t, _)| *t == p.id).map(|(_, o)| o) else {
                    continue;
                };
                for ev in &out.events {
                    p.repl.apply_events(&mut p.world, *from, ev);
                }
                if let Some(state) = &out.state {
                    p.repl.apply_state(&mut p.world, *from, state);
                }
            }
        }
    }
    (ps, spawned)
}

/// Each demo entity's authoritative position, read on its owner, in spawn order.
fn truth(ps: &[Participant], spawned: &[(usize, Entity)]) -> Vec<Position> {
    spawned
        .iter()
        .map(|&(i, e)| *ps[i].world.get::<Position>(e).unwrap())
        .collect()
}

/// The positions of the entities a participant owns (any order).
fn owned_positions(p: &mut Participant) -> Vec<Position> {
    let mut q = p.world.query::<(Entity, &Owner, &Position)>();
    let mut v: Vec<(Entity, Position)> = q
        .iter(&p.world)
        .filter(|(_, o, _)| o.0 == p.id)
        .map(|(e, _, pos)| (e, *pos))
        .collect();
    v.sort_by_key(|(e, _)| *e);
    v.into_iter().map(|(_, pos)| pos).collect()
}

fn all_positions(p: &mut Participant) -> Vec<Position> {
    let mut q = p.world.query::<&Position>();
    q.iter(&p.world).copied().collect()
}

fn near(a: Position, b: Position) -> bool {
    (a.x - b.x).abs() <= TOL && (a.y - b.y).abs() <= TOL
}

fn assert_in_arena(label: &str, positions: &[Position]) {
    for p in positions {
        assert!(
            p.x.abs() <= ARENA.0 && p.y.abs() <= ARENA.1,
            "{label}: {p:?} left the arena"
        );
    }
}

/// Every replica position matches some authoritative position.
fn assert_converged(label: &str, replicas: &[Position], truth: &[Position]) {
    assert_eq!(replicas.len(), truth.len(), "{label}: entity count");
    for r in replicas {
        assert!(
            truth.iter().any(|t| near(*r, *t)),
            "{label}: replica {r:?} is not within {TOL} of any owner position {truth:?}"
        );
    }
}

#[test]
fn the_sample_game_runs_unchanged_in_all_three_modes() {
    let m1 = mode1();
    assert_in_arena("mode 1", &m1);

    // Mode 2: peer 1 owns entities 0 and 1, peer 2 owns entity 2.
    let (mut m2, spawned) = networked(&[1, 2], &[1, 1, 2]);
    let m2_truth = truth(&m2, &spawned);
    assert_eq!(
        m2_truth, m1,
        "mode 2 owners compute exactly what mode 1 computes"
    );
    for p in m2.iter_mut() {
        let replicas = all_positions(p);
        assert_converged("mode 2", &replicas, &m2_truth);
    }

    // Mode 3: server 0 owns all three; clients 1 and 2 only apply.
    let (mut m3, spawned) = networked(&[0, 1, 2], &[0, 0, 0]);
    let m3_truth = truth(&m3, &spawned);
    assert_eq!(
        m3_truth, m1,
        "the mode 3 server computes exactly what mode 1 computes"
    );
    for p in m3.iter_mut().skip(1) {
        assert!(owned_positions(p).is_empty(), "mode 3 clients own nothing");
        let replicas = all_positions(p);
        assert_converged("mode 3", &replicas, &m3_truth);
    }
}

/// Control: without the script the same scene leaves the arena, so the test above proves the script
/// steers.
#[test]
fn without_the_script_the_entities_leave_the_arena() {
    let mut world = World::new();
    insert_sim(&mut world, PeerId(1), DT);
    for (pos, vel) in demo_spawns() {
        spawn_owned(&mut world, PeerId(1), pos, vel);
    }
    let mut schedule = Schedule::default();
    schedule.add_systems((advance_tick, simulate).chain());
    for _ in 0..TICKS {
        schedule.run(&mut world);
    }
    let mut q = world.query::<&Position>();
    assert!(q.iter(&world).any(|p| p.x.abs() > ARENA.0));
}

// ── Audit follow-ups (2026-10-21): ownership moves, receivers, load_content ──

/// The mode_proof harness as a stepper, so a test can move ownership mid-run.
fn peers(ids: &[u64], owners: &[u64], src: &str) -> (Vec<Participant>, Vec<(usize, Entity)>) {
    let mut ps: Vec<Participant> = ids
        .iter()
        .map(|&id| {
            let mut world = World::new();
            insert_sim(&mut world, PeerId(id), DT);
            let mut schedule = Schedule::default();
            schedule.add_systems((advance_tick, run_entity_scripts, simulate).chain());
            let repl = Replication::new(&mut world, test_key(PeerId(id)));
            Participant {
                id: PeerId(id),
                world,
                schedule,
                repl,
            }
        })
        .collect();
    let mut spawned = Vec::new();
    for ((pos, vel), &owner) in demo_spawns().into_iter().zip(owners) {
        let i = ps.iter().position(|p| p.id == PeerId(owner)).unwrap();
        let p = &mut ps[i];
        spawned.push((i, spawn_owned(&mut p.world, p.id, pos, vel)));
    }
    for p in ps.iter_mut() {
        enable_scripts(&mut p.world, ScriptEngine::from_source(src).unwrap());
    }
    let all: Vec<PeerId> = ps.iter().map(|p| p.id).collect();
    for p in ps.iter_mut() {
        for &other in &all {
            if other != p.id {
                p.repl.track_peer(other);
            }
        }
    }
    (ps, spawned)
}

/// One round over the participants that are still `live`.
fn round(ps: &mut [Participant], live: &[bool]) {
    for (p, &on) in ps.iter_mut().zip(live) {
        if on {
            p.schedule.run(&mut p.world);
        }
    }
    let outboxes: Vec<_> = ps
        .iter_mut()
        .zip(live)
        .filter(|(_, on)| **on)
        .map(|(p, _)| (p.id, p.repl.collect_all(&mut p.world)))
        .collect();
    for (p, &on) in ps.iter_mut().zip(live) {
        if !on {
            continue;
        }
        for (from, outs) in &outboxes {
            if *from == p.id {
                continue;
            }
            let Some(out) = outs.iter().find(|(t, _)| *t == p.id).map(|(_, o)| o) else {
                continue;
            };
            for ev in &out.events {
                p.repl.apply_events(&mut p.world, *from, ev);
            }
            if let Some(state) = &out.state {
                p.repl.apply_state(&mut p.world, *from, state);
            }
        }
    }
}

fn sample_src() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/sample/manifest.json");
    load_bundle_dir(&path).unwrap().entry_source
}

/// Every entity a participant owns is inside the arena, and it owns `expected` of them.
fn assert_owner_keeps_them_in_arena(label: &str, p: &mut Participant, expected: usize) {
    let owned = owned_positions(p);
    assert_eq!(owned.len(), expected, "{label}: owned entity count");
    assert_in_arena(label, &owned);
}

/// Mode 2: peer 1 (the coordinator) hands an entity to peer 2 mid-game; peer 2 runs its script from
/// then on, so it stays in the arena.
#[test]
fn a_handed_off_entity_keeps_running_its_script() {
    let (mut ps, spawned) = peers(&[1, 2], &[1, 1, 1], &sample_src());
    let live = [true, true];
    for _ in 0..200 {
        round(&mut ps, &live);
    }
    let (owner, e) = spawned[0];
    let p = &mut ps[owner];
    p.repl
        .transfer_ownership(&mut p.world, e, PeerId(2))
        .expect("peer 1 owns it");
    for _ in 200..TICKS {
        round(&mut ps, &live);
    }
    assert_owner_keeps_them_in_arena("peer 2 after the handoff", &mut ps[1], 1);
    assert_owner_keeps_them_in_arena("peer 1 after the handoff", &mut ps[0], 2);
}

/// Mode 2: peer 1 leaves; peer 2 inherits its entities by host migration and runs their script.
#[test]
fn a_migrated_entity_keeps_running_its_script() {
    let (mut ps, _) = peers(&[1, 2], &[1, 1, 2], &sample_src());
    for _ in 0..200 {
        round(&mut ps, &[true, true]);
    }
    let survivor = &mut ps[1];
    survivor.repl.untrack_peer(PeerId(1));
    let moved = survivor
        .repl
        .reassign_orphans(&mut survivor.world, PeerId(1));
    assert_eq!(moved.len(), 2, "peer 2 inherits peer 1's two entities");
    for _ in 200..TICKS {
        round(&mut ps, &[false, true]);
    }
    assert_owner_keeps_them_in_arena("peer 2 after migration", &mut ps[1], 3);
}

/// Receivers carry the marker on their replicas but never run another peer's script: a probe
/// script that sets an impossible velocity never shows up on a Mode-3 client.
#[test]
fn receivers_never_run_scripts_on_replicas() {
    let (mut ps, _) = peers(&[0, 1, 2], &[0, 0, 0], "fn update(a) { a.vx = 123.0; a }");
    for _ in 0..20 {
        round(&mut ps, &[true, true, true]);
    }
    for p in ps.iter_mut().skip(1) {
        let mut q = p.world.query::<(&engine_core::Scripted, &Velocity)>();
        assert_eq!(q.iter(&p.world).count(), 3, "client replicas are marked");
        // Run the client's sim once more WITHOUT applying the server's state: a receiver that ran
        // scripts would now show the probe velocity on its replicas.
        p.schedule.run(&mut p.world);
        let mut q = p.world.query::<(&Owner, &Velocity)>();
        for (owner, vel) in q.iter(&p.world) {
            assert_eq!(owner.0, PeerId(0));
            assert_eq!(vel.x, 123.0, "the server's scripted velocity replicates");
        }
    }
    // The control that makes the check above meaningful: with no state applied, a client's replica
    // velocity changes only if the client itself ran the script. Prove that by giving one replica a
    // different velocity and running the client again.
    let client = &mut ps[1];
    let mut q = client.world.query::<(Entity, &Velocity)>();
    let (e, _) = q.iter(&client.world).next().unwrap();
    client.world.get_mut::<Velocity>(e).unwrap().x = -7.0;
    client.schedule.run(&mut client.world);
    assert_eq!(
        client.world.get::<Velocity>(e).unwrap().x,
        -7.0,
        "the client did not run it"
    );
}

/// `standalone::load_content` loads the sample bundle and refuses a missing one.
#[test]
fn standalone_load_content_loads_the_sample_and_refuses_a_missing_bundle() {
    let mut app = standalone::build_standalone_app(PeerId(1), 3);
    let good = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/sample/manifest.json");
    standalone::load_content(&mut app, &good).expect("the sample bundle loads");
    let world = app.world_mut();
    assert_eq!(
        world.query::<&engine_core::Scripted>().iter(world).count(),
        3
    );
    let mut app = standalone::build_standalone_app(PeerId(1), 3);
    assert!(standalone::load_content(&mut app, Path::new("/nonexistent/manifest.json")).is_err());
}
