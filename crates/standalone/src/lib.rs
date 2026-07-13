//! `standalone` — the Mode-1 (Standalone) runtime (ADR-0030).
//!
//! Runs the IDENTICAL simulation as every other mode — `engine_core::simulate`
//! and friends, unmodified. Mode 1 is expressed purely as data: this process
//! spawns (and therefore owns) ALL entities, so `authority_of` returns `Local`
//! for everything and the `Authority::Remote` apply arm never fires. **There is
//! no mode branch** — the authority-swap thesis, in its trivial case.
//!
//! The point of this crate is what it does NOT depend on: no `transport`, no
//! `replication`, no `matchbox`/`str0m`. The Mode-1 app assembles from
//! `engine-core` (+ `protocol`) and the bevy app/time/ecs crates only, so
//! "runs with the networking stack absent" is provable at the crate-graph level
//! (a `cargo tree` guard in `scripts/git-hooks/pre-commit` enforces it).
//!
//! App shape (mirrors the server's `MinimalPlugins` equivalent, minus the net
//! half): `FixedUpdate` at 64 Hz (`Time<Fixed>`) runs `sync_sim_dt` →
//! `advance_tick` → `simulate` → `resolve_interactions`, chained. `TimePlugin`
//! is mandatory — without it `FixedUpdate` silently never runs. There is NO
//! `Update`/`net_pump`, and no `Transport` parameter.

use std::time::Duration;

use bevy_app::{App, FixedUpdate, ScheduleRunnerPlugin, TaskPoolPlugin};
use bevy_ecs::prelude::*;
use bevy_time::{Fixed, Time, TimePlugin};
use engine_core::{
    Position, SimDt, Velocity, advance_tick, insert_sim, resolve_interactions, simulate,
    spawn_owned,
};
use protocol::PeerId;

/// The fixed simulation tick rate (also `Time<Fixed>`'s default; set explicitly
/// for intent). Matches the server so Mode 1 and Mode 3 run the sim at one rate.
pub const TICK_HZ: f64 = 64.0;

/// Feed engine-core's `SimDt` contract from the fixed clock. Inside
/// `FixedUpdate`, `Res<Time>` yields `Time<Fixed>`'s delta. A 3-line duplicate
/// of the server's private `sync_sim_dt` (`server/src/lib.rs`) — the `server`
/// crate cannot be a dependency here (it would drag in transport/replication).
fn sync_sim_dt(time: Res<Time>, mut dt: ResMut<SimDt>) {
    dt.0 = time.delta_secs();
}

/// Add the NET-FREE shared simulation systems to `FixedUpdate`. This is the seam
/// reused by the browser-playable client (Item A2): the SAME engine-core systems
/// the server runs, minus the server-only pieces (`count_tick`, `apply_input`)
/// and minus `net_pump`. Order matches the server: dt → tick → simulate →
/// interactions.
pub fn add_sim_systems(app: &mut App) {
    app.add_systems(
        FixedUpdate,
        (sync_sim_dt, advance_tick, simulate, resolve_interactions).chain(),
    );
}

/// Assemble the Mode-1 Standalone `App`: local authority over `entity_count`
/// entities, no networking. Every entity is spawned with `spawner == local`, so
/// `authority_of` returns `Local` for all of them and `simulate` integrates
/// every one. NO `Transport`, NO `Replication`, NO `Update`/`net_pump`.
pub fn build_standalone_app(local: PeerId, entity_count: usize) -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        TimePlugin,
        ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / TICK_HZ)),
    ));
    app.insert_resource(Time::<Fixed>::from_hz(TICK_HZ));

    let world = app.world_mut();
    insert_sim(world, local, (1.0 / TICK_HZ) as f32);
    // Demo scene: all entities owned by `local` (Mode-1 shape). Keep vel.x
    // nonzero so a running sim is observable on the x axis.
    for i in 0..entity_count {
        spawn_owned(
            world,
            local,
            Position {
                x: 0.0,
                y: 2.0 * i as f32,
            },
            Velocity {
                x: 2.0,
                y: 0.5 * i as f32,
            },
        );
    }

    add_sim_systems(&mut app);
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_with_all_entities_locally_owned() {
        use engine_core::Owner;
        let local = PeerId(7);
        let mut app = build_standalone_app(local, 3);
        let world = app.world_mut();
        let owners: Vec<PeerId> = world.query::<&Owner>().iter(world).map(|o| o.0).collect();
        assert_eq!(owners.len(), 3);
        assert!(owners.iter().all(|&o| o == local));
    }
}
