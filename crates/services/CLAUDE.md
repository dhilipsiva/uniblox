# CLAUDE.md — `services`

**Purpose:** signaling / session-registry / matchmaking WebSocket service
(required even for free tiers). Becomes a binary in Phase 5.
**Risk tier:** standard (LOW — delegate; host-migration election is MIXED, human-review).

## Status
**Scoped signaling service (ADR-0037) — library + binary.** `build_signaling_server(addr, registry)` wraps
`matchbox_signaling`'s FullMesh with `{mode, version}` SCOPING via the room PATH (`<mode>~<engine>.<content>.<schema>~<lobby>`,
e.g. `m1~1.2.3~arena`): FullMesh isolates strictly by the path string, so a different mode/version is a different
room and is NEVER matched (offers/answers relay only within one room) — structural enforcement, reusing the proven
relay. An `on_connection_request` gate rejects a malformed `~`-scoped path (401); a plain path (no `~`) is a legacy
room (accepted — keeps `uniblox-demo` working). The in-memory `SessionRegistry` lists active sessions
(`list()`/`peer_count()`/`session_count()`), tracked by a lifecycle-BALANCED callback chain: gate stashes `room`
by `origin` → `on_id_assignment` bridges `peer→room` → `on_peer_connected` joins into `sessions` → `on_peer_disconnected`
removes+prunes (the add is at post-upgrade `on_peer_connected`, NOT pre-upgrade id-assignment, so a failed upgrade
never over-reports). `parse_scope` + `Scope`/`Mode`(signaling-local tag) reuse `protocol::VersionTriple`. Binary =
thin wrapper (port `UNIBLOX_SIGNALING_PORT`/3536, `RUST_LOG` tracing). 3 unit + 7 raw-WS integration tests.

## Crate-local invariants
- **DONE (ADR-0037):** `{mode, version}` scoping via the room path (exact-match; FullMesh isolates structurally) +
  the connection gate + the in-memory session registry. **Matchmaking groups only same-mode, same-version players**
  (a different mode/version is a different room ⇒ never matched).
- **DEFERRED (later Phase-5 bullets), each needing more than exact path-scoping:** a custom `SignalingTopology`
  with client-specified `?next=N` session-SIZE grouping (matchbox's `?next=N` lives in the un-vendored
  `matchbox_server`, not the library); the ASYMMETRIC version filter (admit engine ≥ declared minimum, content +
  schema exact — needs grouping compatible-but-not-identical peers); stateless nodes + a shared Redis/Postgres
  registry for horizontal scale; rate-limit + authenticate room join (signaling-DoS, Phase 11).
- Scoping is structural ISOLATION, not access control (no room secret; a client may name any path — auth is
  the deferred bullet).

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
