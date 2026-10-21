//! uniblox signaling service: room-based WebRTC SDP/ICE signaling with a
//! `{mode, version}` scope + the ASYMMETRIC version filter (ADR-0038), optional
//! `?next=N` session-SIZE grouping (ADR-0039), a session registry that can be
//! shared through Redis (ADR-0040, ADR-0048), and join tokens and DoS limits
//! (ADR-0047). It speaks matchbox's wire protocol (`matchbox_protocol`) from its
//! own axum server (`server.rs`), so matchbox clients connect unchanged.
//!
//! **Scope (in the room PATH):** `<mode>~<content>.<schema>~<min>~<lobby>`
//! (e.g. `m1~7.2~3~arena`). The client's OWN engine rides the `?engine=N` query.
//! content/schema/min/lobby are in the path ⇒ their exact match is structural (a
//! different one is a different room ⇒ never matched); the engine is NOT in the
//! path ⇒ compatible-but-newer engines share a room. The connection gate admits
//! iff `engine >= min` (the asymmetric filter) and returns a REASONED rejection
//! otherwise: `426 Upgrade Required` for `engine < min`, `400 Bad Request` for a
//! malformed scope / bad `?engine` / bad `?next`. A plain path (no `~`) is a
//! legacy room (no engine gate).
//!
//! **Session-SIZE grouping (`?next=N`, ADR-0039):** the relay is a full-mesh
//! SDP/ICE relay (matchbox `FullMesh` re-implemented from its contract)
//! GENERALIZED with size grouping. `?next`
//! absent ⇒ ONE unbounded session per room (behaviourally identical to FullMesh);
//! `?next=N` ⇒ the room subdivides into sessions of at most N, keyed
//! `"<room>#<index>"`. Lifecycle is **batch-deal / no-backfill**: a session seals
//! when it reaches N and never refills — a departure shrinks it but new joiners
//! always fill the current open session (a fresh one once the last sealed). The
//! gate RESERVES `#` (the sub-session delimiter):
//! a room path may not contain it, so the `"<room>#<index>"` key namespace stays
//! disjoint from the raw-room-path namespace (no collision / cross-lobby leak).
//! `?next` is a per-lobby CONVENTION: peers naming the same room but disagreeing
//! on whether they send `?next` land in disjoint session namespaces (`R` vs
//! `R#0`) and are never matched — clients of one lobby must agree on it.
//!
//! The [`SessionRegistry`] is the relay's shared state: it holds each member's
//! bounded outbound queue and the grouping bookkeeping, and lists sessions
//! (`list`/`peer_count`/`session_count`). A poisoned lock degrades to a no-op /
//! empty listing rather than panicking the server.
//!
//! **Trust model:** `?engine` and the path `min` are self-declared — desync
//! defense for HONEST clients (`CLAUDE.md`: the version gate is not anti-cheat; a
//! modified client can lie, but cannot lie into a *stricter* room with an old
//! engine, since a different `min` is a different room).
//!
//! **Authentication and limits (ADR-0047):** [`build_signaling_server_with`] takes a
//! [`SignalingConfig`]. With a [`JoinTokenKey`], the gate admits only a connection carrying a valid
//! `?token=` for the room path it asks for (401 with a reason otherwise); an optional endpoint at
//! [`TOKEN_PATH`] issues tokens. Every server limits connection attempts per client IP (429), open
//! connections per IP (429) and in total (503), frames per connection (dropped, then the socket is
//! closed), the size of a WebSocket message (the WebSocket layer closes the connection), idle time,
//! each peer's outbound queue (a peer that does not read is disconnected), and the peers per
//! session (503; `?next` above the cap is a 400).
//!
//! Deferred: the Mode-2 coordinator peer service.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use matchbox_protocol::PeerId;
use tokio::time::timeout;
use uuid::Uuid;

mod auth;
mod limits;
mod redis_store;
mod server;
mod store;
mod turn;

