# CLAUDE.md — `replication`  ⚠️ HIGH-RISK

**Purpose:** the custom replication protocol — wire format, quantization,
delta/baseline, authority, ownership handoff, anti-entropy resync.
**Risk tier:** **HIGH-RISK.** Plan-mode-first, TDD (human specifies cases),
`netcode-auditor` on every diff (never the session that wrote the code).

## Status
Stub (Phase 1.1). No functional code yet.

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
