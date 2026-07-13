//! The shared session registry (ADR-0040): a [`RegistryStore`] abstraction that
//! lets multiple STATELESS signaling nodes share one session→peer LISTING, plus
//! the in-process [`MemoryRegistryStore`].
//!
//! Only the LISTING / membership metadata is shared — the WebRTC relay (which
//! holds per-connection `SignalingChannel`s) stays node-local, so the two peers
//! of a session must be on the SAME node (sticky routing at the load balancer;
//! cross-node relay is out of scope). The real backend is
//! [`crate::RedisRegistryStore`]; `MemoryRegistryStore` is the single-node default
//! (and, shared across instances, the hermetic two-node test double). Membership
//! is tagged by `node` so a node's entries stay attributable (future
//! stale-cleanup).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use matchbox_protocol::PeerId;

/// One active session in a registry listing. `room` is the session key: a room
/// path (unbounded), or `"<room>#<index>"` when subdivided by `?next=N`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub room: String,
    pub peers: usize,
}

/// A shared-registry backend failure (e.g. a Redis I/O error). The in-memory
/// store never errors.
#[derive(Debug)]
pub struct RegistryError(pub String);

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "registry store error: {}", self.0)
    }
}

impl std::error::Error for RegistryError {}

pub type RegistryResult<T> = Result<T, RegistryError>;

/// The shared session registry: session→peer membership that many stateless
/// signaling nodes can read/write. Records ONLY listing metadata — never the
/// node-local relay senders. Membership is tagged by `node`.
#[async_trait]
pub trait RegistryStore: Send + Sync {
    /// Record that `peer` (on `node`) joined the session keyed `key`.
    async fn record_join(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()>;
    /// Record that `peer` (on `node`) left `key`; prune the session if now empty.
    async fn record_leave(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()>;
    /// All active sessions (key + total peer count across nodes), sorted by key.
    async fn list(&self) -> RegistryResult<Vec<SessionInfo>>;
    /// Total peers in `key` across all nodes.
    async fn peer_count(&self, key: &str) -> RegistryResult<usize>;
    /// Number of active sessions across all nodes.
    async fn session_count(&self) -> RegistryResult<usize>;
}

/// The in-memory membership map, keyed by the SESSION IDENTITY `(node, key)` — the
/// node is part of the identity so two nodes hosting the same session key (e.g. a
/// `?next` `room#0` group on each) are DISTINCT sessions, never conflated (ADR-0040
/// reviewer MEDIUM). Value = the set of member peers on that node.
type Members = HashMap<(String, String), HashSet<PeerId>>;

/// An in-process [`RegistryStore`] — the single-node default. Cheap to `clone`
/// (shared `Arc`); two [`crate::SessionRegistry`] instances over ONE clone share
/// the registry (the hermetic two-node test double). Infallible (always `Ok`); a
/// poisoned lock degrades to a no-op / empty listing.
#[derive(Clone, Default)]
pub struct MemoryRegistryStore(Arc<Mutex<Members>>);

impl MemoryRegistryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl RegistryStore for MemoryRegistryStore {
    async fn record_join(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()> {
        if let Ok(mut g) = self.0.lock() {
            g.entry((node.to_string(), key.to_string()))
                .or_default()
                .insert(peer);
        }
        Ok(())
    }

    async fn record_leave(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()> {
        let id = (node.to_string(), key.to_string());
        if let Ok(mut g) = self.0.lock()
            && let Some(members) = g.get_mut(&id)
        {
            members.remove(&peer);
            if members.is_empty() {
                g.remove(&id);
            }
        }
        Ok(())
    }

    async fn list(&self) -> RegistryResult<Vec<SessionInfo>> {
        let Ok(g) = self.0.lock() else {
            return Ok(Vec::new());
        };
        // One entry per (node, key) session; `room` is the key (two nodes hosting
        // the same key ⇒ two distinct entries with the same `room` string).
        let mut out: Vec<SessionInfo> = g
            .iter()
            .map(|((_node, key), m)| SessionInfo {
                room: key.clone(),
                peers: m.len(),
            })
            .collect();
        out.sort_by(|a, b| a.room.cmp(&b.room));
        Ok(out)
    }

    async fn peer_count(&self, key: &str) -> RegistryResult<usize> {
        // Aggregate across nodes for this key (coarse — sums relay-isolated sessions).
        Ok(self
            .0
            .lock()
            .ok()
            .map(|g| {
                g.iter()
                    .filter(|((_node, k), _)| k == key)
                    .map(|(_, m)| m.len())
                    .sum()
            })
            .unwrap_or(0))
    }

    async fn session_count(&self) -> RegistryResult<usize> {
        Ok(self.0.lock().map(|g| g.len()).unwrap_or(0))
    }
}