use server::PeerTx;
pub use server::SignalingServer;

pub use auth::{JoinTokenKey, MAX_TOKEN_LIFETIME_SECS, MIN_KEY_BYTES, TokenError, unix_now};
pub use limits::{ClientIp, SignalingLimits};
pub use redis_store::RedisRegistryStore;
pub use store::{
    DEFAULT_NODE_TTL, MemoryRegistryStore, RegistryError, RegistryResult, RegistryStore,
    SessionInfo,
};
pub use turn::{MAX_TURN_TTL_SECS, TurnConfig, TurnCredential, turn_credential};

/// The scope delimiter between a room path's tilde-separated parts.
pub(crate) const SCOPE_SEP: char = '~';

/// The `?engine=N` query key carrying the client's own engine version.
const ENGINE_PARAM: &str = "engine";

/// The `?next=N` query key carrying the desired session-SIZE cap.
const NEXT_PARAM: &str = "next";

/// Bounded wait for a shared-registry mirror op. A slow/black-holed store (redis-rs
/// has no default response timeout) degrades to a bounded delay + a dropped mirror,
/// never a hang that would stall a peer's relay-loop start (ADR-0040).
const STORE_MIRROR_TIMEOUT: Duration = Duration::from_secs(2);

/// The RESERVED delimiter that separates a room from its sub-session index in a
/// bounded session key (`"<room>#<index>"`). A room path may NOT contain it (the
/// gate rejects it), so the synthesized bounded-session-key namespace stays
/// DISJOINT from the raw-room-path (unbounded) namespace — otherwise a client
/// could name a room (e.g. `arena%230` ⇒ `arena#0` after axum percent-decoding)
/// that collides with another room's `?next` sub-session and cross the isolation
/// boundary (ADR-0039 reviewer MEDIUM).
const SESSION_SEP: char = '#';

/// The mode tag of a scoped room. The engine has no `Mode` enum (mode is data
/// everywhere else); this tag exists ONLY to scope matchmaking at signaling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `m1` — Standalone.
    Standalone,
    /// `m2` — P2P Hybrid.
    P2p,
    /// `m3` — Full-Server.
    Server,
}

impl Mode {
    fn parse(tag: &str) -> Option<Mode> {
        match tag {
            "m1" => Some(Mode::Standalone),
            "m2" => Some(Mode::P2p),
            "m3" => Some(Mode::Server),
            _ => None,
        }
    }
}

/// A parsed matchmaking scope. Two peers share a room iff their FULL scope
/// (mode, content, schema, min-engine, lobby) is identical — content/schema/lobby
/// exactness is structural. The client's own engine is NOT part of the scope
/// (it rides the `?engine=` query, gated `>= min_engine`), so compatible-but-newer
/// engines share one room (the ASYMMETRIC filter, ADR-0038).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub mode: Mode,
    /// Content ID — must match EXACTLY across peers (structural).
    pub content: u32,
    /// Schema version — must match EXACTLY across peers (structural).
    pub schema: u32,
    /// The game's declared MINIMUM engine version; a joiner is admitted iff its
    /// own `?engine=` is `>= min_engine`.
    pub min_engine: u32,
    pub lobby: String,
}

/// Why a `~`-shaped room path is not a valid scope, or a query param is bad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeError {
    /// Not exactly `<mode>~<content>.<schema>~<min>~<lobby>` (wrong part count).
    Shape,
    /// The mode tag is not one of `m1` / `m2` / `m3`.
    Mode,
    /// `content` is not a `u32`, or the `content.schema` part isn't two fields.
    Content,
    /// `schema` is not a `u32`, or the `content.schema` part isn't two fields.
    Schema,
    /// The min-engine part is not a `u32`.
    Min,
    /// The lobby segment is empty.
    Lobby,
    /// The `?engine=` query param is absent or not a `u32`.
    Engine,
    /// The `?next=` query param is present but not a positive integer.
    Next,
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ScopeError::Shape => "scope must be <mode>~<content>.<schema>~<min>~<lobby>",
            ScopeError::Mode => "mode must be m1, m2, or m3",
            ScopeError::Content => "content must be a u32 (<content>.<schema>)",
            ScopeError::Schema => "schema must be a u32 (<content>.<schema>)",
            ScopeError::Min => "min-engine must be a u32",
            ScopeError::Lobby => "lobby must be non-empty",
            ScopeError::Engine => "missing or non-numeric ?engine=<u32>",
            ScopeError::Next => "?next must be a positive integer (>= 1)",
        })
    }
}

