//! `protocol` — shared wire types: peer identity, network entity identity,
//! quantization, and the replication messages (ADR-0013).
//!
//! Encoding is postcard (varint ints, wasm-safe, not self-describing — hence
//! the leading `version` byte on every top-level message). The state channel
//! carries **presence-deltas with ABSOLUTE quantized values** — a component is
//! present iff it changed, but its value is always the full quantized state,
//! NEVER an arithmetic delta (the channel is lossy; arithmetic deltas would
//! compound loss into permanent drift — Phase 3's acked baselines own that).

use serde::{Deserialize, Serialize};

/// Wire-format version, checked on decode. Bump on ANY message-shape change.
/// v6 (ADR-0025 A-handshake / Phase 3): the coordinator PULL handshake —
/// `NetEvent` gains `ClaimOwnership`, `OwnershipCommit`, and `ClaimRejected`.
/// v5 (ADR-0025 A-kernel / Phase 3): double-ownership arbitration by coordinator
/// sequence number — a per-entity monotonic [`OwnerSeq`] rides every
/// owner-mutating event: `OwnershipTransfer` and `ResyncSpawn` gain a `seq`
/// field. v4 (ADR-0024):
/// anti-entropy resync — `NetEvent` gains `Digest` (a sender's per-peer summary
/// of the entities it owns, for divergence detection), `ResyncRequest` (a
/// receiver asks the owner to re-assert ids it diverged on), and `ResyncSpawn`
/// (the owner's privileged create-or-correct that heals a frozen wrong-owner /
/// orphaned proxy). v3 (ADR-0022): `StateMsg` gains `tick` + `last_input`. v2
/// added `NetEvent::Ack`. Pre-release hard cutover.
pub const WIRE_VERSION: u8 = 6;

/// Fixed-point quantization scale: world units × 1024.
///
/// Envelope (documented, asserted by tests): round-trip error ≤ 1/2048 for
/// |v| ≤ 16384.0 (beyond that, f32 ULP of `v*1024` exceeds 1.0 and the bound
/// degrades); values saturate at ±(2^31/1024) ≈ ±2,097,152 units.
pub const QUANT_SCALE: f32 = 1024.0;

/// Identity of a peer (player instance or server) in a session.
///
/// Shared by `engine-core` (ownership tags), `transport`, and `replication`.
/// `Ord` is deliberate: host-migration election (Phase 3/5) tiebreaks on the
/// lowest peer ID.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub u64);

impl PeerId {
    /// Deterministic map from a transport-level UUID (matchbox PeerId bytes)
    /// to the protocol id: the FIRST 8 BYTES, big-endian.
    ///
    /// This MUST remain a pure function of the UUID: every peer must derive
    /// the same u64 for a given peer, or `Owner` comparisons diverge per
    /// machine. (Truncation collision odds are negligible at mesh scale;
    /// interim until Phase 5 session join assigns canonical ids.)
    pub fn from_uuid_bytes(bytes: [u8; 16]) -> PeerId {
        let mut first8 = [0u8; 8];
        first8.copy_from_slice(&bytes[..8]);
        PeerId(u64::from_be_bytes(first8))
    }
}

/// Network identity of a replicated entity — STABLE for the entity's lifetime.
///
/// Minted exactly once by the spawner from its local Bevy `Entity`
/// (`index_u32()`, `generation().to_bits()`) and never changes, INCLUDING
/// across ownership transfers: identity ≠ authority. Current authority lives
/// only in the proxy's `Owner` component, mutated only by reliable
/// `OwnershipTransfer` events. Receivers never reconstruct a foreign `Entity`
/// — they map `NetEntityId` → local proxy. (Bevy's `Entity::to_bits` u64
/// layout is opaque and never shipped.)
/// `Ord` is deliberate: the replication sender emits Spawns / state entries /
/// despawns in `NetEntityId` order so the per-peer wire output is DETERMINISTIC
/// (independent of HashSet/HashMap iteration seed) — reproducible captures and
/// stable tests. Ordering is `(spawner, index, generation)`; it has no wire
/// meaning (the format is unchanged).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NetEntityId {
    pub spawner: PeerId,
    pub index: u32,
    pub generation: u32,
}

