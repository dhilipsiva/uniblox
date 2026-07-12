# CLAUDE.md — `engine-core`

**Purpose:** Bevy setup, shared systems, ECS components. The mode-agnostic
simulation that all three modes run.
**Risk tier:** standard (but load-bearing — the authority-swap thesis lives here).

## Status
Implemented (the mini-game sim + authority abstraction). `Position`/`Velocity`/`Owner`
components, `LocalPeer`/`SimDt` resources, `authority_of` (the single decision point,
pure over `PeerId`), and the one branching `simulate` system: `Local` computes
(`pos += vel*dt`), `Remote` is the **documented apply-path placeholder** — replication
(later in Phase 1) fills it with "apply snapshot + interpolate"; it must NEVER
re-integrate velocity. `spawn_owned` is the sole sim-entity construction path.
8 tests green incl. the Mode-2 two-perspective and Mode-3 shape proofs (the
authority-swap demonstrated at the unit level, before transport exists).

**Render/interpolation (ADR-0022 Stage A):** a SEPARATE `RenderPos` component is
the render-boundary output — the ONLY thing the render path writes, so
authoritative `Position` stays snap-applied (receivers never re-simulate others).
`interpolate` lerps a remote's `InterpBuffer(VecDeque<Snapshot>)` at
`RenderTick − INTERP_DELAY_TICKS` (clamps out of range — NEVER extrapolates);
`copy_owned_render` sets `RenderPos=Position` for Local entities (schedule it
AFTER `interpolate`). `Tick` (advanced by `advance_tick`) is the authoritative
sim tick stamped into snapshots; `RenderTick` is the interp clock (app-advanced;
tests set it). `push_snapshot` is cap-evicting + tick-monotonic. `spawn_owned`
attaches `RenderPos`. Stages B (predict-own/reconcile) + C (handoff) to come.

## Crate-local invariants
- **The SAME systems run in all three modes; only authority assignment differs.**
  There must be a single `authority_of(entity)` decision point and **no
  mode-specific gameplay branches** (provable by grep/audit).
- Split logic into "authority computes state" vs "receiver applies state."
- **Default ownership = the entity's spawner/controller.**
- Server (Mode 3) uses `MinimalPlugins` + `ScheduleRunnerPlugin` + `FixedUpdate`
  at a fixed tick; drive network send timing separately (fixed-timestep ≠ wall-clock).

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`. A new
gameplay branch keyed on mode breaks the core thesis — don't add one. The
`Remote` match arm in `simulate` is a placeholder, not dead code — do not delete
it. The future replication sender must gate on `authority_of`, never on
`Changed<Position>` alone (remote-applied mutations also fire `Changed` → echo-back).