impl std::error::Error for ScopeError {}

/// Parse a scoped room path. Only meaningful for a path containing `SCOPE_SEP`
/// — a plain path (no `~`) is a legacy room, not a scope.
pub fn parse_scope(path: &str) -> Result<Scope, ScopeError> {
    let mut it = path.split(SCOPE_SEP);
    let mode = it.next().ok_or(ScopeError::Shape)?;
    let content_schema = it.next().ok_or(ScopeError::Shape)?;
    let min = it.next().ok_or(ScopeError::Shape)?;
    let lobby = it.next().ok_or(ScopeError::Shape)?;
    if it.next().is_some() {
        return Err(ScopeError::Shape); // more than four parts
    }
    let mode = Mode::parse(mode).ok_or(ScopeError::Mode)?;
    let (content, schema) = parse_content_schema(content_schema)?;
    let min_engine = min.parse().map_err(|_| ScopeError::Min)?;
    if lobby.is_empty() {
        return Err(ScopeError::Lobby);
    }
    Ok(Scope {
        mode,
        content,
        schema,
        min_engine,
        lobby: lobby.to_string(),
    })
}

/// Split the `<content>.<schema>` scope part into its two `u32`s.
fn parse_content_schema(s: &str) -> Result<(u32, u32), ScopeError> {
    let mut it = s.split('.');
    let content = it.next().ok_or(ScopeError::Content)?;
    let schema = it.next().ok_or(ScopeError::Schema)?;
    if it.next().is_some() {
        return Err(ScopeError::Schema); // more than two fields
    }
    Ok((
        content.parse().map_err(|_| ScopeError::Content)?,
        schema.parse().map_err(|_| ScopeError::Schema)?,
    ))
}

/// Read the client's own engine version from the `?engine=<u32>` query params.
/// Separate from [`parse_scope`] because the engine is deliberately OUT of the
/// room key (so compatible engines share a room).
pub fn parse_engine(query: &HashMap<String, String>) -> Result<u32, ScopeError> {
    query
        .get(ENGINE_PARAM)
        .and_then(|v| v.parse().ok())
        .ok_or(ScopeError::Engine)
}

/// Read the optional `?next=<usize>` session-SIZE cap. Absent ⇒ `Ok(None)`
/// (one unbounded session per room). Present ⇒ a `usize >= 1`, else
/// [`ScopeError::Next`].
pub fn parse_next(query: &HashMap<String, String>) -> Result<Option<usize>, ScopeError> {
    match query.get(NEXT_PARAM) {
        None => Ok(None),
        Some(v) => match v.parse::<usize>() {
            Ok(n) if n >= 1 => Ok(Some(n)),
            _ => Err(ScopeError::Next),
        },
    }
}

/// Where the token endpoint is served: two path segments, so it can never be a room path.
pub const TOKEN_PATH: &str = "/_uniblox/token";

/// Where TURN credentials are served (ADR-0050): `GET` this `?room=<room>&token=<join token>`.
pub const TURN_PATH: &str = "/_uniblox/turn";

/// The query parameter that carries a join token.
pub const TOKEN_PARAM: &str = "token";

/// The longest room path accepted, in bytes.
pub const MAX_ROOM_BYTES: usize = 256;