/// Monotonic ownership sequence — the arbiter for concurrent / reordered
/// ownership changes (ADR-0025 A). Anchored PER ENTITY: every owner-mutating
/// event ([`NetEvent::OwnershipTransfer`], [`NetEvent::OwnershipCommit`],
/// [`NetEvent::ResyncSpawn`]) carries the `OwnerSeq` it establishes, and a
/// receiver accepts an owner change only when it OUTRANKS the entity's current
/// one — so a cross-sender-reordered stale assertion is dropped by rank, never
/// by freezing (this is what resolves the ADR-0024 R6 gap at the source).
///
/// `Ord` is LEXICOGRAPHIC by field order: `seq` first (higher wins), then
/// `coordinator` (higher peer id wins) to break equal-`seq` ties DETERMINISTICALLY.
/// The tiebreak is load-bearing for the CLAIM COORDINATOR (the lowest-live-peer
/// arbiter of ownership claims, Stage A-handshake): when that coordinator departs,
/// the next lowest — a HIGHER peer id — takes over, and if a partial commit
/// delivery leaves peers split between the old and new coordinator's equal-`seq`
/// commits, higher-wins routes convergence to the NEWER coordinator's decision.
/// (Host-migration REASSIGNMENT of a dropped owner's entities is a separate,
/// rank-PRESERVING, pure-local operation — see `replication::reassign_orphans`;
/// it deliberately does not mint a new rank, so a real transfer/commit always
/// outranks it and the `>=` resync gate can re-affirm the reassignment to a
/// non-witness.) Seeded `{0, spawner}` at birth: a PURE function of the id, so
/// every peer agrees on the baseline rank.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OwnerSeq {
    pub seq: u64,
    pub coordinator: PeerId,
}

/// A quantized 2D vector (position or velocity): i32 fixed-point at
/// [`QUANT_SCALE`]. Quantization happens ONLY on the sender; receivers
/// dequantize — no cross-platform float agreement is ever required.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct QVec2 {
    pub x: i32,
    pub y: i32,
}

/// Quantize one component. Saturates at i32 range; NaN maps to 0 (a NaN
/// position is an upstream sim bug — the wire refuses to amplify it).
pub fn quantize(v: f32) -> i32 {
    debug_assert!(v.is_finite(), "quantizing a non-finite value: {v}");
    let scaled = (v * QUANT_SCALE).round();
    if scaled.is_nan() { 0 } else { scaled as i32 }
}

/// Dequantize one component.
pub fn dequantize(q: i32) -> f32 {
    q as f32 / QUANT_SCALE
}

/// Quantize an (x, y) pair.
pub fn quantize_vec2(x: f32, y: f32) -> QVec2 {
    QVec2 {
        x: quantize(x),
        y: quantize(y),
    }
}

/// One sender's atomic snapshot of its changed authoritative entities at one
/// network tick. Sent on the UNRELIABLE state channel; receivers apply
/// last-write-wins by `seq` (newest seq wins, NOT latest arrival — the channel
/// is unordered).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StateMsg {
    pub version: u8,
    /// Per-SENDER monotonic, starting at 1 (0 = receiver's "nothing seen").
    pub seq: u64,
    /// The authoritative sim tick this snapshot was sampled at (ADR-0022). The
    /// receiver's interpolation buffer keys on this — a uniform, loss-immune,
    /// deterministic time axis (unlike arrival time or the delta-warped `seq`).
    pub tick: u64,
    /// The recipient's newest input seq reflected in these entries (ADR-0022
    /// reconciliation marker). Per-peer, since `collect_all` is per-peer: "your
    /// inputs through this seq are applied here." 0 = none / no input from them.
    pub last_input: u64,
    pub entries: Vec<StateEntry>,
}

