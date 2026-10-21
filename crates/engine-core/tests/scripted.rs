//! TDD battery for script-driven entity logic (ADR-0045). `run_entity_scripts` runs the content's
//! `update(agent)` for every `Scripted` entity this peer OWNS (`authority_of == Local`) and writes
//! back only the velocity; `simulate` still integrates position. Receivers never run another peer's
//! script (no re-simulation, no cross-platform float determinism). Written before the
//! implementation.

use bevy_ecs::prelude::*;
use engine_core::{
    Contacts, Controlled, ControlledBy, MAX_SCRIPT_SPEED, Owner, Position, Scripted, Unscripted,
    Velocity, enable_scripts, insert_sim, run_entity_scripts, simulate, spawn_owned,
};
use protocol::PeerId;
use scripting::ScriptEngine;

const LOCAL: PeerId = PeerId(1);
const OTHER: PeerId = PeerId(2);
const SET_VX: &str = "fn update(a) { a.vx = 5.0; a }";

fn world_with(engine: Option<ScriptEngine>) -> World {
    let mut world = World::new();
    insert_sim(&mut world, LOCAL, 0.5);
    if let Some(engine) = engine {
        scripting::insert_scripting(&mut world, engine);
    }
    world
}

fn spawn(world: &mut World, owner: PeerId, scripted: bool) -> Entity {
    let e = spawn_owned(
        world,
        owner,
        Position { x: 0.0, y: 0.0 },
        Velocity { x: 1.0, y: 0.0 },
    );
    if scripted {
        world.entity_mut(e).insert(Scripted);
    }
    e
}

fn run(world: &mut World) {
    let mut schedule = Schedule::default();
    schedule.add_systems(run_entity_scripts);
    schedule.run(world);
}

#[test]
fn scripts_run_only_for_owned_scripted_entities() {
    let mut world = world_with(Some(ScriptEngine::from_source(SET_VX).unwrap()));
    let owned_scripted = spawn(&mut world, LOCAL, true);
    let owned_plain = spawn(&mut world, LOCAL, false);
    let remote_scripted = spawn(&mut world, OTHER, true);
    run(&mut world);
    assert_eq!(world.get::<Velocity>(owned_scripted).unwrap().x, 5.0);
    assert_eq!(
        world.get::<Velocity>(owned_plain).unwrap().x,
        1.0,
        "not Scripted"
    );
    assert_eq!(
        world.get::<Velocity>(remote_scripted).unwrap().x,
        1.0,
        "a receiver never runs another peer's script"
    );
}

#[test]
fn without_a_script_engine_the_system_is_a_no_op() {
    let mut world = world_with(None);
    let e = spawn(&mut world, LOCAL, true);
    run(&mut world);
    assert_eq!(world.get::<Velocity>(e).unwrap().x, 1.0);
}

#[test]
fn the_script_steers_and_simulate_integrates() {
    let mut world = world_with(Some(ScriptEngine::from_source(SET_VX).unwrap()));
    let e = spawn(&mut world, LOCAL, true);
    let mut schedule = Schedule::default();
    schedule.add_systems((run_entity_scripts, simulate).chain());
    schedule.run(&mut world);
    assert_eq!(world.get::<Position>(e).unwrap().x, 2.5, "5.0 * dt 0.5");
}

#[test]
fn an_unchanged_velocity_is_not_marked_changed() {
    let mut world = world_with(Some(
        ScriptEngine::from_source("fn update(a) { a }").unwrap(),
    ));
    let e = spawn(&mut world, LOCAL, true);
    world.clear_trackers();
    run(&mut world);
    assert!(
        !world.entity(e).get_ref::<Velocity>().unwrap().is_changed(),
        "a script that changes nothing must not trigger replication"
    );
}

#[test]
fn contacts_reach_the_script() {
    let engine =
        ScriptEngine::from_source("fn update(a) { if a.contacts == 3 { a.vy = 1.0; } a }").unwrap();
    let mut world = world_with(Some(engine));
    let e = spawn(&mut world, LOCAL, true);
    world.entity_mut(e).insert(Contacts(3));
    run(&mut world);
    assert_eq!(world.get::<Velocity>(e).unwrap().y, 1.0);
}