/// How a signaling server authenticates and limits its clients (ADR-0047).
#[derive(Clone, Debug)]
pub struct SignalingConfig {
    /// With a key, a peer must present a valid join token for its room; `None` leaves the gate open
    /// (local development and the in-process tests).
    pub auth: Option<JoinTokenKey>,
    /// Serve [`TOKEN_PATH`]`?room=<room>`, issuing a token to anyone (within the per-IP limit). Needs
    /// `auth`. For a hosted demo; a deployment with accounts mints tokens in its own backend.
    pub issue_tokens: bool,
    /// The lifetime of a token the endpoint issues (capped at [`MAX_TOKEN_LIFETIME_SECS`]).
    pub token_lifetime: Duration,
    /// The TURN relay to hand time-limited credentials for, at [`TURN_PATH`] (ADR-0050). Needs
    /// `auth`: credentials go only to a client with a valid join token for its room.
    pub turn: Option<TurnConfig>,
    /// The rate limits and caps.
    pub limits: SignalingLimits,
    /// Where the client IP for the per-IP limit comes from.
    pub client_ip: ClientIp,
}

impl Default for SignalingConfig {
    fn default() -> Self {
        Self {
            auth: None,
            issue_tokens: false,
            token_lifetime: Duration::from_secs(600),
            turn: None,
            limits: SignalingLimits::default(),
            client_ip: ClientIp::Peer,
        }
    }
}

/// A room path the server accepts: non-empty, at most [`MAX_ROOM_BYTES`], free of the reserved
/// sub-session delimiter, and a well-formed scope if it has the scope shape. The gate, the token
/// endpoint and `services mint-token` all apply it.
pub fn validate_room(room: &str) -> Result<(), String> {
    if room.is_empty() {
        return Err("missing room".to_string());
    }
    if room.len() > MAX_ROOM_BYTES {
        return Err(format!("room longer than {MAX_ROOM_BYTES} bytes"));
    }
    if room.contains(SESSION_SEP) {
        return Err(format!("room may not contain '{SESSION_SEP}'"));
    }
    if room.contains(SCOPE_SEP) {
        parse_scope(room).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Default)]
struct RegistryInner {
    /// Live sessions keyed by session key: the room path (unbounded), or
    /// `"<room>#<index>"` when subdivided by `?next=N`. Maps each member to its
    /// bounded outbound queue. This is what the node-local `list()` reports.
    sessions: HashMap<String, HashMap<PeerId, PeerTx>>,
    /// The current OPEN (fillable) session key per room, for `?next=N` rooms only.
    /// Removed when the session seals (reaches N) ⇒ no backfill (ADR-0039).
    open: HashMap<String, String>,
    /// Monotonic sub-session index per room, for unique session keys; dropped once the room has no
    /// session left.
    counter: HashMap<String, u64>,
    /// peer → its session key (reverse index for relay + `leave`).
    peer_session: HashMap<PeerId, String>,
}

/// The signaling node's session state (ADR-0040). Two layers:
///
/// * **node-local relay** (`local`): each session's peers + live relay senders +
///   the `?next` grouping bookkeeping. Drives the relay; never leaves the process.
///   `list`/`peer_count`/`session_count` report THIS node's live sessions.
/// * **shared registry** (`store`): a [`RegistryStore`] mirror of session→peer
///   membership, tagged by `node_id`, that many STATELESS nodes share. The
///   `global_*` methods report the registry across all nodes.
///
/// Cheap to `clone` (shared `Arc`s): the same handle is the relay's state and the
/// gate's view of session sizes. Only the LISTING is shared —
/// the relay is node-local, so the two peers of a session must be on the same node
/// (sticky routing; cross-node relay is out of scope).
#[derive(Clone)]
pub struct SessionRegistry {
    local: Arc<Mutex<RegistryInner>>,
    store: Arc<dyn RegistryStore>,
    node_id: Arc<str>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    /// A single-node registry: an in-memory store + a fresh random node id.
    pub fn new() -> Self {
        Self::with_store(
            Arc::new(MemoryRegistryStore::new()),
            Uuid::new_v4().to_string(),
        )
    }