/// Per-entity state: a component is present iff it differs from the sender's
/// per-peer baseline OR that baseline is not yet acked by every peer (ADR-0020
/// delta; the fixed keyframe is gone). Values are ABSOLUTE.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateEntry {
    pub id: NetEntityId,
    pub pos: Option<QVec2>,
    pub vel: Option<QVec2>,
}

impl StateEntry {
    /// The changed-component bitmask, DERIVED from Option presence (bit0 =
    /// position, bit1 = velocity) — computed from the single source of truth,
    /// so it cannot disagree with the payload.
    pub fn mask(&self) -> u8 {
        u8::from(self.pos.is_some()) | (u8::from(self.vel.is_some()) << 1)
    }
}

/// One entry in an anti-entropy [`NetEvent::Digest`] (ADR-0024): a sender
/// summarizing an entity it currently owns. The owner is IMPLICIT — the digest
/// sender. `state_hash` is `Some(fnv32(qpos, qvel))` ONLY for an entity that is
/// confirmed + quiet for the recipient (so a divergence on an otherwise-silent
/// value is still detectable); `None` for an entity still active in the delta
/// stream (an owner mismatch is caught without a hash).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigestEntry {
    pub id: NetEntityId,
    pub state_hash: Option<u32>,
}

/// A durable event on the RELIABLE, ORDERED events channel. No seq field —
/// SCTP ordering is the mechanism (adding one would invite misuse).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EventMsg {
    pub version: u8,
    /// RESERVED for Phase 6 ed25519 op-signing. Always `None` in the slice —
    /// present so the wire format does not change when signing lands.
    pub sig: Option<Vec<u8>>,
    pub event: NetEvent,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum NetEvent {
    /// Introduce an entity. Only the minting peer (`id.spawner`) may send this.
    Spawn {
        id: NetEntityId,
        pos: QVec2,
        vel: QVec2,
    },
    /// Remove an entity. Only the entity's CURRENT owner may send this.
    Despawn { id: NetEntityId },
    /// Transfer authority (a PUSH — the current owner hands the entity off).
    /// Identity (`id`) never changes — only the proxy's `Owner` does. `seq` is
    /// the fresh [`OwnerSeq`] this transfer establishes (`{prev.seq + 1,
    /// coordinator: giver}`); a receiver accepts only if it OUTRANKS the proxy's
    /// current seq, so the `owner!=from` check is no longer the gate (ADR-0025 A).
    OwnershipTransfer {
        id: NetEntityId,
        new_owner: PeerId,
        seq: OwnerSeq,
    },
    /// Delta-baseline acknowledgement (ADR-0020, Phase 3): "I have applied up
    /// to state `seq` of YOUR stream." Sent DIRECTED (receiver → the acked
    /// sender), on the reliable channel, so the sender can advance its
    /// per-peer confirmed baseline and stop re-sending confirmed values.
    /// Ephemeral bookkeeping — never signed (`sig` stays `None`).
    Ack { seq: u64 },
    /// A client input command for its controlled avatar (ADR-0022 Stage B):
    /// sent DIRECTED (client → the avatar's authority) on the RELIABLE channel,
    /// so the authority processes each input once, in order (`last_input`
    /// advances contiguously). `seq` is per-controlled-entity monotonic;
    /// `intent` is the quantized desired velocity.
    Input { seq: u64, intent: QVec2 },
    /// Anti-entropy DIGEST (ADR-0024): the sender summarizes the entities it
    /// currently owns for ONE recipient, so the recipient can detect a diverged
    /// proxy (missing, frozen at a wrong owner after a cross-sender reorder, or a
    /// stale silent value) and request a resync. Directed, per-peer. Ephemeral.
    Digest { entries: Vec<DigestEntry> },
    /// Anti-entropy RESYNC REQUEST (ADR-0024): the recipient of a `Digest` asks
    /// the sender (the current owner) to re-assert these ids it diverged on.
    /// Directed (receiver → owner). Ephemeral.
    ResyncRequest { ids: Vec<NetEntityId> },
    /// Anti-entropy RESYNC SPAWN (ADR-0024): the current owner's PRIVILEGED
    /// create-or-correct. Re-asserts an entity's existence, owner (the sender),
    /// and state — healing a frozen wrong-owner proxy (owner override) or an
    /// orphan (create). No owner field: the sender IS the asserted owner
    /// (identity is not authority; `id.spawner` is unchanged). `seq` is the
    /// entity's CURRENT [`OwnerSeq`] as the owner holds it (ADR-0025 A) — an
    /// owner-change heal is gated `seq >= proxy.seq` (a strictly-lower stale
    /// former-owner assert is dropped); a same-owner value heal ignores it.
    /// Reliable, directed.
    ResyncSpawn {
        id: NetEntityId,
        pos: QVec2,
        vel: QVec2,
        seq: OwnerSeq,
    },
    /// CLAIM ownership of an entity you do NOT currently own (ADR-0025
    /// A-handshake — the Mode-2 PULL). Directed to the COORDINATOR (the lowest
    /// live peer id). It flips NO `Owner` anywhere — the claimant assumes nothing
    /// until it receives an [`NetEvent::OwnershipCommit`] naming it. Reliable.
    ClaimOwnership { id: NetEntityId },
    /// The coordinator's arbitrated GRANT of a claim (ADR-0025 A-handshake):
    /// `new_owner` (the lowest-id claimant) wins the entity at `seq` — a fresh
    /// [`OwnerSeq`] `{prev.seq + 1, coordinator: self}`. Sent to every peer that
    /// must re-tag (the claimants, the demoting prior owner, the coordinator's own
    /// AOI-knowers); gated `seq > proxy.seq`, identical to a transfer. Reliable.
    OwnershipCommit {
        id: NetEntityId,
        new_owner: PeerId,
        seq: OwnerSeq,
    },
    /// The coordinator tells a LOSING claimant its claim did not win (ADR-0025
    /// A-handshake). No state change (the claimant never pre-flipped); the pump
    /// may re-claim. Reliable, directed.
    ClaimRejected { id: NetEntityId },
}

