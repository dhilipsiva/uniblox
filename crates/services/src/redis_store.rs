//! The Redis-backed [`RegistryStore`] (ADR-0040) — the real shared registry for
//! horizontal scale. Many stateless signaling nodes point at one Redis and share
//! the session listing.
//!
//! A session's IDENTITY is `(node, key)` — the node is part of it so two nodes
//! hosting the same session key (e.g. a `?next` `room#0` group on each) stay
//! DISTINCT sessions, never conflated. The identity is encoded `<node>\0<key>`
//! (`\0` can't appear in a node id or a room path).
//!
//! Key scheme (no `KEYS`/`SCAN`): each session is a SET `uniblox:sess:<node>\0<key>`
//! of member peers; an index SET `uniblox:sessions` holds the active identities;
//! `uniblox:node:<node>` exists while the node is alive (ADR-0048: set with a TTL by
//! every join and heartbeat). `record_join` is one `MULTI` (`SADD` member, `SADD`
//! index, refresh liveness); `record_leave` is one Lua script (`SREM` member, and
//! de-index once the set empties), so neither leaves a half-applied update. The
//! reads skip the identities of nodes whose liveness key has expired, and `sweep`
//! deletes them (one `MULTI` per identity).
//!
//! A node that misses its heartbeats for longer than the TTL (a long pause or a
//! partition, not only a crash) is swept like a crashed one: its sessions drop out
//! of the listing, and a member reappears only when it joins again. The relay is
//! never affected.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use matchbox_protocol::PeerId;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;

use crate::store::{DEFAULT_NODE_TTL, RegistryError, RegistryResult, RegistryStore, SessionInfo};

/// The index SET of active session identities.
const SESSIONS_INDEX: &str = "uniblox:sessions";

/// The session identity `<node>\0<key>` — `\0` separates node from key (neither a
/// node id nor a room path contains it).
fn session_id(node: &str, key: &str) -> String {
    format!("{node}\0{key}")
}

/// The session KEY portion of an identity (the part after `\0`).
fn id_key(id: &str) -> &str {
    id.split_once('\0').map(|(_, k)| k).unwrap_or(id)
}

/// The NODE portion of an identity (the part before `\0`).
fn id_node(id: &str) -> &str {
    id.split_once('\0').map(|(n, _)| n).unwrap_or("")
}

/// The Redis key that exists while `node` is alive.
fn node_key(node: &str) -> String {
    format!("uniblox:node:{node}")
}

/// Remove a member; de-index the identity once its set is empty. Atomic (one script).
const LEAVE_SCRIPT: &str = r"
redis.call('SREM', KEYS[1], ARGV[1])
if redis.call('SCARD', KEYS[1]) == 0 then
  redis.call('SREM', KEYS[2], ARGV[2])
end
return 0
";

/// The Redis key of the SET holding a session identity's member peers.
fn sess_key(id: &str) -> String {
    format!("uniblox:sess:{id}")
}

fn to_err(e: redis::RedisError) -> RegistryError {
    RegistryError(e.to_string())
}

/// A Redis-backed shared session registry. Cheap to `clone` — the multiplexed
/// connection multiplexes concurrent commands over one socket.
#[derive(Clone)]
pub struct RedisRegistryStore {
    conn: MultiplexedConnection,
    ttl: Duration,
}

impl RedisRegistryStore {
    /// Connect to Redis at `url` (e.g. `redis://127.0.0.1:6379`).
    ///
    /// redis-rs 1.x gives this connection a 1 s connect timeout and a 500 ms response timeout (0.27 had
    /// none, ADR-0041): an unreachable or slow Redis fails here, or makes a store call return a
    /// `RegistryError`, instead of hanging. That suits a best-effort listing mirror, whose failures are
    /// logged and never touch the node-local relay. redis-rs 1.x also rejects `redis://0.0.0.0`.
    pub async fn connect(url: &str) -> RegistryResult<Self> {
        let client = redis::Client::open(url).map_err(to_err)?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(to_err)?;
        Ok(Self {
            conn,
            ttl: DEFAULT_NODE_TTL,
        })
    }