    /// A registry over a SHARED `store` (e.g. Redis, or one `MemoryRegistryStore`
    /// shared by several nodes) with an explicit `node_id`. Two nodes over one
    /// store share the registry (the horizontal-scale surface).
    pub fn with_store(store: Arc<dyn RegistryStore>, node_id: impl Into<Arc<str>>) -> Self {
        Self {
            local: Arc::new(Mutex::new(RegistryInner::default())),
            store,
            node_id: node_id.into(),
        }
    }

    /// Keep this node alive in the shared store and sweep the sessions of dead nodes, every third of
    /// the store's node TTL, until the task is aborted (ADR-0048). [`build_signaling_server_with`]
    /// starts it; a host that drives a `SessionRegistry` without that builder starts it itself. Store
    /// failures are logged and retried at the next beat; each call is bounded by the mirror timeout.
    pub fn spawn_heartbeat(&self) -> tokio::task::JoinHandle<()> {
        let store = Arc::clone(&self.store);
        let node = Arc::clone(&self.node_id);
        let every = (store.node_ttl() / 3).max(Duration::from_millis(10));
        tokio::spawn(async move {
            loop {
                match timeout(STORE_MIRROR_TIMEOUT, store.heartbeat(&node)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => tracing::warn!("registry heartbeat failed: {err}"),
                    Err(_) => tracing::warn!("registry heartbeat timed out"),
                }
                match timeout(STORE_MIRROR_TIMEOUT, store.sweep()).await {
                    Ok(Ok(0)) => {}
                    Ok(Ok(n)) => tracing::info!("swept {n} sessions of dead signaling nodes"),
                    Ok(Err(err)) => tracing::warn!("registry sweep failed: {err}"),
                    Err(_) => tracing::warn!("registry sweep timed out"),
                }
                tokio::time::sleep(every).await;
            }
        })
    }

    /// Node-local join assignment (sync; the lock is dropped before it returns).
    /// Assigns `peer` to a session per the batch-deal / no-backfill rule, registers
    /// its `sender`, and returns `Some((session key, existing members' senders))` —
    /// the senders captured BEFORE inserting the newcomer (so it gets no
    /// self-event). `None` iff the local lock is poisoned or the session already holds
    /// `max_peers` (nothing was registered, so the caller mirrors nothing — LOW-2 symmetry).
    fn join_local(
        &self,
        room: &str,
        peer: PeerId,
        sender: PeerTx,
        next: Option<usize>,
        max_peers: usize,
    ) -> Option<(String, Vec<PeerTx>)> {
        let mut g = self.local.lock().ok()?;

        let key = match next {
            None => room.to_string(), // unbounded ⇒ one session per room (FullMesh)
            Some(n) => {
                // Reuse the room's OPEN session iff it still has room; else open a
                // fresh one and mark it open.
                let reuse = match g.open.get(room) {
                    Some(k) => {
                        let k = k.clone(); // release the `open` borrow before `sessions`
                        g.sessions.get(&k).is_some_and(|m| m.len() < n).then_some(k)
                    }
                    None => None,
                };
                match reuse {
                    Some(k) => k,
                    None => {
                        let c = g.counter.entry(room.to_string()).or_insert(0);
                        let idx = *c;
                        *c += 1;
                        // `room` is guaranteed `#`-free (the gate rejects it), so
                        // this key can't collide with any raw room path.
                        let k = format!("{room}{SESSION_SEP}{idx}");
                        g.open.insert(room.to_string(), k.clone());
                        k
                    }
                }
            }
        };

        // The gate refuses a full session with a reason; this is the backstop for a race.
        if g.sessions.get(&key).is_some_and(|m| m.len() >= max_peers) {
            return None;
        }
        // Existing members' senders — captured BEFORE inserting the newcomer.
        let existing: Vec<PeerTx> = g
            .sessions
            .get(&key)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();

        // Insert the newcomer; seal the session if it just reached capacity.
        let sealed = {
            let members = g.sessions.entry(key.clone()).or_default();
            members.insert(peer, sender);
            matches!(next, Some(n) if members.len() >= n)
        };
        if sealed && g.open.get(room).map(String::as_str) == Some(key.as_str()) {
            g.open.remove(room); // no backfill: a sealed session is never reused
        }
        g.peer_session.insert(peer, key.clone());
        Some((key, existing))
    }

