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
pub const WIRE_VERSION: u8 = 1;

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
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NetEntityId {
    pub spawner: PeerId,
    pub index: u32,
    pub generation: u32,
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
    pub entries: Vec<StateEntry>,
}

/// Per-entity state: a component is present iff it changed since the sender's
/// last collect (or a keyframe forced a full refresh). Values are ABSOLUTE.
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
    /// Transfer authority. Only the CURRENT owner may send this; identity
    /// (`id`) never changes — only the proxy's `Owner` component does.
    OwnershipTransfer { id: NetEntityId, new_owner: PeerId },
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
}
