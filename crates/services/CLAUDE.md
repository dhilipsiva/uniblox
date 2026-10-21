# CLAUDE.md — `services`

**Purpose:** signaling / session-registry / matchmaking WebSocket service
(required even for free tiers). Becomes a binary in Phase 5.
**Risk tier:** standard (LOW — delegate; host-migration election is MIXED, human-review).

## Status
**Scoped signaling with the asymmetric filter + `?next=N` grouping (ADR-0037/0038/0039) — library + binary.**
The service is its OWN axum server (`server.rs`, ADR-0047) speaking matchbox's wire protocol (`matchbox_protocol`
`JsonPeerRequest`/`JsonPeerEvent`), so matchbox clients connect unchanged; matchbox_signaling is no longer used.
`build_signaling_server(addr, registry)` (open) / `build_signaling_server_with(.., SignalingConfig)` return a
`SignalingServer` with matchbox's `bind`/`serve` API. Rooms are the URL PATH `<mode>~<content>.<schema>~<min>~<lobby>`
(e.g. `m1~7.2~3~arena`); the client's OWN engine rides `?engine=N`. Isolation is strictly by the path string, so
content/schema/min/lobby are EXACT-match structural while the engine (out of the key) lets compatible versions share
ONE session. The gate (`Shared::gate`) applies, in order: the per-IP attempt rate (429), the room (`validate_room`,
400), the join token (401), the ASYMMETRIC filter (`426` too-old, `400` malformed/bad-`?engine`), the `?next` and
session caps (400/503), then a connection slot (429 per IP, 503 total). A plain path (no `~`) is a legacy room. The
relay (`run_peer`/`relay`) is matchbox `FullMesh`'s contract GENERALIZED with `?next=N` session-SIZE grouping:
absent ⇒ one unbounded session per room; `?next=N` ⇒ sessions keyed `"<room>#<index>"`, each capped at N,
**batch-deal / no-backfill**. `?next` goes straight from the gate to `join_local`. The `SessionRegistry` holds each
member's bounded `PeerTx` (queue + disconnect switch) and the grouping bookkeeping, and lists sessions
(`list()`/`peer_count()`/`session_count()`). Lock discipline: collect `PeerTx`s under the `Mutex`, drop it, then
send. `parse_scope`/`parse_engine`/`parse_next` + `Scope`/`Mode` (signaling-local tag). Binary = thin wrapper (port
`UNIBLOX_SIGNALING_PORT`/3536, `RUST_LOG` tracing, warnings by default).

**Authentication and limits (ADR-0047, proposed):** `build_signaling_server_with(addr, registry, SignalingConfig)`.
With `auth: Some(JoinTokenKey)` the gate admits only a valid `?token=` for the requested room path (HMAC-SHA256
`v1.<expiry>.<tag>`, at most a day, 401 with a reason otherwise); `issue_tokens` serves `GET /_uniblox/token?room=`.
`SignalingLimits`: a per-IP bucket for connection attempts and token requests (429; IPv4-mapped as IPv4, IPv6 per
/64; `ClientIp::ForwardedFor(hops)` behind proxies; bounded table, fail closed), 16 open connections per IP and
10,000 in total, a 32 KiB WebSocket message/frame cap enforced as fragments arrive, a 60 s idle timeout, a
per-connection frame bucket (1,024 burst, 100/s; close after 256 net drops), a 256-message outbound queue and a 1 MiB
write buffer per peer (a non-reader is disconnected), and 64 peers per session. The binary refuses
to start without `UNIBLOX_SIGNALING_SECRET` unless `UNIBLOX_SIGNALING_INSECURE=1`; `services mint-token <room>`.
`build_signaling_server` (no config) stays open for tests and local development.

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

**Registry liveness (ADR-0048, proposed):** each node is alive while its liveness entry (Redis
`uniblox:node:<node>`, a TTL set by every join and heartbeat) has not expired; the reads count only live nodes and
`RegistryStore::sweep` deletes dead nodes' sessions. `SessionRegistry::spawn_heartbeat` (started by
`build_signaling_server_with` under a runtime) beats + sweeps every TTL/3 (default 30 s). Redis join = one `MULTI`,
leave = one Lua script, sweep = one `MULTI` per dead session.

## Crate-local invariants
- **DONE (ADR-0037/0038/0039/0040):** `{mode, version}` scoping + the connection gate + the session registry; the
  **ASYMMETRIC version filter** (admit `engine >= min`, content + schema EXACT, reasoned `426`/`400`); **`?next=N`
  session-SIZE grouping** in the relay (batch-deal / no-backfill); and **horizontal scale** — the
  `RegistryStore` split + `RedisRegistryStore` (two stateless nodes share one Redis registry). The relay is
  re-implemented from FullMesh's contract — the ADR-0037/0038 tests double as its regression on the unbounded path.
- **DEFERRED (later Phase-5 / Phase-11):** the Mode-2 coordinator peer service; cross-node relay (sticky routing
  assumed); autoscale-under-load.
- Scoping is structural ISOLATION; access control is the join token (ADR-0047), a bearer credential for one room
  until its expiry, not an identity.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