    /// The relay target for a `Signal`: `receiver`'s sender IFF it shares
    /// `peer`'s session (cross-session ⇒ `None`, the isolation boundary).
    fn target(&self, peer: PeerId, receiver: PeerId) -> Option<PeerTx> {
        let g = self.local.lock().ok()?;
        let key = g.peer_session.get(&peer)?;
        g.sessions.get(key)?.get(&receiver).cloned()
    }

    /// Node-local leave (sync; lock dropped before it returns): drop `peer` from
    /// its session (pruning an emptied session + clearing a dangling `open`
    /// pointer). Returns `Some((session key, remaining members' senders))`, or
    /// `None` if the peer was in no session (or the lock is poisoned).
    fn leave_local(&self, peer: PeerId) -> Option<(String, Vec<PeerTx>)> {
        let mut g = self.local.lock().ok()?;
        let key = g.peer_session.remove(&peer)?;
        let mut remaining = Vec::new();
        let mut emptied = false;
        if let Some(members) = g.sessions.get_mut(&key) {
            members.remove(&peer);
            remaining = members.values().cloned().collect();
            emptied = members.is_empty();
        }
        if emptied {
            g.sessions.remove(&key);
            // Clear a still-open pointer to the now-gone session (an unsealed
            // session that emptied; a sealed one was already un-pointed).
            g.open.retain(|_, v| v != &key);
            // A `?next` room with no session left forgets its index counter (no leak per room).
            if let Some((room, _)) = key.rsplit_once(SESSION_SEP) {
                let prefix = format!("{room}{SESSION_SEP}");
                if !g.sessions.keys().any(|k| k.starts_with(&prefix)) {
                    g.counter.remove(room);
                }
            }
        }
        Some((key, remaining))
    }

    /// Mirror a join to the shared registry — best-effort with a BOUNDED timeout
    /// (`STORE_MIRROR_TIMEOUT`): a slow/black-holed store degrades to a bounded
    /// delay + a dropped mirror, NEVER a hang (redis-rs has no default response
    /// timeout). Called AFTER the caller broadcast `NewPeer`, so peer discovery is
    /// never delayed by the store — only this peer's own relay-loop start, and only
    /// up to the timeout. The listing may miss this peer; the relay is unaffected.
    async fn mirror_join(&self, key: &str, peer: PeerId) {
        match timeout(
            STORE_MIRROR_TIMEOUT,
            self.store.record_join(&self.node_id, key, peer),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("shared registry record_join failed: {e}"),
            Err(_) => tracing::warn!("shared registry record_join timed out"),
        }
    }

    /// Mirror a leave to the shared registry — best-effort, bounded (see
    /// [`mirror_join`](Self::mirror_join)).
    async fn mirror_leave(&self, key: &str, peer: PeerId) {
        match timeout(
            STORE_MIRROR_TIMEOUT,
            self.store.record_leave(&self.node_id, key, peer),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("shared registry record_leave failed: {e}"),
            Err(_) => tracing::warn!("shared registry record_leave timed out"),
        }
    }

    /// List THIS node's live sessions (keys + peer counts), sorted. For the shared
    /// registry across all nodes, use [`global_list`](Self::global_list).
    pub fn list(&self) -> Vec<SessionInfo> {
        let Ok(g) = self.local.lock() else {
            return Vec::new();
        };
        let mut out: Vec<SessionInfo> = g
            .sessions
            .iter()
            .map(|(room, peers)| SessionInfo {
                room: room.clone(),
                peers: peers.len(),
            })
            .collect();
        out.sort_by(|a, b| a.room.cmp(&b.room));
        out
    }

