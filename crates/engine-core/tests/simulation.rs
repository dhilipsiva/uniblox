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
    Authority, Contacts, Interactable, Owner, Position, Velocity, authority_of, insert_sim,
    interaction_decider, overlaps, resolve_interactions, simulate, spawn_owned,
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

// ── Cross-owner interactions (ADR-0027) ──────────────────────────────────────
//
// Rule R1: each contact effect is applied by the OWNER of the affected entity
// (`authority_of == Local`) — "the entity being hit is authoritative", straight
// out of single-ownership (only the owner may write it). The other entity is only
// READ (its replicated Position), never re-simulated. Coarse = circle overlap.

/// A world + schedule running ONLY `resolve_interactions`.
fn interaction_world(local: PeerId) -> (World, Schedule) {
    let mut world = World::new();
    insert_sim(&mut world, local, DT);
    let mut schedule = Schedule::default();
    schedule.add_systems(resolve_interactions);
    (world, schedule)
}

/// Spawn a stationary interactable entity (circle of `radius`) owned by `owner`.
fn spawn_interactable(world: &mut World, owner: PeerId, p: Position, radius: f32) -> Entity {
    let e = spawn_owned(world, owner, p, vel(0.0, 0.0));
    world
        .entity_mut(e)
        .insert((Interactable { radius }, Contacts::default()));
    e
}

/// INT-geom: `overlaps` is a coarse circle test — overlapping and exactly-touching
/// count; strictly-apart does not.
#[test]
fn overlaps_is_a_coarse_circle_test() {
    // Centers 1.0 apart, radii 0.6+0.6=1.2 > 1.0 → overlap.
    assert!(overlaps(pos(0.0, 0.0), 0.6, pos(1.0, 0.0), 0.6));
    // Centers exactly 2.0 apart, radii 1.0+1.0=2.0 → touching counts (≤).
    assert!(overlaps(pos(0.0, 0.0), 1.0, pos(2.0, 0.0), 1.0));
    // Centers 3.0 apart, radii 1.0+1.0=2.0 < 3.0 → apart.
    assert!(!overlaps(pos(0.0, 0.0), 1.0, pos(3.0, 0.0), 1.0));
}

/// INT-decider: the shared-outcome tiebreak is the LOWER owner PeerId — symmetric,
/// and picks EXACTLY ONE of the two peers (so a shared result is recorded once, no
/// double-count).
#[test]
fn interaction_decider_is_the_lower_peer_and_exactly_one() {
    assert_eq!(interaction_decider(PeerId(2), PeerId(5)), PeerId(2));
    assert_eq!(interaction_decider(PeerId(5), PeerId(2)), PeerId(2)); // symmetric
    // Exactly one of the two owners is the decider.
    let (p, q) = (PeerId(3), PeerId(8));
    let p_decides = interaction_decider(p, q) == p;
    let q_decides = interaction_decider(p, q) == q;
    assert!(
        p_decides ^ q_decides,
        "exactly one peer decides the shared outcome"
    );
    assert!(p_decides, "the lower owner PeerId decides");
}

/// INT-local: a contact accrues ONLY on an entity the local peer OWNS. My entity
/// overlapping a REMOTE entity gets its contact (I own it); the remote entity is
/// NOT applied by me (its owner does that); a non-overlapping entity gets nothing.
#[test]
fn contact_accrues_only_on_the_local_owner() {
    let me = PeerId(1);
    let (mut world, mut schedule) = interaction_world(me);
    let a = spawn_interactable(&mut world, me, pos(0.0, 0.0), 1.0);
    let r = spawn_interactable(&mut world, PeerId(2), pos(1.0, 0.0), 1.0); // overlaps A
    let far = spawn_interactable(&mut world, me, pos(100.0, 0.0), 1.0); // overlaps nothing

    schedule.run(&mut world);

    assert_eq!(
        world.get::<Contacts>(a).unwrap().0,
        1,
        "my entity accrues its own contact"
    );
    assert_eq!(
        world.get::<Contacts>(r).unwrap().0,
        0,
        "the remote entity is NOT applied by me — its owner decides that (no cross-owner write)"
    );
    assert_eq!(
        world.get::<Contacts>(far).unwrap().0,
        0,
        "no overlap, no contact"
    );
}

/// INT-mode3-dissolves: under Mode-3 ownership (the server owns ALL entities), the
/// single authority applies EVERY contact frame-perfectly — the cross-owner case
/// dissolves, with NO code fork. A client (owns neither) applies none locally.
#[test]
fn mode3_owner_of_all_applies_every_contact() {
    let server = PeerId(0);
    let contacts_as = |local: PeerId| -> (u32, u32) {
        let (mut world, mut schedule) = interaction_world(local);
        let a = spawn_interactable(&mut world, server, pos(0.0, 0.0), 1.0);
        let b = spawn_interactable(&mut world, server, pos(1.0, 0.0), 1.0); // overlaps A
        schedule.run(&mut world);
        (
            world.get::<Contacts>(a).unwrap().0,
            world.get::<Contacts>(b).unwrap().0,
        )
    };
    // Server owns both → applies both contacts (single authority, frame-perfect).
    assert_eq!(contacts_as(server), (1, 1));
    // A client owns neither → applies none locally (it receives the server's result).
    assert_eq!(contacts_as(PeerId(9)), (0, 0));
}
