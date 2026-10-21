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
//! is tagged by `node`, and each node is alive while it heartbeats (ADR-0048): a
//! node that stops (it crashed) drops out of every read once its liveness TTL
//! lapses, and [`RegistryStore::sweep`] removes what it left.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a node counts as alive after its last heartbeat or join (ADR-0048). A running server
/// heartbeats every third of it.
pub const DEFAULT_NODE_TTL: Duration = Duration::from_secs(30);

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
/// node-local relay senders. Membership is tagged by `node`; the reads count only
/// the members of nodes that are alive (ADR-0048).
#[async_trait]
pub trait RegistryStore: Send + Sync {
    /// Record that `peer` (on `node`) joined the session keyed `key`; this also marks
    /// `node` alive for [`node_ttl`](Self::node_ttl).
    async fn record_join(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()>;
    /// Record that `peer` (on `node`) left `key`; prune the session if now empty.
    async fn record_leave(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()>;
    /// All active sessions of live nodes (key + peer count), sorted by key.
    async fn list(&self) -> RegistryResult<Vec<SessionInfo>>;
    /// Total peers in `key` across all live nodes.
    async fn peer_count(&self, key: &str) -> RegistryResult<usize>;
    /// Number of active sessions across all live nodes.
    async fn session_count(&self) -> RegistryResult<usize>;
    /// Mark `node` alive for another [`node_ttl`](Self::node_ttl).
    async fn heartbeat(&self, node: &str) -> RegistryResult<()>;
    /// Remove every session of a node that is no longer alive; returns how many.
    async fn sweep(&self) -> RegistryResult<usize>;
    /// How long a node counts as alive after its last heartbeat or join.
    fn node_ttl(&self) -> Duration;
}

/// The in-memory membership map, keyed by the SESSION IDENTITY `(node, key)` — the
/// node is part of the identity so two nodes hosting the same session key (e.g. a
/// `?next` `room#0` group on each) are DISTINCT sessions, never conflated (ADR-0040
/// reviewer MEDIUM). Value = the set of member peers on that node.
type Members = HashMap<(String, String), HashSet<PeerId>>;

#[derive(Default)]
struct MemoryInner {
    members: Members,
    /// node → when it stops counting as alive.
    alive_until: HashMap<String, Instant>,
}

impl MemoryInner {
    fn live(&self, node: &str, now: Instant) -> bool {
        self.alive_until.get(node).is_some_and(|until| *until > now)
    }

    /// The sessions of live nodes.
    fn live_sessions(&self, now: Instant) -> impl Iterator<Item = (&String, &HashSet<PeerId>)> {
        self.members
            .iter()
            .filter(move |((node, _), m)| !m.is_empty() && self.live(node, now))
            .map(|((_, key), m)| (key, m))
    }
}

/// An in-process [`RegistryStore`] — the single-node default. Cheap to `clone`
/// (shared `Arc`); two [`crate::SessionRegistry`] instances over ONE clone share
/// the registry (the hermetic two-node test double). Infallible (always `Ok`); a
/// poisoned lock degrades to a no-op / empty listing.
#[derive(Clone)]
pub struct MemoryRegistryStore {
    inner: Arc<Mutex<MemoryInner>>,
    ttl: Duration,
}

impl Default for MemoryRegistryStore {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            ttl: DEFAULT_NODE_TTL,
        }
    }
}

impl MemoryRegistryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The same store with a different node liveness TTL.
    pub fn with_node_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }
}

#[async_trait]
impl RegistryStore for MemoryRegistryStore {
    async fn record_join(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()> {
        if let Ok(mut g) = self.inner.lock() {
            g.members
                .entry((node.to_string(), key.to_string()))
                .or_default()
                .insert(peer);
            g.alive_until
                .insert(node.to_string(), Instant::now() + self.ttl);
        }
        Ok(())
    }

    async fn record_leave(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()> {
        let id = (node.to_string(), key.to_string());
        if let Ok(mut g) = self.inner.lock()
            && let Some(members) = g.members.get_mut(&id)
        {
            members.remove(&peer);
            if members.is_empty() {
                g.members.remove(&id);
            }
        }
        Ok(())
    }

    async fn list(&self) -> RegistryResult<Vec<SessionInfo>> {
        let Ok(g) = self.inner.lock() else {
            return Ok(Vec::new());
        };
        // One entry per (node, key) session; `room` is the key (two nodes hosting
        // the same key ⇒ two distinct entries with the same `room` string).
        let mut out: Vec<SessionInfo> = g
            .live_sessions(Instant::now())
            .map(|(key, m)| SessionInfo {
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
            .inner
            .lock()
            .ok()
            .map(|g| {
                g.live_sessions(Instant::now())
                    .filter(|(k, _)| *k == key)
                    .map(|(_, m)| m.len())
                    .sum()
            })
            .unwrap_or(0))
    }

    async fn session_count(&self) -> RegistryResult<usize> {
        Ok(self
            .inner
            .lock()
            .map(|g| g.live_sessions(Instant::now()).count())
            .unwrap_or(0))
    }

    async fn heartbeat(&self, node: &str) -> RegistryResult<()> {
        if let Ok(mut g) = self.inner.lock() {
            g.alive_until
                .insert(node.to_string(), Instant::now() + self.ttl);
        }
        Ok(())
    }

    async fn sweep(&self) -> RegistryResult<usize> {
        let Ok(mut g) = self.inner.lock() else {
            return Ok(0);
        };
        let now = Instant::now();
        let before = g.members.len();
        let alive: HashSet<String> = g
            .alive_until
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(node, _)| node.clone())
            .collect();
        g.members.retain(|(node, _), _| alive.contains(node));
        g.alive_until.retain(|node, _| alive.contains(node));
        Ok(before - g.members.len())
    }

    fn node_ttl(&self) -> Duration {
        self.ttl
    }
}
