//! TDD battery for the mode-agnostic mini-game simulation.
//!
//! Written FIRST and locked by the `tests/`-guard hook. The core property under
//! test is the authority-swap thesis: the SAME `simulate` system yields Mode 1
//! (Standalone), Mode 2 (P2P), and Mode 3 (Server) by changing ONLY the data
//! inputs (`Owner` values + `LocalPeer`) — never the gameplay code.
//!
//! Floats: use exactly-representable constants (dt=0.5, vel=±2.0 → ±1.0/tick)
//! and assert exact equality — no epsilon fragility.
//!
//! Counting: always component-filtered queries, never bare `Query<Entity>`
//! (bevy_ecs 0.19 stores resources on entities; a bare query would count them).

use bevy_ecs::prelude::*;
use engine_core::{
    Authority, Owner, Position, Velocity, authority_of, insert_sim, simulate, spawn_owned,
};
use protocol::PeerId;

const DT: f32 = 0.5;

fn pos(x: f32, y: f32) -> Position {
    Position { x, y }
}

fn vel(x: f32, y: f32) -> Velocity {
    Velocity { x, y }
}

/// Build a world + schedule running `simulate`, with `local` as the local peer.
fn sim_world(local: PeerId) -> (World, Schedule) {
    let mut world = World::new();
    insert_sim(&mut world, local, DT);
    let mut schedule = Schedule::default();
    schedule.add_systems(simulate);
    (world, schedule)
}

// ── Acceptance ──────────────────────────────────────────────────────────────

/// AC-A: Mode 1 (Standalone) — the local peer owns its entities; they advance
/// by vel*dt per tick under local authority.
#[test]
fn mode1_local_authority_advances() {
    let p = PeerId(1);
    let (mut world, mut schedule) = sim_world(p);
    let e = spawn_owned(&mut world, p, pos(0.0, 0.0), vel(2.0, -2.0));

    for _ in 0..4 {
        schedule.run(&mut world);
    }

    let got = *world.get::<Position>(e).unwrap();
    assert_eq!(got, pos(4.0 * 2.0 * DT, 4.0 * -2.0 * DT)); // (4.0, -4.0)
}

/// AC-A: a spawned entity is owned by its spawner by default.
#[test]
fn default_ownership_is_spawner() {
    let spawner = PeerId(42);
    let (mut world, _) = sim_world(PeerId(1));
    let e = spawn_owned(&mut world, spawner, pos(0.0, 0.0), vel(0.0, 0.0));

    assert_eq!(*world.get::<Owner>(e).unwrap(), Owner(spawner));
}

/// AC-B: the authority gate — an entity owned by a DIFFERENT peer is NOT
/// computed locally (Remote → apply path, a no-op until replication lands).
#[test]
fn remote_entity_not_computed_locally() {
    let (mut world, mut schedule) = sim_world(PeerId(1));
    let e = spawn_owned(&mut world, PeerId(2), pos(3.0, 3.0), vel(2.0, 2.0));

    for _ in 0..4 {
        schedule.run(&mut world);
    }

    assert_eq!(*world.get::<Position>(e).unwrap(), pos(3.0, 3.0));
}

/// AC-B: the single decision point, as a pure function — Local iff owner == local.
#[test]
fn authority_of_is_local_iff_owner_matches() {
    assert_eq!(authority_of(PeerId(1), PeerId(1)), Authority::Local);
    assert_eq!(authority_of(PeerId(1), PeerId(2)), Authority::Remote);
    assert_eq!(authority_of(PeerId(2), PeerId(1)), Authority::Remote);
}

// ── The thesis, at the unit level ───────────────────────────────────────────

/// One Local + one Remote entity in a single pass: only the Local one moves.
/// (Per-entity gating within one system run — the Mode-2 crux.)
#[test]
fn mixed_pass_gates_per_entity() {
    let (mut world, mut schedule) = sim_world(PeerId(1));
    let mine = spawn_owned(&mut world, PeerId(1), pos(0.0, 0.0), vel(2.0, 0.0));
    let theirs = spawn_owned(&mut world, PeerId(2), pos(0.0, 0.0), vel(2.0, 0.0));

    schedule.run(&mut world);

    assert_eq!(*world.get::<Position>(mine).unwrap(), pos(1.0, 0.0));
    assert_eq!(*world.get::<Position>(theirs).unwrap(), pos(0.0, 0.0));
}

/// ★ Mode 2 (P2P) two-perspective proof: the same entity layout run from peer
/// A's perspective moves only A's entities; from B's perspective, only B's.
/// The ONLY difference between the two runs is the `LocalPeer` value — pure data.
#[test]
fn mode2_two_perspectives_differ_only_in_local_peer() {
    let (a, b) = (PeerId(1), PeerId(2));

    let run_as = |local: PeerId| -> (Position, Position) {
        let (mut world, mut schedule) = sim_world(local);
        let ent_a = spawn_owned(&mut world, a, pos(0.0, 0.0), vel(2.0, 0.0));
        let ent_b = spawn_owned(&mut world, b, pos(0.0, 0.0), vel(0.0, 2.0));
        schedule.run(&mut world);
        (
            *world.get::<Position>(ent_a).unwrap(),
            *world.get::<Position>(ent_b).unwrap(),
        )
    };

    // Peer A's instance: A's entity computes, B's stays (awaiting replication).
    assert_eq!(run_as(a), (pos(1.0, 0.0), pos(0.0, 0.0)));
    // Peer B's instance: the mirror.
    assert_eq!(run_as(b), (pos(0.0, 0.0), pos(0.0, 1.0)));
}

/// ★ Mode 3 (Server) shape proof: server owns ALL entities. On the server
/// instance everything computes; on a client instance nothing computes locally
/// (clients apply replicated state). Same code, only data differs.
#[test]
fn mode3_server_computes_all_client_computes_none() {
    let (server, client) = (PeerId(0), PeerId(9));

    let run_as = |local: PeerId| -> (Position, Position) {
        let (mut world, mut schedule) = sim_world(local);
        let e1 = spawn_owned(&mut world, server, pos(0.0, 0.0), vel(2.0, 0.0));
        let e2 = spawn_owned(&mut world, server, pos(0.0, 0.0), vel(0.0, 2.0));
        schedule.run(&mut world);
        (
            *world.get::<Position>(e1).unwrap(),
            *world.get::<Position>(e2).unwrap(),
        )
    };

    assert_eq!(run_as(server), (pos(1.0, 0.0), pos(0.0, 1.0)));
    assert_eq!(run_as(client), (pos(0.0, 0.0), pos(0.0, 0.0)));
}

// ── Hygiene ─────────────────────────────────────────────────────────────────

/// Orphan audit: `spawn_owned` is the sole construction path, so no sim entity
/// (anything with a Position) exists without an Owner — an ownerless entity
/// would be silently skipped by simulation AND replication.
#[test]
fn no_position_without_owner() {
    let (mut world, _) = sim_world(PeerId(1));
    spawn_owned(&mut world, PeerId(1), pos(0.0, 0.0), vel(1.0, 1.0));
    spawn_owned(&mut world, PeerId(2), pos(1.0, 1.0), vel(0.0, 0.0));

    let orphans = world
        .query_filtered::<Entity, (With<Position>, Without<Owner>)>()
        .iter(&world)
        .count();
    assert_eq!(orphans, 0);
}