#[test]
fn a_failing_script_leaves_the_entity_unchanged() {
    let engine = ScriptEngine::from_source("fn update(a) { a.x = 9.0; a }").unwrap();
    let mut world = world_with(Some(engine));
    let e = spawn(&mut world, LOCAL, true);
    run(&mut world);
    assert_eq!(
        *world.get::<Velocity>(e).unwrap(),
        Velocity { x: 1.0, y: 0.0 }
    );
    assert_eq!(
        *world.get::<Position>(e).unwrap(),
        Position { x: 0.0, y: 0.0 }
    );
}

#[test]
fn enable_scripts_marks_the_owned_entities_and_inserts_the_engine() {
    let mut world = world_with(None);
    let owned = spawn(&mut world, LOCAL, false);
    let remote = spawn(&mut world, OTHER, false);
    enable_scripts(&mut world, ScriptEngine::from_source(SET_VX).unwrap());
    assert!(world.get::<Scripted>(owned).is_some());
    assert!(
        world.get::<Scripted>(remote).is_none(),
        "only entities this peer owns"
    );
    run(&mut world);
    assert_eq!(world.get::<Velocity>(owned).unwrap().x, 5.0);
}

// ── Audit follow-ups (2026-10-21): handoff marking, the speed clamp, player control ──

/// A finite but huge velocity is clamped, so position stays finite and replication never sees inf.
#[test]
fn a_huge_velocity_is_clamped_to_the_maximum_speed() {
    let engine = ScriptEngine::from_source("fn update(a) { a.vx = 3.0e38; a.vy = -3.0e38; a }").unwrap();
    let mut world = world_with(Some(engine));
    let e = spawn(&mut world, LOCAL, true);
    let mut schedule = Schedule::default();
    schedule.add_systems((run_entity_scripts, simulate).chain());
    for _ in 0..100 {
        schedule.run(&mut world);
    }
    let vel = *world.get::<Velocity>(e).unwrap();
    assert_eq!(vel, Velocity { x: MAX_SCRIPT_SPEED, y: -MAX_SCRIPT_SPEED });
    assert!(world.get::<Position>(e).unwrap().x.is_finite());
}

/// Entities that appear after content loads (a replica, a handed-off or migrated entity, a reloaded
/// save) are marked too, so whichever peer owns one later runs its script; a receiver still skips it.
#[test]
fn entities_appearing_after_load_are_scripted_on_whichever_peer_owns_them() {
    let mut world = world_with(None);
    enable_scripts(&mut world, ScriptEngine::from_source(SET_VX).unwrap());
    let later_owned = spawn(&mut world, LOCAL, false);
    let later_proxy = spawn(&mut world, OTHER, false);
    run(&mut world);
    run(&mut world);
    assert!(world.get::<Scripted>(later_proxy).is_some(), "proxies are marked");
    assert_eq!(world.get::<Velocity>(later_proxy).unwrap().x, 1.0, "but not run");
    assert_eq!(world.get::<Velocity>(later_owned).unwrap().x, 5.0);
    // Ownership arrives at this peer (a handoff or host migration): now it runs here.
    world.entity_mut(later_proxy).insert(Owner(LOCAL));
    run(&mut world);
    assert_eq!(world.get::<Velocity>(later_proxy).unwrap().x, 5.0);
}

/// A player-controlled entity is never scripted: the player's input owns its velocity.
#[test]
fn player_controlled_and_opted_out_entities_are_never_scripted() {
    let mut world = world_with(None);
    let by = spawn(&mut world, LOCAL, false);
    world.entity_mut(by).insert(ControlledBy(OTHER));
    let here = spawn(&mut world, LOCAL, false);
    world.entity_mut(here).insert(Controlled { next_seq: 0 });
    let opted_out = spawn(&mut world, LOCAL, false);
    world.entity_mut(opted_out).insert(Unscripted);
    let forced = spawn(&mut world, LOCAL, true);
    world.entity_mut(forced).insert(ControlledBy(OTHER));
    enable_scripts(&mut world, ScriptEngine::from_source(SET_VX).unwrap());
    run(&mut world);
    run(&mut world);
    for e in [by, here, opted_out, forced] {
        assert_eq!(world.get::<Velocity>(e).unwrap().x, 1.0, "{e:?} was scripted");
    }
}
