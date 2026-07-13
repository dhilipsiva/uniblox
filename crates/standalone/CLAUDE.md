# CLAUDE.md — `standalone`

**Purpose:** the Mode-1 (Standalone) runtime — free, local-authority, no
networking, no anti-cheat. Assembles the headless Mode-1 Bevy app.
**Risk tier:** standard.

## Status
Implemented (ADR-0030, Phase 4 Item A1). `build_standalone_app(local, entity_count)` assembles the Mode-1 app:
`(TaskPoolPlugin, TimePlugin, ScheduleRunnerPlugin::run_loop(1/64 s))` + `Time::<Fixed>::from_hz(64.0)` +
`insert_sim(world, local, 1/64)` + a `spawn_owned(owner=local)` loop + the FixedUpdate sim chain. Mode 1 is
expressed purely as data — every entity is owned by `local`, so `authority_of` returns `Local` for all and
`simulate` integrates every one (the `Authority::Remote` arm never fires). `add_sim_systems(app)` is the
NET-FREE shared seam (reused by the browser-playable client, Item A2): `(sync_sim_dt, advance_tick, simulate,
resolve_interactions).chain()` on `FixedUpdate` — the SAME engine-core systems the server runs, minus the
server-only `count_tick`/`apply_input` and minus `net_pump`. `sync_sim_dt` is a 3-line duplicate of the
server's private one (the `server` crate can't be a dependency here — it would pull transport/replication in).
`src/main.rs` is a `cargo run -p standalone` demo. Acceptance: `tests/standalone_app.rs` drives the real App and
confirms local-authority advance + all-`Local` ownership + `Tick` advanced, with the net stack absent.

## Crate-local invariants
- **NET-FREE crate graph is the point.** Deps are `engine-core` (+ `protocol`) and `bevy_app`/`bevy_time`/
  `bevy_ecs` only — NO `transport`/`replication`/`matchbox`/`str0m`. That absence IS the "runs with the
  networking stack absent" acceptance; a `cargo tree -p standalone` check in `scripts/git-hooks/pre-commit`
  guards it. Do NOT add a networked dependency here — a mode that needs networking is Mode 2/3, not this crate.
- Runs the **identical simulation** as the client and server (same `engine-core` systems), with authority =
  local over ALL entities — **NO logic fork**. Mode is data, never a mode enum/branch.
- Standalone `bevy_app`+`bevy_time` assembly + `ScheduleRunnerPlugin::run_loop(Duration)`; sim in `FixedUpdate`
  at `Time::<Fixed>::from_hz(64.0)`.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
