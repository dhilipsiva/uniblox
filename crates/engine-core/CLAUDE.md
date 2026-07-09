# CLAUDE.md — `engine-core`

**Purpose:** Bevy setup, shared systems, ECS components. The mode-agnostic
simulation that all three modes run.
**Risk tier:** standard (but load-bearing — the authority-swap thesis lives here).

## Status
Stub (Phase 1 scaffolding). No functional code yet.

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
gameplay branch keyed on mode breaks the core thesis — don't add one.
