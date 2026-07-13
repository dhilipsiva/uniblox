//! uniblox signaling service: room-based WebRTC SDP/ICE signaling with a
//! `{mode, version}` scope + the ASYMMETRIC version filter (ADR-0038), optional
//! `?next=N` session-SIZE grouping via a custom topology (ADR-0039), and an
//! in-memory session registry.
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
//! **Session-SIZE grouping (`?next=N`, ADR-0039):** [`NextTopology`] is a custom
//! [`SignalingTopology`] — a full-mesh SDP/ICE relay (matchbox `FullMesh`
//! re-implemented from its contract) GENERALIZED with size grouping. `?next`
//! absent ⇒ ONE unbounded session per room (behaviourally identical to FullMesh);
//! `?next=N` ⇒ the room subdivides into sessions of at most N, keyed
//! `"<room>#<index>"`. Lifecycle is **batch-deal / no-backfill**: a session seals
//! when it reaches N and never refills — a departure shrinks it but new joiners
//! always fill the current open session (a fresh one once the last sealed). The
//! matchbox topology never sees the query, so `?next` is stashed by the gate
//! (keyed by `origin`) and bridged to the `PeerId` at id assignment, exactly like
//! the registry bookkeeping. The gate RESERVES `#` (the sub-session delimiter):
//! a room path may not contain it, so the `"<room>#<index>"` key namespace stays
//! disjoint from the raw-room-path namespace (no collision / cross-lobby leak).
//! `?next` is a per-lobby CONVENTION: peers naming the same room but disagreeing
//! on whether they send `?next` land in disjoint session namespaces (`R` vs
//! `R#0`) and are never matched — clients of one lobby must agree on it.
//!
//! The [`SessionRegistry`] IS the topology's shared state: it holds the relay
//! senders, the grouping bookkeeping, and the stashed `?next`, and lists sessions
//! (`list`/`peer_count`/`session_count`). A poisoned lock degrades to a no-op /
//! empty listing rather than panicking the server.
//!
//! **Trust model:** `?engine` and the path `min` are self-declared — desync
//! defense for HONEST clients (`CLAUDE.md`: the version gate is not anti-cheat; a
//! modified client can lie, but cannot lie into a *stricter* room with an old
//! engine, since a different `min` is a different room).
//!
//! Deferred to later Phase-5 items: a shared Redis/Postgres registry for
//! horizontal scale; the Mode-2 coordinator peer service; signaling-DoS
//! rate-limiting/auth.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::extract::ws::Message;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use matchbox_protocol::{JsonPeerEvent, JsonPeerRequest, PeerId};
use matchbox_signaling::common_logic::{SignalingChannel, parse_request, try_send};
use matchbox_signaling::{
    ClientRequestError, NoCallbacks, SignalingServer, SignalingServerBuilder, SignalingState,
    SignalingTopology, WsStateMeta,
};

/// The scope delimiter between a room path's tilde-separated parts.
const SCOPE_SEP: char = '~';

/// The `?engine=N` query key carrying the client's own engine version.
const ENGINE_PARAM: &str = "engine";

/// The `?next=N` query key carrying the desired session-SIZE cap.
const NEXT_PARAM: &str = "next";

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

/// Parse a scoped room path. Only meaningful for a path containing [`SCOPE_SEP`]
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

/// One active session in a [`SessionRegistry::list`] listing. `room` is the
/// session key: a room path (unbounded), or `"<room>#<index>"` when subdivided by
/// `?next=N`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub room: String,
    pub peers: usize,
}

#[derive(Default)]
struct RegistryInner {
    /// Gate-stashed `?next` size (None = unbounded) per connecting address;
    /// consumed by `bridge` at id assignment.
    pending: HashMap<SocketAddr, Option<usize>>,
    /// peer → requested session size, bridged from `pending` at id assignment
    /// (fires before the topology's `state_machine`); consumed (removed) by
    /// [`join`](SessionRegistry::join). A never-upgraded id-assigned peer leaves a
    /// staged entry — non-listed, memory only, bounded by the deferred rate-limit.
    peer_next: HashMap<PeerId, Option<usize>>,
    /// Live sessions keyed by session key: the room path (unbounded), or
    /// `"<room>#<index>"` when subdivided by `?next=N`. Maps each member to its
    /// relay sender. This is what `list()` reports.
    sessions: HashMap<String, HashMap<PeerId, SignalingChannel>>,
    /// The current OPEN (fillable) session key per room, for `?next=N` rooms only.
    /// Removed when the session seals (reaches N) ⇒ no backfill (ADR-0039).
    open: HashMap<String, String>,
    /// Monotonic sub-session index per room, for unique session keys.
    counter: HashMap<String, u64>,
    /// peer → its session key (reverse index for relay + `leave`).
    peer_session: HashMap<PeerId, String>,
}

