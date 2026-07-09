# CLAUDE.md — `server`

**Purpose:** headless authoritative Bevy sim (`MinimalPlugins` + fixed tick) —
the Mode 3 authoritative hub.
**Risk tier:** standard (Mode 3 validation logic becomes HIGH in Phases 9/11).

## Status
Stub (Phase 1 scaffolding). Prints a placeholder; no Bevy yet.

## Crate-local invariants
- Runs the **identical simulation** as the client (same `engine-core` systems),
  with authority reassigned to the server — **NO logic fork**.
- **Mode 3 is authoritative, not a relay/SFU.** That authoritative guarantee is
  what the subscription sells; if it degrades to a relay, the anti-cheat value evaporates.
- `MinimalPlugins` + `ScheduleRunnerPlugin::run_loop(Duration)`; sim in `FixedUpdate`
  at `Time::<Fixed>::from_hz(tick_rate)` (default 64 Hz). Drive network send timing
  separately from the fixed tick.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
