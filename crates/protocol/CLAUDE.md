# CLAUDE.md — `protocol`

**Purpose:** shared wire types — protocol versions, message enums, content IDs.
**Risk tier:** standard.

## Status
The replication wire format lives here (ADR-0013): `PeerId` (+serde, `from_uuid_bytes` — a PURE
function of the transport UUID; all peers must agree), spawner-stable `NetEntityId` (now also `Ord`,
`(spawner,index,generation)` — the ADR-0021 sender emits Spawns/state/despawns in this order for
DETERMINISTIC per-peer wire output; the ordering has no wire meaning), quantization
(`QUANT_SCALE`=1024, tolerance ≤1/2048 for |v|≤16384, saturating), `StateMsg`/`StateEntry`
(Options-only presence, derived mask, ABSOLUTE values — never arithmetic deltas; `StateMsg` also carries
`tick` — the interpolation time axis — and `last_input` — the reconciliation marker, ADR-0022), `EventMsg` with
the reserved-but-None signature field (Phase 6) + the `NetEvent::Input{seq,intent}` client-input variant
(ADR-0022 Stage B, reliable channel) + the **ADR-0024 anti-entropy resync** variants `NetEvent::{Digest{entries:
Vec<DigestEntry>}, ResyncRequest{ids}, ResyncSpawn{id,pos,vel,seq}}` (`DigestEntry{id, state_hash: Option<u32>}`;
all reliable, directed) + the **ADR-0025 A ownership-arbitration rank** `OwnerSeq{seq:u64, coordinator:PeerId}`
(lexicographic `Ord` — `seq` dominant, `coordinator` breaks equal-seq ties toward the higher id) which now rides
`OwnershipTransfer{id,new_owner,seq}` AND `ResyncSpawn` (A-kernel) + the **coordinator PULL handshake**
`NetEvent::{ClaimOwnership{id}, OwnershipCommit{id,new_owner,seq}, ClaimRejected{id}}` (A-handshake), versioned
postcard codecs (mismatch → clean Err). **`WIRE_VERSION`=7.** **Content-addressing (ADR-0032):**
`ContentId([u8;32])` = the blake3-256 digest of a byte blob (`content_id()`, `to_hex`/`from_hex` round-trip,
`ContentIdError`, `Ord` for deterministic content-store iteration) — THE primitive for the Phase-4 Mode-1 save
(B2) + Phase-7 object storage + Phase-8 publish. `blake3` is pinned `{default-features=false, features=["std",
"pure"]}` (already in the lock via `bevy_asset`; `pure` = no `cc`/C toolchain, wasm-safe). The reserved
`VersionTriple {engine, content, schema}` is a forward hook (a `pub`, not-yet-consumed type) — Phase 5 enforces
the `{engine, content, schema}` triple at session join, and a save blob carries `Option<VersionTriple>`=`None`
today so enabling it needs no shape change.

## Crate-local invariants
- The `{engine, content, schema}` version triple lives here; it is the desync
  defense (matched at session join, Phase 5).
- Wire types are shared by `replication`, `transport`, `client`, `server` — a
  change here ripples everywhere; keep it minimal and versioned.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`. Do not
relitigate settled decisions — record new ones in `../../DECISIONS.md`.
