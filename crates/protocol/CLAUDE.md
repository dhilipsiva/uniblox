# CLAUDE.md — `protocol`

**Purpose:** shared wire types — protocol versions, message enums, content IDs.
**Risk tier:** standard.

## Status
Stub (Phase 1.1). No functional code yet.

## Crate-local invariants
- The `{engine, content, schema}` version triple lives here; it is the desync
  defense (matched at session join, Phase 5).
- Wire types are shared by `replication`, `transport`, `client`, `server` — a
  change here ripples everywhere; keep it minimal and versioned.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`. Do not
relitigate settled decisions — record new ones in `../../DECISIONS.md`.
