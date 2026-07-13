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
//! of member peers; an index SET `uniblox:sessions` holds the active identities.
//! `record_join` = `SADD` member + `SADD` index; `record_leave` = `SREM` member,
//! then de-index when the set empties; `peer_count(key)` sums the sets whose
//! identity has that key; `session_count` = `SCARD` the index; `list` = `SMEMBERS`
//! the index → `SCARD` each.
//!
//! Best-effort: the multi-command sequences are NOT atomic, so under concurrent
//! join+leave on ONE identity `session_count` (a bare `SCARD` of the index) can
//! transiently disagree with `list().len()` (which masks empty sets) until the next
//! clean op on that identity. It never affects the relay. Atomic `MULTI`/Lua is
//! deferred. A crashed node's members linger (no TTL/heartbeat) — deferred to
//! Phase-11 (registry-under-load).

use async_trait::async_trait;
use matchbox_protocol::PeerId;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;

use crate::store::{RegistryError, RegistryResult, RegistryStore, SessionInfo};

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
}

impl RedisRegistryStore {
    /// Connect to Redis at `url` (e.g. `redis://127.0.0.1:6379`).
    pub async fn connect(url: &str) -> RegistryResult<Self> {
        let client = redis::Client::open(url).map_err(to_err)?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(to_err)?;
        Ok(Self { conn })
    }
}

#[async_trait]
impl RegistryStore for RedisRegistryStore {
    async fn record_join(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()> {
        let mut conn = self.conn.clone();
        let id = session_id(node, key);
        let _: () = conn
            .sadd(sess_key(&id), peer.to_string())
            .await
            .map_err(to_err)?;
        let _: () = conn.sadd(SESSIONS_INDEX, &id).await.map_err(to_err)?;
        Ok(())
    }

    async fn record_leave(&self, node: &str, key: &str, peer: PeerId) -> RegistryResult<()> {
        let mut conn = self.conn.clone();
        let id = session_id(node, key);
        let _: () = conn
            .srem(sess_key(&id), peer.to_string())
            .await
            .map_err(to_err)?;
        let remaining: usize = conn.scard(sess_key(&id)).await.map_err(to_err)?;
        if remaining == 0 {
            let _: () = conn.srem(SESSIONS_INDEX, &id).await.map_err(to_err)?;
        }
        Ok(())
    }

    async fn list(&self) -> RegistryResult<Vec<SessionInfo>> {
        let mut conn = self.conn.clone();
        let ids: Vec<String> = conn.smembers(SESSIONS_INDEX).await.map_err(to_err)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let peers: usize = conn.scard(sess_key(&id)).await.map_err(to_err)?;
            // Skip a de-index race leftover (indexed identity whose set emptied).
            if peers > 0 {
                out.push(SessionInfo {
                    room: id_key(&id).to_string(),
                    peers,
                });
            }
        }
        out.sort_by(|a, b| a.room.cmp(&b.room));
        Ok(out)
    }

    async fn peer_count(&self, key: &str) -> RegistryResult<usize> {
        // Aggregate across nodes for this key (coarse — sums relay-isolated sessions).
        let mut conn = self.conn.clone();
        let ids: Vec<String> = conn.smembers(SESSIONS_INDEX).await.map_err(to_err)?;
        let mut total = 0;
        for id in ids {
            if id_key(&id) == key {
                total += conn
                    .scard::<_, usize>(sess_key(&id))
                    .await
                    .map_err(to_err)?;
            }
        }
        Ok(total)
    }

    async fn session_count(&self) -> RegistryResult<usize> {
        let mut conn = self.conn.clone();
        conn.scard(SESSIONS_INDEX).await.map_err(to_err)
    }
}
