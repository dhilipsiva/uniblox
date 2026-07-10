# CLAUDE.md — `protocol`

**Purpose:** shared wire types — protocol versions, message enums, content IDs.
**Risk tier:** standard.

## Status
Minimal: `PeerId(u64)` (shared peer identity; `Ord` for the Phase-3/5 lowest-peer-ID
host-migration tiebreak). Wire messages, the version triple, and serde land with the
replication wire format (later in Phase 1) and Phase 5.

## Crate-local invariants
- The `{engine, content, schema}` version triple lives here; it is the desync
  defense (matched at session join, Phase 5).
- Wire types are shared by `replication`, `transport`, `client`, `server` — a
  change here ripples everywhere; keep it minimal and versioned.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`. Do not
relitigate settled decisions — record new ones in `../../DECISIONS.md`.