/// In-memory state of active signaling sessions — the shared [`SignalingState`]
/// backing [`NextTopology`]. Holds each session's peers + relay senders, the
/// `?next` grouping bookkeeping, and the stashed `?next` awaiting id assignment.
/// Cheap to `clone` (shared `Arc`): the same handle is the topology state, the
/// gate's stash target, and the id-assignment bridge.
#[derive(Clone, Default)]
pub struct SessionRegistry(Arc<Mutex<RegistryInner>>);

impl SignalingState for SessionRegistry {}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Gate: stash the requested `?next` size for a connecting address (the only
    /// id available before id assignment).
    fn stash(&self, addr: SocketAddr, next: Option<usize>) {
        if let Ok(mut g) = self.0.lock() {
            g.pending.insert(addr, next);
        }
    }

    /// Id assignment (fires PRE-topology): bridge the stashed `?next` from the
    /// connecting `addr` to its assigned `PeerId`, so the topology can read it.
    fn bridge(&self, addr: SocketAddr, peer: PeerId) {
        if let Ok(mut g) = self.0.lock()
            && let Some(next) = g.pending.remove(&addr)
        {
            g.peer_next.insert(peer, next);
        }
    }

    /// Topology join: assign `peer` (connecting to `room`) to a session per the
    /// batch-deal / no-backfill rule, register its `sender`, and RETURN the
    /// senders of the peers ALREADY in that session (so the caller can broadcast
    /// `NewPeer` AFTER releasing the lock — the newcomer thus gets no self-event,
    /// matching FullMesh).
    fn join(&self, room: &str, peer: PeerId, sender: SignalingChannel) -> Vec<SignalingChannel> {
        let Ok(mut g) = self.0.lock() else {
            return Vec::new();
        };
        // Requested size: `None` if the peer carried no `?next` (or wasn't
        // bridged). `.flatten()` collapses "no entry" and "entry == None".
        let next = g.peer_next.remove(&peer).flatten();

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

        // Existing members' senders — captured BEFORE inserting the newcomer.
        let existing: Vec<SignalingChannel> = g
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
        g.peer_session.insert(peer, key);
        existing
    }

    /// The relay target for a `Signal`: `receiver`'s sender IFF it shares
    /// `peer`'s session (cross-session ⇒ `None`, the isolation boundary).
    fn target(&self, peer: PeerId, receiver: PeerId) -> Option<SignalingChannel> {
        let g = self.0.lock().ok()?;
        let key = g.peer_session.get(&peer)?;
        g.sessions.get(key)?.get(&receiver).cloned()
    }

    /// Topology exit: drop `peer` from its session (pruning an emptied session +
    /// clearing a dangling `open` pointer) and RETURN the remaining members'
    /// senders (so the caller can broadcast `PeerLeft` after releasing the lock).
    fn leave(&self, peer: PeerId) -> Vec<SignalingChannel> {
        let Ok(mut g) = self.0.lock() else {
            return Vec::new();
        };
        g.peer_next.remove(&peer); // in case the peer never reached `join`
        let Some(key) = g.peer_session.remove(&peer) else {
            return Vec::new();
        };
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
        }
        remaining
    }

    /// List active sessions (session keys + peer counts), sorted for determinism.
    pub fn list(&self) -> Vec<SessionInfo> {
        let Ok(g) = self.0.lock() else {
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

    /// Number of peers currently in the session keyed `key`.
    pub fn peer_count(&self, key: &str) -> usize {
        self.0
            .lock()
            .ok()
            .and_then(|g| g.sessions.get(key).map(HashMap::len))
            .unwrap_or(0)
    }

    /// Number of active sessions.
    pub fn session_count(&self) -> usize {
        self.0.lock().map(|g| g.sessions.len()).unwrap_or(0)
    }
}

/// A `400 Bad Request` gate rejection carrying a plain-text reason.
fn bad_request(reason: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, reason.into()).into_response()
}

/// A `426 Upgrade Required` gate rejection: the client's engine is below the
/// game's declared minimum.
fn engine_too_old(engine: u32, min_engine: u32) -> Response {
    (
        StatusCode::UPGRADE_REQUIRED,
        format!("engine {engine} below required minimum {min_engine}; upgrade to join"),
    )
        .into_response()
}

/// The custom uniblox signaling topology: a full-mesh SDP/ICE relay (matchbox
/// `FullMesh` re-implemented from its contract) GENERALIZED with `?next=N`
/// session-SIZE grouping. `?next` absent ⇒ one unbounded session per room
/// (FullMesh-equivalent); `?next=N` ⇒ the room subdivides into sessions of at
/// most N (sealed on fill; no backfill — ADR-0039). Bookkeeping is inline (a
/// custom topology has no `on_peer_connected`/`on_peer_disconnected`).
struct NextTopology;

