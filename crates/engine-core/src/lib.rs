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

use std::collections::VecDeque;

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

/// Render-space position (ADR-0022): the SMOOTHED output the render boundary
/// reads. The ONLY thing interpolation/prediction write — authoritative
/// [`Position`] stays snap-applied, so receivers never re-simulate others'
/// entities. For an owned entity it tracks `Position`; for an interpolated
/// remote it lags ~[`INTERP_DELAY_TICKS`] behind; for a predicted avatar (later
/// stage) it leads. Every sim entity carries one ([`spawn_owned`] adds it).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct RenderPos {
    pub x: f32,
    pub y: f32,
}

/// One buffered authoritative snapshot for interpolation (ADR-0022): the
/// position the sender held at sim-tick `tick`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snapshot {
    pub tick: u64,
    pub x: f32,
    pub y: f32,
}

/// Ring buffer of received snapshots for a REMOTE, non-controlled (interpolated)
/// entity — the interpolation source. The receiver pushes on each applied
/// snapshot; capped at [`INTERP_BUFFER_CAP`]. Presence of this component is what
/// marks an entity "interpolated" (owned/predicted entities never have it).
#[derive(Component, Default)]
pub struct InterpBuffer(pub VecDeque<Snapshot>);

/// The interpolation clock, in SIM-TICK units. The app advances it from
/// wall-clock (converted at the sim rate); tests set it directly (virtual clock).
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct RenderTick(pub f64);

/// The authoritative sim tick (ADR-0022): stamped into outgoing snapshots as the
/// interpolation time axis, and advanced once per sim tick by [`advance_tick`].
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct Tick(pub u64);

/// Interpolation delay in SIM-TICK units: ~100 ms at 64 Hz ≈ 2 net ticks. The
/// receiver renders interpolated entities this far behind the newest snapshot,
/// so it always has two snapshots to lerp between (hides jitter + one drop).
pub const INTERP_DELAY_TICKS: f64 = 6.4;

/// Max buffered snapshots per interpolated entity (~1.6 s at 20 Hz net).
const INTERP_BUFFER_CAP: usize = 32;

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

/// Advance the authoritative sim tick once per sim step (ADR-0022). The server
/// chains this in `FixedUpdate`; it feeds the `tick` stamped into snapshots.
pub fn advance_tick(mut tick: ResMut<Tick>) {
    tick.0 += 1;
}

/// Owned (authority `Local`) entities render at their authoritative position —
/// the local sim IS the prediction, no smoothing needed. `RenderPos = Position`.
/// (Interpolated remotes are driven by [`interpolate`]; they carry an
/// [`InterpBuffer`] and are `Remote`, so this leaves them untouched.)
///
/// ORDER (auditor): schedule this AFTER [`interpolate`] — both write `RenderPos`,
/// and an entity newly adopted to `Local` may still carry a stale `InterpBuffer`
/// until the role-reset removes it; running last guarantees a `Local` entity's
/// `RenderPos` ends at its authoritative `Position`, not a frozen old snapshot.
pub fn copy_owned_render(local: Res<LocalPeer>, mut q: Query<(&Owner, &Position, &mut RenderPos)>) {
    for (owner, pos, mut render) in &mut q {
        if authority_of(owner.0, local.0) == Authority::Local {
            render.x = pos.x;
            render.y = pos.y;
        }
    }
}

/// Interpolate REMOTE (interpolated) entities: sample each [`InterpBuffer`] at
/// `RenderTick - INTERP_DELAY_TICKS`, lerp the two bracketing snapshots into
/// `RenderPos`. Out of range it CLAMPS to the newest/oldest buffered snapshot —
/// it NEVER extrapolates (a receiver must not re-simulate others' entities).
/// Runs at render frame rate; the authoritative `Position` is untouched.
pub fn interpolate(render_tick: Res<RenderTick>, mut q: Query<(&InterpBuffer, &mut RenderPos)>) {
    let target = render_tick.0 - INTERP_DELAY_TICKS;
    for (buf, mut render) in &mut q {
        if let Some((x, y)) = sample_buffer(&buf.0, target) {
            render.x = x;
            render.y = y;
        }
    }
}

/// Sample a snapshot buffer at `target` (sim-tick units): lerp between the two
/// bracketing snapshots; clamp to the oldest/newest out of range (no
/// extrapolation); `None` if empty. Pure — the core of [`interpolate`].
fn sample_buffer(buf: &VecDeque<Snapshot>, target: f64) -> Option<(f32, f32)> {
    let oldest = buf.front()?;
    let newest = buf.back()?;
    if target <= oldest.tick as f64 {
        return Some((oldest.x, oldest.y)); // before the buffer — hold the oldest
    }
    if target >= newest.tick as f64 {
        return Some((newest.x, newest.y)); // past the newest — clamp, never extrapolate
    }
    for i in 0..buf.len().saturating_sub(1) {
        let (a, b) = (buf[i], buf[i + 1]);
        if (a.tick as f64) <= target && target <= (b.tick as f64) {
            let span = (b.tick - a.tick) as f64;
            let f = if span > 0.0 {
                ((target - a.tick as f64) / span) as f32
            } else {
                0.0
            };
            return Some((a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f));
        }
    }
    Some((newest.x, newest.y)) // unreachable (target is bracketed) — clamp defensively
}

/// Push a received snapshot into an interpolation buffer, evicting the oldest
/// past [`INTERP_BUFFER_CAP`]. The receiver calls this on each applied snapshot.
/// DROPS a snapshot whose tick is not strictly newer than the buffer's back, so
/// the buffer stays tick-monotonic by construction (a buggy/reordered sender
/// can't warp the lerp with an out-of-order or duplicate tick — auditor NIT).
pub fn push_snapshot(buf: &mut InterpBuffer, snap: Snapshot) {
    if buf.0.back().is_some_and(|b| snap.tick <= b.tick) {
        return;
    }
    buf.0.push_back(snap);
    while buf.0.len() > INTERP_BUFFER_CAP {
        buf.0.pop_front();
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
    // Every sim entity carries a RenderPos, seeded to the spawn position (ADR-0022
    // render-boundary smoothing). Interpolated remotes additionally get an
    // InterpBuffer attached by the replication receiver on Spawn.
    let render = RenderPos { x: pos.x, y: pos.y };
    world.spawn((Owner(spawner), pos, vel, render)).id()
}

/// Seed a world for simulation: who "I" am and the fixed timestep. Mode is
/// expressed purely by how callers assign owners relative to `local`.
pub fn insert_sim(world: &mut World, local: PeerId, dt: f32) {
    world.insert_resource(LocalPeer(local));
    world.insert_resource(SimDt(dt));
    world.insert_resource(Tick(0));
    world.insert_resource(RenderTick(0.0));
}
