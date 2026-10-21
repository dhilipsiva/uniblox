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
            (Position { x: 0.0, y: 2.0 * i }, Velocity { x: 2.0, y: 0.5 * i })
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
/// spawns (and owns) demo entity i. Returns the participants after TICKS identical rounds.
fn networked(ids: &[u64], owners: &[u64]) -> Vec<Participant> {
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
            let repl = Replication::new(&mut world);
            Participant {
                id: PeerId(id),
                world,
                schedule,
                repl,
            }
        })
        .collect();
    for ((pos, vel), &owner) in demo_spawns().into_iter().zip(owners) {
        let p = ps.iter_mut().find(|p| p.id == PeerId(owner)).unwrap();
        spawn_owned(&mut p.world, p.id, pos, vel);
    }
    for p in ps.iter_mut() {
        enable_scripts(&mut p.world, ScriptEngine::from_source(&engine_src).unwrap());
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
    ps
}

/// An owner's entities in spawn order (the order its demo entities were spawned on it).
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
    let mut m2 = networked(&[1, 2], &[1, 1, 2]);
    let m2_truth: Vec<Position> = {
        let mut a = owned_positions(&mut m2[0]);
        a.extend(owned_positions(&mut m2[1]));
        a
    };
    assert_eq!(m2_truth, m1, "mode 2 owners compute exactly what mode 1 computes");
    for p in m2.iter_mut() {
        let replicas = all_positions(p);
        assert_converged("mode 2", &replicas, &m2_truth);
    }

    // Mode 3: server 0 owns all three; clients 1 and 2 only apply.
    let mut m3 = networked(&[0, 1, 2], &[0, 0, 0]);
    let m3_truth = owned_positions(&mut m3[0]);
    assert_eq!(m3_truth, m1, "the mode 3 server computes exactly what mode 1 computes");
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
