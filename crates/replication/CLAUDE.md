# CLAUDE.md — `replication`  ⚠️ HIGH-RISK

**Purpose:** the custom replication protocol — wire format, quantization,
delta/baseline, authority, ownership handoff, anti-entropy resync.
**Risk tier:** **HIGH-RISK.** Plan-mode-first, TDD (human specifies cases),
`netcode-auditor` on every diff (never the session that wrote the code).

## Status
Implemented (ADR-0013 slice + ADR-0020 delta baseline). Sender: cached-`SystemState` behind the
`authority_of` gate (NO `Changed` filter anywhere — grep-auditable), **acked-baseline delta** — a
component is sent while its QUANTIZED value differs from the per-entity baseline OR that value is not
yet acked by every tracked peer (contiguous-run cumulative-ack; the fixed keyframe is GONE), decide/
commit split so an empty tick consumes no seq, same-tick transfer+despawn purge (owned-ghost guard),
>1150B datagram warn. Receiver: newest-seq LWW (`last_seq` "seen"), SEPARATE `applied_seq` "fully
applied" high-water that drives acks (F1: never ack a value we dropped), full-`NetEntityId` keying
(stale generations inert), current-Owner sender validity, `authority_of == Remote` apply-gate,
snap-apply, `drain_acks` → directed `NetEvent::Ack`. Handoff: local Owner flips when the reliable
Transfer is queued (no double-authority window). 28-test two-World battery + codec + e2e-over-real-
transport green; netcode-audited twice (F1 blocker fixed → MERGE). Phase 3 still owns: interpolation
buffers, anti-entropy resync, message splitting, per-entry ack granularity, peer-departure cleanup,
a client-acks-server integration test (pre-Mode-2), and the documented cross-sender handoff-reordering
gaps (see lib.rs module docs).

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
