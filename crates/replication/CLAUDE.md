# CLAUDE.md — `replication`  ⚠️ HIGH-RISK

**Purpose:** the custom replication protocol — wire format, quantization,
delta/baseline, authority, ownership handoff, anti-entropy resync.
**Risk tier:** **HIGH-RISK.** Plan-mode-first, TDD (human specifies cases),
`netcode-auditor` on every diff (never the session that wrote the code).

## Status
Implemented (the slice protocol, ADR-0013). Sender: cached-`SystemState` change masks behind the
`authority_of` gate (NO `Changed` filter anywhere — grep-auditable), keyframe every 30 collects,
same-tick transfer+despawn purge (owned-ghost guard), >1150B datagram warn. Receiver: newest-seq
LWW, full-`NetEntityId` keying (stale generations inert), current-Owner sender validity,
`authority_of == Remote` apply-gate, snap-apply. Handoff: local Owner flips when the reliable
Transfer is queued (no double-authority window). 27-test battery (codec / two-World / e2e-over-
real-transport) green; netcode-audited (findings fixed or documented). Phase 3 owns: acked
baselines/arithmetic deltas, interpolation buffers, resync, message splitting, peer-departure
cleanup, and the documented cross-sender handoff-reordering gaps (see lib.rs module docs).

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
