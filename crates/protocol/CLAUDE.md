# CLAUDE.md — `protocol`

**Purpose:** shared wire types — protocol versions, message enums, content IDs.
**Risk tier:** standard.

## Status
The replication wire format lives here (ADR-0013): `PeerId` (+serde, `from_uuid_bytes` — a PURE
function of the transport UUID; all peers must agree), spawner-stable `NetEntityId`, quantization
(`QUANT_SCALE`=1024, tolerance ≤1/2048 for |v|≤16384, saturating), `StateMsg`/`StateEntry`
(Options-only presence, derived mask, ABSOLUTE values — never arithmetic deltas), `EventMsg` with
the reserved-but-None signature field (Phase 6), versioned postcard codecs (mismatch → clean Err).
The `{engine, content, schema}` version triple lands in Phase 5.

## Crate-local invariants
- The `{engine, content, schema}` version triple lives here; it is the desync
  defense (matched at session join, Phase 5).
- Wire types are shared by `replication`, `transport`, `client`, `server` — a
  change here ripples everywhere; keep it minimal and versioned.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`. Do not
relitigate settled decisions — record new ones in `../../DECISIONS.md`.