    /// The same store with a different node liveness TTL (at least a millisecond).
    pub fn with_node_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl.max(Duration::from_millis(1));
        self
    }

    fn ttl_ms(&self) -> u64 {
        u64::try_from(self.ttl.as_millis()).unwrap_or(u64::MAX)
    }

    /// The indexed identities, split into those of live nodes and those of dead ones.
    async fn identities(&self) -> RegistryResult<(Vec<String>, Vec<String>)> {
        let mut conn = self.conn.clone();
        let ids: Vec<String> = conn.smembers(SESSIONS_INDEX).await.map_err(to_err)?;
        let nodes: HashSet<&str> = ids.iter().map(|id| id_node(id)).collect();
        let mut alive = HashMap::with_capacity(nodes.len());
        for node in nodes {
            let exists: bool = conn.exists(node_key(node)).await.map_err(to_err)?;
            alive.insert(node, exists);
        }
        let (live, dead): (Vec<&String>, Vec<&String>) = ids
            .iter()
            .partition(|id| alive.get(id_node(id)).copied().unwrap_or(false));
        Ok((
            live.into_iter().cloned().collect(),
            dead.into_iter().cloned().collect(),
        ))
    }

    /// The live sessions with their member counts (an emptied set left by a race is skipped).
    async fn live_sessions(&self) -> RegistryResult<Vec<(String, usize)>> {
        let (live, _) = self.identities().await?;
        let mut conn = self.conn.clone();
        let mut out = Vec::with_capacity(live.len());
        for id in live {
            let peers: usize = conn.scard(sess_key(&id)).await.map_err(to_err)?;
            if peers > 0 {
                out.push((id, peers));
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl RegistryStore for RedisRegistryStore {
    async fn record_join(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()> {
        let mut conn = self.conn.clone();
        let id = session_id(node, key);
        let _: () = redis::pipe()
            .atomic()
            .sadd(sess_key(&id), peer.to_string())
            .ignore()
            .sadd(SESSIONS_INDEX, &id)
            .ignore()
            .pset_ex(node_key(node), 1, self.ttl_ms())
            .ignore()
            .query_async(&mut conn)
            .await
            .map_err(to_err)?;
        Ok(())
    }

    async fn record_leave(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()> {
        let mut conn = self.conn.clone();
        let id = session_id(node, key);
        let _: i64 = redis::Script::new(LEAVE_SCRIPT)
            .key(sess_key(&id))
            .key(SESSIONS_INDEX)
            .arg(peer.to_string())
            .arg(&id)
            .invoke_async(&mut conn)
            .await
            .map_err(to_err)?;
        Ok(())
    }

    async fn list(&self) -> RegistryResult<Vec<SessionInfo>> {
        let mut out: Vec<SessionInfo> = self
            .live_sessions()
            .await?
            .into_iter()
            .map(|(id, peers)| SessionInfo {
                room: id_key(&id).to_string(),
                peers,
            })
            .collect();
        out.sort_by(|a, b| a.room.cmp(&b.room));
        Ok(out)
    }

    async fn peer_count(&self, key: &str) -> RegistryResult<usize> {
        // Aggregate across live nodes for this key (coarse — sums relay-isolated sessions).
        Ok(self
            .live_sessions()
            .await?
            .into_iter()
            .filter(|(id, _)| id_key(id) == key)
            .map(|(_, peers)| peers)
            .sum())
    }

    async fn session_count(&self) -> RegistryResult<usize> {
        Ok(self.live_sessions().await?.len())
    }

    async fn heartbeat(&self, node: &str) -> RegistryResult<()> {
        let mut conn = self.conn.clone();
        let _: () = conn
            .pset_ex(node_key(node), 1, self.ttl_ms())
            .await
            .map_err(to_err)?;
        Ok(())
    }

    async fn sweep(&self) -> RegistryResult<usize> {
        let (_, dead) = self.identities().await?;
        let mut conn = self.conn.clone();
        for id in &dead {
            let _: () = redis::pipe()
                .atomic()
                .del(sess_key(id))
                .ignore()
                .srem(SESSIONS_INDEX, id)
                .ignore()
                .query_async(&mut conn)
                .await
                .map_err(to_err)?;
        }
        Ok(dead.len())
    }

    fn node_ttl(&self) -> Duration {
        self.ttl
    }
}
