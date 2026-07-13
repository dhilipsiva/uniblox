//! Content-addressed identity (ADR-0032): a [`ContentId`] is the blake3-256
//! digest of a canonical byte blob. THE content-addressing primitive — used by
//! the Mode-1 save (Phase 4) and, forward, object storage (Phase 7) and publish
//! (Phase 8). The reserved [`VersionTriple`] pre-positions the Phase-5
//! `{engine, content, schema}` triple so adding enforcement later needs no
//! wire/save shape change.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The blake3-256 digest of a byte blob — a stable, collision-resistant content
/// address. `Ord` is deliberate (like [`crate::PeerId`] / [`crate::NetEntityId`]):
/// deterministic content-store iteration + stable tests.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentId([u8; 32]);

impl ContentId {
    /// The raw 32-byte digest.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Construct from a raw 32-byte digest (e.g. a stored key). Prefer
    /// [`content_id`] to DERIVE an id from content.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        ContentId(bytes)
    }

    /// Lowercase 64-char hex — the canonical string form (store filename / key).
    pub fn to_hex(&self) -> String {
        blake3::Hash::from_bytes(self.0).to_hex().to_string()
    }

    /// Parse the 64-char hex form back into a [`ContentId`]. Input is
    /// case-insensitive (blake3 accepts either case); [`to_hex`](Self::to_hex)
    /// always emits lowercase — the canonical form.
    pub fn from_hex(s: &str) -> Result<Self, ContentIdError> {
        let hash = blake3::Hash::from_hex(s).map_err(|_| ContentIdError::BadHex)?;
        Ok(ContentId(*hash.as_bytes()))
    }
}

impl fmt::Display for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// The blake3-256 content id of `bytes`. Deterministic (identical bytes → the
/// same id) and collision-resistant (finding two blobs with one id is
/// cryptographically infeasible). The single content-addressing entry point
/// (Phase-4 save, Phase-7 object store, Phase-8 publish).
pub fn content_id(bytes: &[u8]) -> ContentId {
    ContentId(*blake3::hash(bytes).as_bytes())
}

/// Errors parsing a [`ContentId`] from its hex form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentIdError {
    /// Not a valid 64-char lowercase-hex blake3 digest.
    BadHex,
}

impl fmt::Display for ContentIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentIdError::BadHex => write!(f, "invalid content-id hex"),
        }
    }
}

impl std::error::Error for ContentIdError {}

/// The reserved `{engine, content, schema}` version triple. Phase 5 enforces it
/// at session join; Phase 4 only RESERVES it — a save blob carries an
/// `Option<VersionTriple>` (`None` today), so adding enforcement later needs no
/// shape change. `engine` = binary release, `content` = UGC bundle revision,
/// `schema` = wire/save schema revision.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VersionTriple {
    pub engine: u32,
    pub content: u32,
    pub schema: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_same_bytes_same_id() {
        assert_eq!(content_id(b"hello world"), content_id(b"hello world"));
    }

    #[test]
    fn distinct_bytes_distinct_id() {
        assert_ne!(content_id(b"a"), content_id(b"b"));
        assert_ne!(content_id(b""), content_id(b"a"));
    }

    #[test]
    fn known_blake3_vector_locks_the_algorithm() {
        // blake3 of the empty input — the documented test vector. Guards against
        // a silent hash-algorithm swap.
        assert_eq!(
            content_id(b"").to_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn hex_round_trips() {
        let id = content_id(b"round trip me");
        assert_eq!(id.to_hex().len(), 64);
        assert_eq!(ContentId::from_hex(&id.to_hex()), Ok(id));
    }

    #[test]
    fn from_hex_rejects_garbage() {
        assert_eq!(
            ContentId::from_hex("not valid hex"),
            Err(ContentIdError::BadHex)
        );
        assert_eq!(ContentId::from_hex("abc"), Err(ContentIdError::BadHex)); // too short
    }

    #[test]
    fn from_bytes_round_trips() {
        let id = content_id(b"x");
        assert_eq!(ContentId::from_bytes(*id.as_bytes()), id);
    }

    #[test]
    fn postcard_round_trips() {
        let id = content_id(b"serde me");
        let bytes = postcard::to_stdvec(&id).expect("encode");
        let back: ContentId = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(id, back);
        // 32 raw digest bytes on the wire (no length prefix for a fixed array).
        assert_eq!(bytes.len(), 32);
    }
}
