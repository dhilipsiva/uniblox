//! `engine-core` — the mode-agnostic simulation: ECS components, shared systems.
//!
//! **The authority-swap thesis lives here.** The SAME systems run in Standalone
//! (Mode 1), P2P (Mode 2), and Full-Server (Mode 3); the ONLY thing that varies
//! across modes is data — which [`PeerId`] owns each entity ([`Owner`]) and who
//! "I" am ([`LocalPeer`]) — seeded by the wiring layer (`client`/`server`).
//!
//! Invariants (see `CLAUDE.md`):
//! - [`authority_of`] is the SINGLE authority decision point (one call site, in
//!   [`simulate`]). No mode-specific gameplay branches exist — this crate has no
//!   "mode" concept at all, which is what makes the property auditable.
//! - Split: **authority computes state; receivers apply state.** The apply path
//!   must NEVER re-simulate other peers' entities (no cross-platform float
//!   determinism — receivers apply replicated snapshots and interpolate).
//! - Single-ownership: exactly one [`Owner`] per entity; default owner = spawner.

use bevy_ecs::prelude::*;
use protocol::PeerId;

/// 2D position, in world units. Plain `f32` fields (not a math-lib type): the
/// replication quantizer (fixed-point positions, later in Phase 1) walks these
/// values directly, and render-side smoothing converts at the render boundary.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

/// 2D velocity, in world units per second.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Velocity {
    pub x: f32,
    pub y: f32,
}

/// The single authority tag: which peer owns (is authoritative over) this
/// entity. Exactly one per entity (ECS guarantees one component per type);
/// default owner = the entity's spawner (see [`spawn_owned`]). Ownership
/// handoff (later in Phase 1) mutates this component — nothing else changes.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner(pub PeerId);

/// Who "I" am in this running instance. In Mode 1 it matches every entity's
/// owner; in Mode 2 each peer's instance differs; in Mode 3 the server's
/// matches all entities and no client's does. Pure data — never a mode enum.
#[derive(Resource, Clone, Copy, Debug)]
pub struct LocalPeer(pub PeerId);

/// Fixed simulation timestep in seconds. The engine-core contract for headless
/// use; the app boundary (Mode 3 server / client) feeds it from the fixed
/// clock (`Time::<Fixed>`) so this crate never depends on `bevy_time`/`bevy_app`.
#[derive(Resource, Clone, Copy, Debug)]
pub struct SimDt(pub f32);

/// The result of the authority decision for one entity in one instance.
/// Deliberately binary — handoff needs no third state (it mutates [`Owner`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authority {
    /// This instance is authoritative: compute the entity's state.
    Local,
    /// Another peer is authoritative: apply its replicated state (never re-simulate).
    Remote,
}

/// THE single authority decision point — the only place authority is *decided*.
/// Pure over [`PeerId`] so it unit-tests without a `World`. Gameplay has exactly
/// one call site (in [`simulate`]); the replication layer calls this SAME
/// function to gate send/apply — never duplicate the comparison inline.
pub fn authority_of(owner: PeerId, local: PeerId) -> Authority {
    if owner == local {
        Authority::Local
    } else {
        Authority::Remote
    }
}

/// The mode-agnostic per-tick simulation: integrate owned entities, leave
/// remote entities to the apply path.
pub fn simulate(
    dt: Res<SimDt>,
    local: Res<LocalPeer>,
    mut q: Query<(&Owner, &mut Position, &Velocity)>,
) {
    for (owner, mut pos, vel) in &mut q {
        match authority_of(owner.0, local.0) {
            // AUTHORITY COMPUTES STATE. Mutating Position fires
            // Changed<Position> — the replication sender (later in Phase 1)
            // reads that, but must gate on authority too, not Changed alone,
            // or remote-applied mutations get echoed back.
            Authority::Local => {
                pos.x += vel.x * dt.0;
                pos.y += vel.y * dt.0;
            }
            // RECEIVER APPLIES STATE. The replication layer (later in Phase 1)
            // fills this path with "apply the latest replicated snapshot +
            // interpolate". It MUST NOT integrate velocity or otherwise
            // re-simulate — receivers never recompute other peers' entities
            // (root invariant: no cross-platform float determinism). This
            // empty arm is the documented apply-path placeholder — do NOT
            // remove it as dead code.
            Authority::Remote => {}
        }
    }
}

/// Spawn a simulated entity with an explicit owner. In gameplay this is the
/// spawner itself (the default-ownership rule: an entity is owned by the peer
/// that creates it); the replication receive path passes the REMOTE owner when
/// instantiating a replica of another peer's entity — the same construction,
/// different data. This is the sole construction path for sim entities, so
/// nothing with a [`Position`] can exist without an [`Owner`] (an ownerless
/// entity would be silently skipped by both simulation and replication).
pub fn spawn_owned(world: &mut World, spawner: PeerId, pos: Position, vel: Velocity) -> Entity {
    world.spawn((Owner(spawner), pos, vel)).id()
}

/// Seed a world for simulation: who "I" am and the fixed timestep. Mode is
/// expressed purely by how callers assign owners relative to `local`.
pub fn insert_sim(world: &mut World, local: PeerId, dt: f32) {
    world.insert_resource(LocalPeer(local));
    world.insert_resource(SimDt(dt));
}
