# CLAUDE.md — `services`

**Purpose:** signaling / session-registry / matchmaking WebSocket service
(required even for free tiers). Becomes a binary in Phase 5.
**Risk tier:** standard (LOW — delegate; host-migration election is MIXED, human-review).

## Status
**Scoped signaling with the asymmetric filter + `?next=N` grouping (ADR-0037/0038/0039) — library + binary.**
`build_signaling_server(addr, registry)` builds the custom **`NextTopology`** (`SignalingServerBuilder::new`, NOT
FullMesh) with a scope in the room PATH `<mode>~<content>.<schema>~<min>~<lobby>` (e.g. `m1~7.2~3~arena`); the
client's OWN engine rides the `?engine=N` query. The topology isolates strictly by the path string, so
content/schema/min/lobby give EXACT-match structural isolation while the engine (out of the key) lets
different-but-compatible versions share ONE session. The `on_connection_request` gate implements the **ASYMMETRIC
filter** (admit iff `?engine >= min_engine`, else a REASONED rejection — `426` too-old, `400` malformed/bad-`?engine`/
bad-`?next`, via `axum` `(StatusCode, body).into_response()` returned verbatim on `Err`) AND stashes `?next`. A plain
path (no `~`) is a legacy room (no engine gate). **`NextTopology` (ADR-0039)** re-implements matchbox `FullMesh`'s
relay (via `common_logic::{parse_request, try_send}`) GENERALIZED with `?next=N` session-SIZE grouping: `?next` absent
⇒ one unbounded session per room (FullMesh-equivalent; session key = the path); `?next=N` ⇒ the room subdivides into
sessions keyed `"<room>#<index>"`, each capped at N, **batch-deal / no-backfill** (a session seals at N and never
refills). The topology can't see the query, so `?next` is stashed by the gate (`origin`) → bridged to `PeerId` at
`on_id_assignment` → consumed in the topology's `join`. The `SessionRegistry` IS the topology's shared
`SignalingState` — it holds the relay senders + grouping bookkeeping + stashed `?next`, and lists sessions
(`list()`/`peer_count()`/`session_count()`; keys are session keys). Join/leave bookkeeping is INLINE in the topology
(a custom topology has no `on_peer_connected/on_peer_disconnected`); lock discipline = collect senders under the
`Mutex`, drop it, then `try_send`. `parse_scope`/`parse_engine`/`parse_next` + `Scope`/`Mode` (signaling-local tag).
Binary = thin wrapper (port `UNIBLOX_SIGNALING_PORT`/3536, `RUST_LOG` tracing). 5 unit + 16 raw-WS integration tests.

**Trust model:** `?engine` + path `min` are self-declared — desync defense for HONEST clients, not anti-cheat (a
modified client can lie, but can't lie into a *stricter* room with an old engine: a different `min` is a different
room).

## Crate-local invariants
- **DONE (ADR-0037/0038/0039):** `{mode, version}` scoping via the room path + the connection gate + the session
  registry; the **ASYMMETRIC version filter** (admit `engine >= min`, content + schema EXACT, reasoned `426`/`400`);
  and **`?next=N` session-SIZE grouping** via the custom `NextTopology` (batch-deal / no-backfill; unbounded when
  `?next` absent). The relay is re-implemented from FullMesh's contract — the ADR-0037/0038 tests double as its
  regression proof on the unbounded path.
- **DEFERRED (later Phase-5 bullets):** the Mode-2 coordinator peer service; stateless nodes + a shared
  Redis/Postgres registry for horizontal scale; rate-limit + authenticate room join (signaling-DoS, Phase 11).
- Scoping is structural ISOLATION, not access control (no room secret; a client may name any path — auth is
  the deferred bullet).

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
