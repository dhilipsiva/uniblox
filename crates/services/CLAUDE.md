# CLAUDE.md — `services`

**Purpose:** signaling / session-registry / matchmaking WebSocket service
(required even for free tiers). Becomes a binary in Phase 5.
**Risk tier:** standard (LOW — delegate; host-migration election is MIXED, human-review).

## Status
Stub (Phase 1 scaffolding). No functional code yet.

## Crate-local invariants
- Start from `matchbox_server`'s room-based signaling + crude `?next=N`
  matchmaking; extend for `{mode, version}` scoping.
- **Matchmaking groups only same-mode, same-version players.** Version-triple
  filter is asymmetric: admit if client engine ≥ the game's declared minimum, but
  require content ID and schema version to match exactly.
- Stateless nodes + shared session registry (Redis/Postgres) for horizontal scale.
- Rate-limit + authenticate room creation/join (signaling-DoS surface).
- Evaluate `github.com/dhilipsiva/nibli` for reusable signaling/NAT-traversal code.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
