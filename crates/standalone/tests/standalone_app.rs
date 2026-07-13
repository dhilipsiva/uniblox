//! Acceptance battery for the Mode-1 (Standalone) runtime (ADR-0030).
//!
//! Drives the REAL assembled `App` (model: `server/tests/headless_app.rs`) and
//! confirms the Phase-4 bullet-1 property: the sim advances under LOCAL
//! authority over ALL entities, with the networking stack absent. The crate
//! graph carries no `transport`/`replication`/`matchbox`/`str0m` (the
//! `scripts/git-hooks/pre-commit` `cargo tree` guard enforces that structurally
//! — there is nothing to import here to even attempt networking).
//!
//! Counting: always component-filtered queries, never bare `Query<Entity>`
//! (bevy_ecs 0.19 stores resources on entities; a bare query would count them).

use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use engine_core::{Owner, Position, Tick};
use protocol::PeerId;
use standalone::build_standalone_app;

/// The Mode-1 app, driven on wall-clock, advances its entities under local
/// authority and ticks the sim — with no networking wired.
#[test]
fn standalone_advances_under_local_authority_without_networking() {
    let local = PeerId(1);
    let mut app = build_standalone_app(local, 2);

    // Capture the first sim entity (Position-filtered) and its starting x.
    let (entity, start_x) = {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &Position)>();
        let (e, p) = q.iter(world).next().expect("a spawned entity exists");
        (e, p.x)
    };

    // FixedUpdate accumulates from Time<Fixed> across app.update() calls; drive
    // real wall-clock until the entity advances in +x (all demo entities have
    // vel.x = 2.0), failing on a generous deadline.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.update();
        if app
            .world()
            .get::<Position>(entity)
            .expect("entity persists")
            .x
            > start_x
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "standalone sim never advanced under local authority"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // Mode-1 shape: every entity is owned by `local`, and the sim tick advanced.
    let world = app.world_mut();
    let owners: Vec<PeerId> = world.query::<&Owner>().iter(world).map(|o| o.0).collect();
    assert_eq!(owners.len(), 2, "both demo entities present");
    assert!(
        owners.iter().all(|&o| o == local),
        "every entity must be locally owned in Mode 1"
    );
    assert!(world.resource::<Tick>().0 > 0, "the sim tick advanced");
}

/// Under local authority, an entity integrates `pos += vel*dt` — the same
/// `engine_core::simulate` the server runs, with no apply/remote path taken.
#[test]
fn standalone_integrates_velocity_on_x() {
    let local = PeerId(42);
    let mut app = build_standalone_app(local, 1);

    let (entity, start_x) = {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &Position)>();
        let (e, p) = q.iter(world).next().expect("a spawned entity exists");
        (e, p.x)
    };

    let deadline = Instant::now() + Duration::from_secs(2);
    let end_x = loop {
        app.update();
        let x = app
            .world()
            .get::<Position>(entity)
            .expect("entity persists")
            .x;
        if x > start_x + 1.0 {
            break x;
        }
        assert!(Instant::now() < deadline, "sim did not integrate velocity");
        std::thread::sleep(Duration::from_millis(5));
    };
    // Moving in +x with vel.x = 2.0 > 0 — monotonic advance, never the Remote
    // (apply/no-op) arm.
    assert!(end_x > start_x + 1.0);
}
