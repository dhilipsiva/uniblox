# CLAUDE.md — `services`

**Purpose:** signaling / session-registry / matchmaking WebSocket service
(required even for free tiers). Becomes a binary in Phase 5.
**Risk tier:** standard (LOW — delegate; host-migration election is MIXED, human-review).

## Status
**Scoped signaling service with the asymmetric version filter (ADR-0037/0038) — library + binary.**
`build_signaling_server(addr, registry)` wraps `matchbox_signaling`'s FullMesh with a scope in the room PATH
`<mode>~<content>.<schema>~<min>~<lobby>` (e.g. `m1~7.2~3~arena` = mode m1, content 7, schema 2, min-engine 3,
lobby arena); the client's OWN engine rides the `?engine=N` query. FullMesh isolates strictly by the path string,
so content/schema/min/lobby in the path give EXACT-match structural isolation, while the engine — deliberately OUT
of the key — lets different-but-compatible versions share ONE room. The `on_connection_request` gate implements the
**ASYMMETRIC filter**: admit iff `?engine >= min_engine`, else a REASONED rejection — `426` for too-old, `400` for a
malformed scope / missing-or-non-numeric `?engine` (built via `axum` `(StatusCode, body).into_response()`, returned
verbatim by matchbox on `Err`). A plain path (no `~`) is a legacy room (accepted, no engine gate — keeps
`uniblox-demo` working). The in-memory `SessionRegistry` lists active sessions (`list()`/`peer_count()`/
`session_count()`), tracked by a lifecycle-BALANCED callback chain: gate stashes `room` by `origin` →
`on_id_assignment` bridges `peer→room` → `on_peer_connected` joins into `sessions` → `on_peer_disconnected`
removes+prunes (the add is at post-upgrade `on_peer_connected`, NOT pre-upgrade id-assignment, so a failed upgrade
never over-reports). `parse_scope`/`parse_engine` + `Scope`/`Mode` (signaling-local tag). Binary = thin wrapper
(port `UNIBLOX_SIGNALING_PORT`/3536, `RUST_LOG` tracing). 4 unit + 10 raw-WS integration tests.

**Trust model:** `?engine` + path `min` are self-declared — desync defense for HONEST clients, not anti-cheat (a
modified client can lie, but can't lie into a *stricter* room with an old engine: a different `min` is a different
room).

## Crate-local invariants
- **DONE (ADR-0037/0038):** `{mode, version}` scoping via the room path + the connection gate + the in-memory
  session registry, AND the **ASYMMETRIC version filter** — admit `engine >= the game's declared minimum` (engine
  releases are backward-compatible), require content + schema EXACT (structural), reasoned `426`/`400` rejection.
  Matchmaking groups only same-mode/same-content/same-schema players; compatible engines (≥ min) share a session.
- **DEFERRED (later Phase-5 bullets):** a custom `SignalingTopology` with client-specified `?next=N` session-SIZE
  grouping (matchbox's `?next=N` lives in the un-vendored `matchbox_server`, not the library); the Mode-2
  coordinator peer service; stateless nodes + a shared Redis/Postgres registry for horizontal scale; rate-limit +
  authenticate room join (signaling-DoS, Phase 11).
- Scoping is structural ISOLATION, not access control (no room secret; a client may name any path — auth is
  the deferred bullet).

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