#[async_trait]
impl SignalingTopology<NoCallbacks, SessionRegistry> for NextTopology {
    async fn state_machine(upgrade: WsStateMeta<NoCallbacks, SessionRegistry>) {
        let WsStateMeta {
            room,
            peer_id,
            sender,
            mut receiver,
            state,
            ..
        } = upgrade;

        // Assign a session; announce the newcomer to the peers ALREADY there
        // (they initiate the offers). `sender` is moved into the session map (it
        // is not used again — the relay writes to TARGET senders). The lock is
        // released before we broadcast.
        let existing = state.join(&room, peer_id, sender);
        broadcast(&existing, &JsonPeerEvent::NewPeer(peer_id));

        // Relay loop (mirrors matchbox FullMesh): forward each Signal to the named
        // receiver IFF it shares this peer's session; drop KeepAlive; a bad frame
        // is recoverable, a reset/close is fatal.
        while let Some(request) = receiver.next().await {
            let request = match parse_request(request) {
                Ok(req) => req,
                Err(ClientRequestError::Json(_) | ClientRequestError::UnsupportedType(_)) => {
                    continue; // recoverable — a malformed frame, keep the socket
                }
                Err(_) => break, // Axum (reset) / Close ⇒ done
            };
            match request {
                JsonPeerRequest::Signal {
                    receiver: target_id,
                    data,
                } => {
                    if let Some(target) = state.target(peer_id, target_id) {
                        let event = JsonPeerEvent::Signal {
                            sender: peer_id,
                            data,
                        };
                        let _ = try_send(&target, Message::Text(event.to_string().into()));
                    }
                    // An unknown / cross-session receiver silently drops — the
                    // session-isolation boundary.
                }
                JsonPeerRequest::KeepAlive => {}
            }
        }

        // Peer gone: drop it from its session, tell the remaining members.
        let remaining = state.leave(peer_id);
        broadcast(&remaining, &JsonPeerEvent::PeerLeft(peer_id));
    }
}

/// Best-effort broadcast of one event to many channels. MUST be called with the
/// registry lock released (never `try_send` while holding the mutex).
fn broadcast(channels: &[SignalingChannel], event: &JsonPeerEvent) {
    if channels.is_empty() {
        return;
    }
    let text = event.to_string();
    for ch in channels {
        let _ = try_send(ch, Message::Text(text.clone().into()));
    }
}

/// Build the uniblox signaling server: the custom [`NextTopology`] (scoped rooms
/// isolated structurally by the path; `?next=N` session-SIZE grouping) + a gate
/// implementing the ASYMMETRIC version filter (admit `?engine >= min`, reasoned
/// 426/400 otherwise) that also stashes `?next` + the `registry` as the shared
/// topology state (relay senders + grouping + listing), bridged `origin → PeerId`
/// at id assignment.
// The gate returns matchbox's `Result<bool, axum::Response>` — `Response` is a
// large `Err` type, but returning it IS the point (a reasoned rejection), so the
// `result_large_err` lint doesn't apply.
#[allow(clippy::result_large_err)]
pub fn build_signaling_server(
    addr: impl Into<SocketAddr>,
    registry: SessionRegistry,
) -> SignalingServer {
    let gate_reg = registry.clone();
    let bridge_reg = registry.clone();
    SignalingServerBuilder::<NextTopology, NoCallbacks, SessionRegistry>::new(
        addr.into(),
        NextTopology,
        registry,
    )
    .on_connection_request(move |meta| {
        let room = meta.path.clone().unwrap_or_default();
        if room.is_empty() {
            return Ok(false); // no room ⇒ bare 401
        }
        // Reserve the sub-session delimiter: a room that decodes to a `#`-bearing
        // string (e.g. `arena%230`) would collide with a `?next` sub-session key
        // `<room>#<index>` and cross the isolation boundary. Reject it (scoped AND
        // legacy paths).
        if room.contains(SESSION_SEP) {
            return Err(bad_request(format!("room may not contain '{SESSION_SEP}'")));
        }
        // A `~`-shaped path MUST be a well-formed scope AND carry a `?engine=`
        // that clears the declared minimum. A plain path is a legacy room.
        if room.contains(SCOPE_SEP) {
            let scope = parse_scope(&room).map_err(|e| bad_request(e.to_string()))?;
            let engine =
                parse_engine(&meta.query_params).map_err(|e| bad_request(e.to_string()))?;
            if engine < scope.min_engine {
                return Err(engine_too_old(engine, scope.min_engine));
            }
        }
        // Optional session-SIZE cap — orthogonal to the scope (also applies to
        // legacy plain rooms).
        let next = parse_next(&meta.query_params).map_err(|e| bad_request(e.to_string()))?;
        gate_reg.stash(meta.origin, next);
        Ok(true)
    })
    .on_id_assignment(move |(addr, peer)| bridge_reg.bridge(addr, peer))
    .cors()
    .build()
}

#[cfg(test)]
mod tests {
    use super::*;

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