/// Wire encode/decode errors. Decode failures are expected runtime events
/// (malformed/foreign-version packets) — log and drop, never panic.
#[derive(Debug)]
pub enum WireError {
    /// postcard (de)serialization failure.
    Codec(postcard::Error),
    /// The message's version byte does not match [`WIRE_VERSION`].
    VersionMismatch { got: u8 },
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Codec(e) => write!(f, "wire codec error: {e}"),
            WireError::VersionMismatch { got } => {
                write!(f, "wire version mismatch: got {got}, want {WIRE_VERSION}")
            }
        }
    }
}

impl std::error::Error for WireError {}

impl From<postcard::Error> for WireError {
    fn from(e: postcard::Error) -> Self {
        WireError::Codec(e)
    }
}

pub fn encode_state(msg: &StateMsg) -> Result<Vec<u8>, WireError> {
    Ok(postcard::to_stdvec(msg)?)
}

pub fn decode_state(bytes: &[u8]) -> Result<StateMsg, WireError> {
    let msg: StateMsg = postcard::from_bytes(bytes)?;
    if msg.version != WIRE_VERSION {
        return Err(WireError::VersionMismatch { got: msg.version });
    }
    Ok(msg)
}

pub fn encode_event(msg: &EventMsg) -> Result<Vec<u8>, WireError> {
    Ok(postcard::to_stdvec(msg)?)
}