    /// Peers in `key` ON THIS NODE. See [`global_peer_count`](Self::global_peer_count).
    pub fn peer_count(&self, key: &str) -> usize {
        self.local
            .lock()
            .ok()
            .and_then(|g| g.sessions.get(key).map(HashMap::len))
            .unwrap_or(0)
    }

    /// Active sessions ON THIS NODE. See [`global_session_count`](Self::global_session_count).
    pub fn session_count(&self) -> usize {
        self.local.lock().map(|g| g.sessions.len()).unwrap_or(0)
    }

    /// The SHARED registry's sessions across all nodes (from the store).
    pub async fn global_list(&self) -> RegistryResult<Vec<SessionInfo>> {
        self.store.list().await
    }

    /// Peers in `key` across all nodes (from the shared registry store).
    pub async fn global_peer_count(&self, key: &str) -> RegistryResult<usize> {
        self.store.peer_count(key).await
    }

    /// Active sessions across all nodes (from the shared registry store).
    pub async fn global_session_count(&self) -> RegistryResult<usize> {
        self.store.session_count().await
    }
}

/// A `400 Bad Request` gate rejection carrying a plain-text reason.
pub(crate) fn bad_request(reason: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, reason.into()).into_response()
}

/// A `426 Upgrade Required` gate rejection: the client's engine is below the
/// game's declared minimum.
pub(crate) fn engine_too_old(engine: u32, min_engine: u32) -> Response {
    (
        StatusCode::UPGRADE_REQUIRED,
        format!("engine {engine} below required minimum {min_engine}; upgrade to join"),
    )
        .into_response()
}

/// Build the uniblox signaling server with the default [`SignalingConfig`]: the gate is OPEN (no
/// join token) and the default limits apply. For local development and in-process tests; a public
/// server uses [`build_signaling_server_with`] and a [`JoinTokenKey`].
pub fn build_signaling_server(
    addr: impl Into<SocketAddr>,
    registry: SessionRegistry,
) -> SignalingServer {
    build_signaling_server_with(addr, registry, SignalingConfig::default())
}

