//! `protocol` — shared wire types: versions, messages, content IDs.
//!
//! Minimal at this stage: only the shared peer identity exists. Wire messages,
//! the `{engine, content, schema}` version triple, and serde derives land with
//! the replication wire format (later in Phase 1) and Phase 5.

/// Identity of a peer (player instance or server) in a session.
///
/// Shared by `engine-core` (ownership tags), `transport`, and `replication`.
/// `Ord` is deliberate: host-migration election (Phase 3/5) tiebreaks on the
/// lowest peer ID. No serde yet — that lands with the wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub u64);

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
