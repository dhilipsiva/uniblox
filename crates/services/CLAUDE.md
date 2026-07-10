# CLAUDE.md — `services`

**Purpose:** signaling / session-registry / matchmaking WebSocket service
(required even for free tiers). Becomes a binary in Phase 5.
**Risk tier:** standard (LOW — delegate; host-migration election is MIXED, human-review).

## Status
Minimal signaling binary (ADR-0012): embeds `matchbox_signaling`'s full-mesh
topology — rooms are URL paths (`ws://host:3536/<room>`), port via
`UNIBLOX_SIGNALING_PORT`, tracing via `RUST_LOG`. Phase 5 extends it.

## Crate-local invariants
- `?next=N` matchmaking is NOT in the library's FullMesh topology (it lives in the
  `matchbox_server` binary) — Phase 5 adds it via a custom `SignalingTopology`,
  together with `{mode, version}` scoping.
- **Matchmaking groups only same-mode, same-version players.** Version-triple
  filter is asymmetric: admit if client engine ≥ the game's declared minimum, but
  require content ID and schema version to match exactly.
- Stateless nodes + shared session registry (Redis/Postgres) for horizontal scale.
- Rate-limit + authenticate room creation/join (signaling-DoS surface).

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
