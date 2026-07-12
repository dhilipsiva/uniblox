# CLAUDE.md — `server`

**Purpose:** headless authoritative Bevy sim (`MinimalPlugins` + fixed tick) —
the Mode 3 authoritative hub.
**Risk tier:** standard (Mode 3 validation logic becomes HIGH in Phases 9/11).

## Status
Implemented (the Mode-3 headless runtime, ADR-0014 — the authority-swap gate PASSED).
`build_server_app`: standalone `bevy_app`+`bevy_time` (TaskPool + Time + ScheduleRunner
at 1/64 s — NOT the `bevy` umbrella; `MinimalPlugins` lives in `bevy_internal`),
FixedUpdate = `sync_sim_dt` → `count_tick` → `advance_tick` → `apply_input` → `simulate`
(chained; `SimDt` fed from the fixed clock at the app boundary), Update = exclusive
`net_pump` (NonSend `Net`; receive every frame, emit acks (`drain_acks`) every frame,
collect+send at `NET_INTERVAL` 50 ms via a virtual-clock accumulator).
Mode 3 is expressed purely as data: the server spawns/owns everything. Exit via
`Messages<AppExit>` (0.19 renamed Events→Messages). M3/M4 tests drive the real App;
demo entities must keep nonzero vel.x (test predicates observe replay-ordered proxies).
`ack_round_trip_confirms_and_goes_quiet` covers the ADR-0020 ack wiring end-to-end over
the real pump: a stationary server entity goes quiet once the client acks it, AND a
client-OWNED stationary entity (Mode-2-shaped) goes quiet once the server's ack-routing
confirms it — the test `Client` carries the client-side ack/collect pump wiring a real
client will need.

## Crate-local invariants
- Runs the **identical simulation** as the client (same `engine-core` systems),
  with authority reassigned to the server — **NO logic fork**.
- **Mode 3 is authoritative, not a relay/SFU.** That authoritative guarantee is
  what the subscription sells; if it degrades to a relay, the anti-cheat value evaporates.
- Standalone `bevy_app`+`bevy_time` assembly + `ScheduleRunnerPlugin::run_loop(Duration)`;
  sim in `FixedUpdate` at `Time::<Fixed>::from_hz(64.0)`. Network send timing is driven
  separately from the fixed tick (virtual-clock accumulator in Update) — fixed-timestep
  is not wall-clock.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
