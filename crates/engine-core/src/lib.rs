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

use std::collections::{HashMap, VecDeque};

use bevy_ecs::prelude::*;
use protocol::{PeerId, dequantize, quantize};

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

/// The 2D intent of one input command (ADR-0022 Stage B): a desired velocity for
/// the mini-game (whose sim is `pos += vel*dt`). Set by the input device / test.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Intent {
    pub vx: f32,
    pub vy: f32,
}

/// One input command: a monotonic per-controlled-entity `seq` + its `intent`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Input {
    pub seq: u64,
    pub intent: Intent,
}

/// CLIENT marker (ADR-0022): THIS instance drives this entity with input and
/// mints its monotonic input seqs. Orthogonal to [`Owner`] — in Mode 3 the
/// avatar is `Controlled` here yet owned (authority) by the server: the
/// PREDICTED role (`authority == Remote` + `Controlled`).
#[derive(Component, Clone, Copy, Debug)]
pub struct Controlled {
    pub next_seq: u64,
}

/// AUTHORITY marker (ADR-0022): this entity is driven by the given peer — the
/// authority applies that peer's inputs to it. The client↔avatar association
/// (session join, future) sets this; tests designate it.
///
/// SCOPE (auditor F2): Stage B assumes ONE controlled entity per peer (the
/// Mode-3 avatar model). The reconciliation marker `StateMsg.last_input` is
/// per-peer-message and `ProcessedInput` is per-peer, so two entities marked
/// with the same peer would share one input stream (the second starves).
/// Multi-avatar-per-peer needs a per-entity wire marker — a future item.
#[derive(Component, Clone, Copy, Debug)]
pub struct ControlledBy(pub PeerId);

/// CLIENT (ADR-0022): applied-but-unacked inputs on the controlled avatar,
/// replayed each [`predict`] tick from the authoritative anchor. Pruned by the
/// reconciliation marker; capped ([`INPUT_CAP`]) — bounded prediction error if
/// the authority stalls.
#[derive(Component, Default)]
pub struct InputHistory(pub VecDeque<Input>);

/// SERVER (ADR-0022): per-controlled-entity input jitter buffer, filled by the
/// receiver, drained ONE-per-tick by [`apply_input`].
#[derive(Resource, Default)]
pub struct PendingInputs(pub HashMap<Entity, VecDeque<Input>>);

/// SERVER (ADR-0022): the newest input seq PROCESSED per controlling peer —
/// stamped into that peer's snapshot as the reconciliation marker
/// (`StateMsg.last_input`).
#[derive(Resource, Default)]
pub struct ProcessedInput(pub HashMap<PeerId, u64>);

/// Max buffered inputs (client history / server pending) — bounds prediction
/// error / server memory if a peer stalls.
const INPUT_CAP: usize = 128;

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

/// CLIENT (ADR-0022): record an input on a controlled entity — mint
/// `Input{next_seq, intent}`, push it to the entity's [`InputHistory`], and bump
/// `next_seq`. The input device / test calls this once per sim tick before
/// [`predict`]. No-op if the entity isn't `Controlled` / has no history.
pub fn record_input(world: &mut World, entity: Entity, intent: Intent) {
    // Store the intent EXACTLY as it will cross the wire (quantize → dequantize),
    // so the client's replay reproduces the server's applied Velocity bit-for-bit
    // for a representable value — a correct prediction reconciles with NO pop
    // (auditor F1: predicting the raw value left a sub-unit pop each snapshot).
    let intent = Intent {
        vx: dequantize(quantize(intent.vx)),
        vy: dequantize(quantize(intent.vy)),
    };
    let Some(seq) = world.get_mut::<Controlled>(entity).map(|mut c| {
        let s = c.next_seq;
        c.next_seq += 1;
        s
    }) else {
        return;
    };
    if let Some(mut hist) = world.get_mut::<InputHistory>(entity) {
        hist.0.push_back(Input { seq, intent });
        while hist.0.len() > INPUT_CAP {
            hist.0.pop_front();
        }
    }
}

/// CLIENT prediction (ADR-0022): for each controlled entity, recompute
/// `RenderPos` from the authoritative `Position` ANCHOR + replay of the un-acked
/// [`InputHistory`] (one dt step per input). Recomputed from the anchor every
/// tick — so it never accumulates float error and re-pins to server truth on
/// every snapshot. NEVER writes authoritative `Position`/`Velocity` (the
/// predicted avatar is `Remote`, so the sender structurally never emits it).
pub fn predict(
    dt: Res<SimDt>,
    mut q: Query<(&Position, &InputHistory, &mut RenderPos), With<Controlled>>,
) {
    for (pos, hist, mut render) in &mut q {
        let (mut x, mut y) = (pos.x, pos.y);
        for input in &hist.0 {
            x += input.intent.vx * dt.0;
            y += input.intent.vy * dt.0;
        }
        render.x = x;
        render.y = y;
    }
}