pub fn decode_event(bytes: &[u8]) -> Result<EventMsg, WireError> {
    let msg: EventMsg = postcard::from_bytes(bytes)?;
    if msg.version != WIRE_VERSION {
        return Err(WireError::VersionMismatch { got: msg.version });
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke() {
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn peer_id_orders_by_value() {
        // Host-migration election relies on "lowest peer ID wins".
        assert!(PeerId(1) < PeerId(2));
        assert_eq!(PeerId(7), PeerId(7));
    }

    #[test]
    fn owner_seq_orders_lexicographically() {
        // The whole ADR-0025 A arbitration rests on this order: `seq` dominates,
        // and `coordinator` breaks equal-seq ties toward the HIGHER id (the newer,
        // post-migration coordinator). A higher seq always outranks a lower one
        // regardless of coordinator.
        let lo_seq_hi_coord = OwnerSeq {
            seq: 4,
            coordinator: PeerId(9),
        };
        let hi_seq_lo_coord = OwnerSeq {
            seq: 5,
            coordinator: PeerId(1),
        };
        assert!(
            hi_seq_lo_coord > lo_seq_hi_coord,
            "seq dominates coordinator"
        );

        // Equal seq: the higher coordinator wins (newer coordinator after a
        // migration is a higher peer id).
        let old_coord = OwnerSeq {
            seq: 5,
            coordinator: PeerId(1),
        };
        let new_coord = OwnerSeq {
            seq: 5,
            coordinator: PeerId(2),
        };
        assert!(
            new_coord > old_coord,
            "equal seq breaks toward higher coordinator"
        );
    }

    #[test]
    fn ack_event_round_trips() {
        let msg = EventMsg {
            version: WIRE_VERSION,
            sig: None,
            event: NetEvent::Ack { seq: 42 },
        };
        let bytes = encode_event(&msg).expect("encode");
        let back = decode_event(&bytes).expect("decode");
        assert_eq!(back, msg);
    }

    #[test]
    fn resync_events_round_trip() {
        let id = NetEntityId {
            spawner: PeerId(1),
            index: 5,
            generation: 0,
        };
        for event in [
            NetEvent::Digest {
                entries: vec![
                    DigestEntry {
                        id,
                        state_hash: Some(0xdead_beef),
                    },
                    DigestEntry {
                        id,
                        state_hash: None,
                    },
                ],
            },
            NetEvent::ResyncRequest { ids: vec![id] },
            NetEvent::ResyncSpawn {
                id,
                pos: quantize_vec2(1.5, -2.0),
                vel: quantize_vec2(0.25, 0.0),
                seq: OwnerSeq {
                    seq: 3,
                    coordinator: PeerId(2),
                },
            },
            NetEvent::OwnershipTransfer {
                id,
                new_owner: PeerId(7),
                seq: OwnerSeq {
                    seq: 9,
                    coordinator: PeerId(1),
                },
            },
        ] {
            let msg = EventMsg {
                version: WIRE_VERSION,
                sig: None,
                event,
            };
            let bytes = encode_event(&msg).expect("encode");
            assert_eq!(decode_event(&bytes).expect("decode"), msg);
        }
    }

    #[test]
    fn handshake_events_round_trip() {
        let id = NetEntityId {
            spawner: PeerId(4),
            index: 2,
            generation: 0,
        };
        for event in [
            NetEvent::ClaimOwnership { id },
            NetEvent::OwnershipCommit {
                id,
                new_owner: PeerId(2),
                seq: OwnerSeq {
                    seq: 7,
                    coordinator: PeerId(1),
                },
            },
            NetEvent::ClaimRejected { id },
        ] {
            let msg = EventMsg {
                version: WIRE_VERSION,
                sig: None,
                event,
            };
            let bytes = encode_event(&msg).expect("encode");
            assert_eq!(decode_event(&bytes).expect("decode"), msg);
        }
    }

    #[test]
    fn wrong_version_is_clean_err() {
        // A foreign-version event is rejected before variant dispatch.
        let mut msg = EventMsg {
            version: WIRE_VERSION.wrapping_sub(1),
            sig: None,
            event: NetEvent::Ack { seq: 1 },
        };
        let bytes = postcard::to_stdvec(&msg).expect("encode");
        assert!(matches!(
            decode_event(&bytes),
            Err(WireError::VersionMismatch { .. })
        ));
        // And the existing lifecycle variants still round-trip at v2.
        msg.version = WIRE_VERSION;
        msg.event = NetEvent::Despawn {
            id: NetEntityId {
                spawner: PeerId(1),
                index: 3,
                generation: 0,
            },
        };
        let bytes = encode_event(&msg).expect("encode");
        assert_eq!(decode_event(&bytes).expect("decode"), msg);
    }
}
