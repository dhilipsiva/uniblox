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

## Horizontal scale (ADR-0040)
`SessionRegistry` is split: the **node-local relay** (`RegistryInner` — sessions-with-senders + `?next` grouping,
unchanged) drives the WebRTC relay; a shared **`RegistryStore`** (`#[async_trait]`, fallible) MIRRORS session→peer
membership so many STATELESS nodes share one listing. `SessionRegistry { local, store, node_id }`; `new()` = in-memory
single-node, `with_store(store, node_id)` = shared. `join`/`leave` are async — the local work runs under the lock, the
guard is DROPPED, THEN the store is mirrored (no `MutexGuard` across `.await`; the `state_machine` future stays `Send`);
a store failure is best-effort-logged, never breaks the relay. The sync `list`/`peer_count`/`session_count` are THIS
node's view; the async `global_*` read the shared store. Stores: `MemoryRegistryStore` (default; shared = the two-node
test double) + `RedisRegistryStore` (redis-rs; `uniblox:sess:<key>` member SET + `uniblox:sessions` index SET;
SADD/SREM/SCARD, de-index on empty). The binary opts in via `UNIBLOX_REDIS_URL` (+ `UNIBLOX_NODE_ID`). **Only the
LISTING is shared — the relay is node-local, so a session's two peers must be on the same node (sticky routing;
cross-node relay is out of scope).** Hermetic test spawns a real `redis-server` (flake `pkgs.redis`, coturn precedent).

## Crate-local invariants
- **DONE (ADR-0037/0038/0039/0040):** `{mode, version}` scoping + the connection gate + the session registry; the
  **ASYMMETRIC version filter** (admit `engine >= min`, content + schema EXACT, reasoned `426`/`400`); **`?next=N`
  session-SIZE grouping** via the custom `NextTopology` (batch-deal / no-backfill); and **horizontal scale** — the
  `RegistryStore` split + `RedisRegistryStore` (two stateless nodes share one Redis registry). The relay is
  re-implemented from FullMesh's contract — the ADR-0037/0038 tests double as its regression on the unbounded path.
- **DEFERRED (later Phase-5 / Phase-11):** the Mode-2 coordinator peer service; cross-node relay (sticky routing
  assumed); atomic `MULTI`/Lua store ops + TTL/heartbeat crash-cleanup + autoscale-under-load; rate-limit +
  authenticate room join (signaling-DoS, Phase 11).
- Scoping is structural ISOLATION, not access control (no room secret; a client may name any path — auth is
  the deferred bullet).

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
