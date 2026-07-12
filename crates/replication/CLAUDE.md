# CLAUDE.md — `replication`  ⚠️ HIGH-RISK

**Purpose:** the custom replication protocol — wire format, quantization,
delta/baseline, authority, ownership handoff, anti-entropy resync.
**Risk tier:** **HIGH-RISK.** Plan-mode-first, TDD (human specifies cases),
`netcode-auditor` on every diff (never the session that wrote the code).

## Status
Implemented (ADR-0013 slice + ADR-0020 delta + **ADR-0021 interest management / per-peer**). The sender is
now PER-PEER: `collect_all(world) -> Vec<(PeerId, Outbox)>` (was broadcast `collect`). Each tracked peer sees
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
orphan blocker + its over-broad fix, both closed → MERGE).** Phase 3 still owns: interpolation buffers,
anti-entropy resync, message splitting, per-entry ack granularity, hysteresis for AOI flicker, a
client-acks-server integration test (pre-Mode-2), and the documented cross-sender gaps (see lib.rs module
docs) — including a chained handoff to a never-witnessed new owner of an adopted entity.

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