/// Build the uniblox signaling server (ADR-0037 to ADR-0040, ADR-0047, ADR-0048): a full-mesh
/// SDP/ICE relay over the matchbox wire protocol, with rooms isolated by their path, `?next=N`
/// session grouping, the gate (the per-IP attempt rate, the room, the join token when
/// `config.auth` holds a key, the asymmetric version filter, the `?next` and session caps, the
/// connection caps), the WebSocket message limits, the idle timeout, the bounded outbound queues,
/// and, with `config.issue_tokens` and a key, the token endpoint at [`TOKEN_PATH`]. It also starts
/// the registry heartbeat when called inside a tokio runtime.
pub fn build_signaling_server_with(
    addr: impl Into<SocketAddr>,
    registry: SessionRegistry,
    config: SignalingConfig,
) -> SignalingServer {
    // Keep this node alive in the shared registry (ADR-0048). Outside a runtime the server cannot
    // serve either; the host then calls `spawn_heartbeat` itself once it has one.
    if tokio::runtime::Handle::try_current().is_ok() {
        drop(registry.spawn_heartbeat());
    }
    server::build(addr.into(), registry, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `?next` room forgets its sub-session counter once its last session empties (the review's
    /// leak: one entry per room ever used).
    #[test]
    fn a_next_rooms_counter_is_dropped_once_it_empties() {
        let reg = SessionRegistry::new();
        let (tx, _rx) = PeerTx::detached();
        let a = PeerId(Uuid::new_v4());
        let b = PeerId(Uuid::new_v4());
        reg.join_local("room", a, tx.clone(), Some(2), 64).unwrap();
        reg.join_local("room", b, tx.clone(), Some(2), 64).unwrap();
        let counters = |reg: &SessionRegistry| reg.local.lock().unwrap().counter.len();
        assert_eq!(counters(&reg), 1);
        reg.leave_local(a);
        assert_eq!(counters(&reg), 1, "one member is left");
        reg.leave_local(b);
        assert_eq!(counters(&reg), 0);
        assert_eq!(reg.session_count(), 0);
    }

    fn query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_a_valid_scope() {
        assert_eq!(
            parse_scope("m1~7.2~3~arena"),
            Ok(Scope {
                mode: Mode::Standalone,
                content: 7,
                schema: 2,
                min_engine: 3,
                lobby: "arena".to_string(),
            })
        );
        assert_eq!(parse_scope("m2~10.0~7~lobby").unwrap().mode, Mode::P2p);
        assert_eq!(parse_scope("m3~0.0~0~x").unwrap().mode, Mode::Server);
    }

    #[test]
    fn rejects_malformed_scopes() {
        assert_eq!(parse_scope("m1~7.2~3"), Err(ScopeError::Shape)); // too few parts
        assert_eq!(parse_scope("m1~7.2~3~a~b"), Err(ScopeError::Shape)); // too many
        assert_eq!(parse_scope("x1~7.2~3~a"), Err(ScopeError::Mode)); // bad mode
        assert_eq!(parse_scope("m1~7~3~a"), Err(ScopeError::Schema)); // no schema field
        assert_eq!(parse_scope("m1~7.2.9~3~a"), Err(ScopeError::Schema)); // 3 fields
        assert_eq!(parse_scope("m1~a.2~3~a"), Err(ScopeError::Content)); // non-numeric content
        assert_eq!(parse_scope("m1~7.b~3~a"), Err(ScopeError::Schema)); // non-numeric schema
        assert_eq!(parse_scope("m1~7.2~x~a"), Err(ScopeError::Min)); // non-numeric min
        assert_eq!(parse_scope("m1~7.2~3~"), Err(ScopeError::Lobby)); // empty lobby
        assert_eq!(parse_scope("garbage"), Err(ScopeError::Shape)); // no delimiters
    }

    #[test]
    fn reads_the_engine_query_param() {
        assert_eq!(parse_engine(&query(&[("engine", "5")])), Ok(5));
        assert_eq!(parse_engine(&query(&[])), Err(ScopeError::Engine)); // missing
        assert_eq!(
            parse_engine(&query(&[("engine", "nope")])),
            Err(ScopeError::Engine) // non-numeric
        );
    }

    #[test]
    fn reads_the_next_query_param() {
        assert_eq!(parse_next(&query(&[])), Ok(None)); // absent ⇒ unbounded
        assert_eq!(parse_next(&query(&[("next", "4")])), Ok(Some(4)));
        assert_eq!(parse_next(&query(&[("next", "1")])), Ok(Some(1))); // boundary ok
        assert_eq!(parse_next(&query(&[("next", "0")])), Err(ScopeError::Next)); // 0 invalid
        assert_eq!(parse_next(&query(&[("next", "x")])), Err(ScopeError::Next)); // non-numeric
    }

    #[test]
    fn distinct_scopes_are_distinct_keys() {
        // The room string IS the scope key; the topology isolates by it, so these
        // never share a room (mode / content / schema / min / lobby each
        // discriminate — the client's own engine is NOT in the key).
        for (a, b) in [
            ("m1~7.2~3~arena", "m2~7.2~3~arena"), // mode differs
            ("m1~7.2~3~arena", "m1~9.2~3~arena"), // content differs
            ("m1~7.2~3~arena", "m1~7.9~3~arena"), // schema differs
            ("m1~7.2~3~arena", "m1~7.2~4~arena"), // min differs
            ("m1~7.2~3~arena", "m1~7.2~3~other"), // lobby differs
        ] {
            assert_ne!(a, b);
            assert_ne!(parse_scope(a).unwrap(), parse_scope(b).unwrap());
        }
    }
}
