# CLAUDE.md — `replication`  ⚠️ HIGH-RISK

**Purpose:** the custom replication protocol — wire format, quantization,
delta/baseline, authority, ownership handoff, anti-entropy resync.
**Risk tier:** **HIGH-RISK.** Plan-mode-first, TDD (human specifies cases),
`netcode-auditor` on every diff (never the session that wrote the code).

## Status
Implemented (ADR-0013 slice + ADR-0020 delta + **ADR-0021 interest management / per-peer** + **ADR-0023
stage a**). The sender is now PER-PEER: `collect_all(world) -> Vec<(PeerId, Outbox)>` (was broadcast
`collect`). **ADR-0023 (a) quantization hoist:** the once-per-tick `owned` snapshot carries a precomputed
`OwnedRow { id, qpos, qvel }` (quantized ONCE from the peer-invariant raw pos/vel); the per-peer loop reads
`row.qpos`/`row.qvel` instead of re-quantizing per (peer,entity). Byte-identical; the transfer-Spawn path
(entity absent from `owned` post-authority-gate) still reads `world.get` directly. **ADR-0023 (b) AOI-flicker
hysteresis:** `Aoi` has a two-radius band (`radius_inner`/`radius_outer`) — enter at `dist ≤ r_inner`, exit at
`dist > r_outer`; `collect_all` derives `visible_outer` (exit/state/unbounded) + `visible_inner` (enter). A
band entity never inside `r_inner` is still withheld (read-cheat). `set_aoi` = degenerate band
(`inner==outer`, keeps A–H green); `set_aoi_hysteresis` sets a real band (release fail-safe clamps an inverted
band to the single radius). Each tracked peer sees
only entities in its AOI (`set_aoi`; unset ⇒ unbounded/sees-all — a FAIL-OPEN bandwidth default, not a
security guarantee), with its OWN delta baseline (`send_state[peer][entity]`) + seq stream + `known` set.
Out-of-AOI entities are withheld in BOTH state AND existence (spawn-on-enter / despawn-on-exit) — the
read-cheat defense. Per-peer order is load-bearing: **dead → transfer → exit → enter → state** (dead wins
over transfer; exit drops the baseline so re-enter re-baselines; enter Spawns only `spawner==local`, an
adopted entity is stated no-Spawn; the id-map prunes only after all peers are told). Wire output is
DETERMINISTIC (emissions sorted by `NetEntityId`, peers by `PeerId`). Still: cached-`SystemState` behind the
`authority_of` gate (NO `Changed` filter — grep-auditable); acked-baseline delta (`decide_component`, `acked
>= run_start`, decide/commit seq-consumption); >1150B datagram warn. Receiver UNCHANGED (newest-seq LWW +
`applied_seq` F1 split; full-`NetEntityId` keying; current-Owner validity; `authority_of == Remote` gate;
`drain_acks` → directed `NetEvent::Ack`). **ADR-0022 Stage A (interpolate-others):** `collect_all` stamps
`StateMsg.tick`; `apply_state` additionally pushes a snapshot into the proxy's `InterpBuffer` (a pure
side-record — authoritative `Position` snap-apply unchanged); the Spawn handler attaches an `InterpBuffer` to
new proxies. Interpolation itself lives in engine-core (`RenderPos`/`interpolate`). **Stage B (predict/reconcile):**
`apply_events` gains an `Input` arm (server queues into `PendingInputs`); `drain_inputs` (client sends un-sent
`InputHistory` to the avatar's authority); `apply_state` prunes `InputHistory` by `StateMsg.last_input`
(reconciliation); `collect_all` stamps per-peer `last_input` from `ProcessedInput`. The predicted avatar is
`Remote` ⇒ the authority gate already excludes it (client emits inputs only, never state). **Stage C
(handoff):** the `OwnershipTransfer` handler flushes the proxy's `InterpBuffer` on any authority change (source
discontinuity); `drain_inputs` sends only for `authority==Remote` entities (no self-directed inputs). The
role transition itself is engine-core's `reset_render_role`. Handoff: local Owner flips immediately (no double-authority window).
`interest` submodule = `SpatialGrid` (cell-bucketed, floor-celled, exact-dist² filter) + `Aoi`. **46-test
two-World battery + 5 grid unit tests + codec + e2e-over-real-transport green; netcode-audited THREE times (F1
orphan blocker + its over-broad fix, both closed → MERGE).** The ack round-trip is now
integration-covered over the real `net_pump` (both directions — `server/tests/headless_app.rs::
ack_round_trip_confirms_and_goes_quiet`, a Mode-2-shaped client-owned entity drives the server's ack-routing).
**ADR-0024 handoff depth + anti-entropy RESYNC:** deep handoff is now covered (hand-back A→B→A, repeated/cycle
transfers, packet-loss around a handoff — Group R in two_world), and the documented R6 cross-sender reordering
gap (a frozen wrong-owner proxy) now HEALS via resync: `collect_resync` (per-peer `Digest` of owned+known
ids + a confirmed-value hash) → receiver flags divergence (missing / wrong-owner / stale-hash) → directed
`ResyncRequest` (`drain_resync_requests`) → the owner's privileged `ResyncSpawn` (`drain_resync_responses`,
re-filtered by current-ownership + AOI) which create-or-corrects the proxy (own-authority guard; bypasses the
`owner!=from` / `spawner!=from` gates as the current authority).
**ADR-0025 host-migration + ownership-seq arbitration:** **Stage B** — `reassign_orphans(world, departed)`
re-tags a dropped owner's entities to `elect_owner(peers ∪ local)` (lowest live id), pure-local, no wire,
rank-PRESERVING (closes the ADR-0024 E4 orphan). **Stage A-kernel** — a per-entity monotonic
`OwnerSeq{seq,coordinator}` (`NetIdRecord.owner_seq`, seeded `{0,spawner}`) is now the arbiter for EVERY owner
change: `transfer_ownership` mints `{prev.seq+1, coordinator:local}`, and the `OwnershipTransfer` apply gate is
**`seq > rec.owner_seq` (strict), REPLACING the old `owner!=from` check** — so the R6 cross-sender reorder now
RESOLVES BY RANK at the source (no freeze; the resync's R6-freeze-heal role is retired — its residual role is the
stale-silent-value heal, a LOST-transfer wrong-owner proxy, orphan refetch, and E4). `ResyncSpawn` is now
seq-gated too (own-authority guard → same-owner value-heal accept-regardless → owner-change `>=` heal →
orphan-create adopts the rank), which CLOSES the stale-former-owner backdoor. The `>=`(resync)/`>`(transfer)
asymmetry is deliberate + auditor-verified. `owner_seq(entity)` is a white-box test accessor. The STATE owner
gate (`apply_state`, `owner!=from`) is UNCHANGED. Phase 3 still owns: message splitting, per-entry ack
granularity, the **A-handshake** claim/commit/reject PULL path (`ClaimOwnership` → coordinator-arbitrated
`OwnershipCommit`/`ClaimRejected`), and the deferred consistent-membership (`net_pump` Disconnected) wiring that
Stage B's exactly-once reassignment relies on.

## Crate-local invariants
- **Single-ownership per entity ⇒ last-write-wins, NO CRDT.** One authority per
  entity; no concurrent writes ⇒ nothing to merge.
- **No cross-platform float determinism.** Receivers apply + interpolate others'
  state; prediction only touches entities you own. Never re-simulate others.
- **Carry the Bevy entity generation (u32)** alongside the index — indices are
  recycled; reject state addressed to a stale generation.
- **Custom protocol, not lightyear/replicon/renet.**
- Reserve the signature field in the wire format, but do NOT functionally sign
  in the slice — signing is Phase 6; the slice only measures sign/verify cost.

## Rules
"Compiles but subtly wrong" is the dominant risk here — neither the compiler nor
clippy catch it. TDD + a fresh auditor are mandatory. Inherit all root invariants
from `../../CLAUDE.md`.