/// SERVER (ADR-0022): process ONE queued input per controlled entity — set its
/// authoritative `Velocity = intent` (or ZERO on underrun) and record
/// `ProcessedInput[peer] = seq`. Runs in `FixedUpdate` BEFORE `simulate`, so
/// `simulate` integrates exactly ONE input's displacement per tick — the
/// alignment that lets the client's replay (one `intent*dt` per history entry)
/// reproduce the server and reconciliation converge. Skips a seq ≤
/// already-processed (duplicate/stale, consumes no tick). On underrun (no fresh
/// input) it ZEROS Velocity — matching `predict`, which adds nothing for a tick
/// with no input (a "held" velocity would move the server past the client's
/// replay ⇒ a forward pop on every stall — auditor F3). Inputs MUST arrive
/// reliable+ordered: a gap advances the marker past the missing input, which the
/// client then over-prunes with no recovery (no anti-entropy for inputs).
pub fn apply_input(
    mut pending: ResMut<PendingInputs>,
    mut processed: ResMut<ProcessedInput>,
    mut q: Query<(Entity, &ControlledBy, &mut Velocity)>,
) {
    for (entity, controlled_by, mut vel) in &mut q {
        let peer = controlled_by.0;
        let last = processed.0.get(&peer).copied().unwrap_or(0);
        let mut fresh = None;
        if let Some(queue) = pending.0.get_mut(&entity) {
            while let Some(input) = queue.front().copied() {
                queue.pop_front();
                if input.seq <= last {
                    continue; // duplicate / already processed — skip, don't consume a tick
                }
                fresh = Some(input);
                break; // exactly ONE fresh input per tick
            }
        }
        match fresh {
            Some(input) => {
                vel.x = input.intent.vx;
                vel.y = input.intent.vy;
                processed.0.insert(peer, input.seq);
            }
            None => {
                // Underrun: no movement this tick (matches the client's replay).
                vel.x = 0.0;
                vel.y = 0.0;
            }
        }
    }
}

/// SERVER receiver helper (ADR-0022): queue a received input for its controlled
/// entity, capping the jitter buffer at [`INPUT_CAP`].
pub fn push_pending_input(pending: &mut PendingInputs, entity: Entity, input: Input) {
    let queue = pending.0.entry(entity).or_default();
    queue.push_back(input);
    while queue.len() > INPUT_CAP {
        queue.pop_front();
    }
}

/// The render role of an entity, cached so [`reset_render_role`] can detect a
/// TRANSITION (handoff / control change) and run the flush/seed exactly once.
/// Internal bookkeeping — derived from `(authority × Controlled)`.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
enum RenderRole {
    /// authority Local — the local sim IS the truth (`RenderPos = Position`).
    Owned,
    /// authority Remote, not controlled — interpolate a snapshot buffer.
    Interpolated,
    /// authority Remote, controlled — predict from input, reconcile to snapshots.
    Predicted,
}

/// Maintain each entity's render role on a CHANGE — the handoff interplay
/// (ADR-0022 Stage C). Diffs the desired role (from `authority × Controlled`)
/// against a cached [`RenderRole`] and, on a transition, runs the flush/seed:
/// - **→ Owned** (adopt / spawn-owned): drop `InterpBuffer`; clear
///   `InputHistory` (now authoritative — no reconcile).
/// - **→ Predicted** (relinquish-but-keep-control / Mode-3 avatar): drop
///   `InterpBuffer`; ensure a fresh `InputHistory`.
/// - **→ Interpolated** (relinquish to a remote / observe): drop `InputHistory`;
///   ensure an `InterpBuffer` (kept if already present — a new proxy's buffer).
///
/// On a transition it also re-seeds `RenderPos` from the AUTHORITATIVE `Position`
/// (never the stale interpolated `RenderPos`) so an entity is never left
/// rendering ~DELAY in the past across a role change (the Phase-1 auditor's
/// flagged adoption bug). In the standard schedule this seed is belt-and-braces:
/// the same-frame `copy_owned_render`/`predict`/`interpolate` overwrite it in
/// their respective roles — it is the sole writer only for a freshly-relinquished
/// Interpolated entity whose buffer is still empty. The load-bearing effect is
/// the component add/remove (drop a stale `InterpBuffer`/`InputHistory` so old
/// snapshots can't lerp / old inputs can't replay against the new authority). An
/// exclusive system for single-site, deterministic structural changes.
pub fn reset_render_role(world: &mut World) {
    let local = world.resource::<LocalPeer>().0;
    let mut transitions: Vec<(Entity, RenderRole)> = Vec::new();
    {
        let mut q = world.query::<(Entity, &Owner, Option<&Controlled>, Option<&RenderRole>)>();
        for (e, owner, controlled, cached) in q.iter(world) {
            let desired = if authority_of(owner.0, local) == Authority::Local {
                RenderRole::Owned
            } else if controlled.is_some() {
                RenderRole::Predicted
            } else {
                RenderRole::Interpolated
            };
            if cached.copied() != Some(desired) {
                transitions.push((e, desired));
            }
        }
    }
    for (e, role) in transitions {
        match role {
            RenderRole::Owned => {
                world.entity_mut(e).remove::<InterpBuffer>();
                if let Some(mut hist) = world.get_mut::<InputHistory>(e) {
                    hist.0.clear();
                }
            }
            RenderRole::Predicted => {
                world.entity_mut(e).remove::<InterpBuffer>();
                if world.get::<InputHistory>(e).is_none() {
                    world.entity_mut(e).insert(InputHistory::default());
                }
            }
            RenderRole::Interpolated => {
                world.entity_mut(e).remove::<InputHistory>();
                if world.get::<InterpBuffer>(e).is_none() {
                    world.entity_mut(e).insert(InterpBuffer::default());
                }
            }
        }
        // Seed RenderPos from the AUTHORITATIVE Position (never the stale interp).
        if let Some(pos) = world.get::<Position>(e).copied()
            && let Some(mut render) = world.get_mut::<RenderPos>(e)
        {
            render.x = pos.x;
            render.y = pos.y;
        }
        world.entity_mut(e).insert(role);
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
    // Server input-processing state (ADR-0022) — empty on a client (apply_input
    // is a no-op without ControlledBy entities); present so the system's ResMut
    // params always resolve.
    world.insert_resource(PendingInputs::default());
    world.insert_resource(ProcessedInput::default());
}
