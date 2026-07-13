//! uniblox signaling service: room-based WebRTC SDP/ICE signaling with a
//! `{mode, version}` scope, the ASYMMETRIC version filter (ADR-0038), and an
//! in-memory session registry.
//!
//! The scope is encoded in the ROOM PATH — `<mode>~<content>.<schema>~<min>~<lobby>`
//! (e.g. `m1~7.2~3~arena`: mode `m1`, content `7`, schema `2`, min-engine `3`,
//! lobby `arena`); the client sends its OWN engine version out-of-band as the
//! `?engine=N` query param. matchbox's FullMesh topology rooms peers strictly by
//! the path string, so:
//!
//! * **content + schema + lobby + min** are in the path ⇒ their exact match is
//!   structural (a different content/schema/lobby/min is a different room ⇒ never
//!   matched — no float-desync across incompatible peers).
//! * **the client's engine is NOT in the path** (it's the `?engine=` query) ⇒
//!   different-but-compatible engine versions share ONE room. The gate admits a
//!   join iff `engine >= min` — the ASYMMETRIC filter (engine releases are
//!   backward-compatible; content/schema must match exactly).
//!
//! The connection gate returns a REASONED rejection: `426 Upgrade Required` +
//! body for `engine < min`, `400 Bad Request` + body for a malformed scope or a
//! missing/non-numeric `?engine`. A plain single-token path (no `~`) is accepted
//! as a legacy room (keeps the `uniblox-demo` demo working; no engine gate). The
//! [`SessionRegistry`] tracks + lists sessions.
//!
//! **Trust model:** `?engine` and the path `min` are self-declared. This is
//! desync defense for HONEST clients (`CLAUDE.md`: the version gate is not
//! anti-cheat — a modified client can lie, but cannot lie its way into a
//! *stricter* room with an old engine, since a different `min` is a different
//! room).
//!
//! Deferred to later Phase-5 items: a custom `SignalingTopology` with
//! client-specified `?next=N` session-SIZE grouping; a shared Redis/Postgres
//! registry for horizontal scale; and signaling-DoS rate-limiting/auth.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use matchbox_protocol::PeerId;
use matchbox_signaling::SignalingServer;

/// The scope delimiter between a room path's tilde-separated parts.
const SCOPE_SEP: char = '~';

/// The `?engine=N` query key carrying the client's own engine version.
const ENGINE_PARAM: &str = "engine";

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

/// Why a `~`-shaped room path is not a valid scope, or a `?engine=` is missing.
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

/// One active session (room) in a [`SessionRegistry::list`] listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub room: String,
    pub peers: usize,
}

#[derive(Default)]
struct RegistryInner {
    /// Gate-stashed room per connecting address; consumed by `bridge` at id
    /// assignment, which runs in the SAME synchronous handler region as the gate
    /// (no yield between) — so a `pending` entry never persists across handlers.
    pending: HashMap<SocketAddr, String>,
    /// room → the peers currently in it (only successfully-CONNECTED peers — see
    /// `join`). This is what `list()` reports.
    sessions: HashMap<String, HashSet<PeerId>>,
    /// peer → its room: staged by `bridge` (id assignment), read by `join`
    /// (connect), consumed by `remove` (disconnect). An id-assigned peer whose
    /// upgrade never completes leaves a staged entry here — non-listed, memory
    /// only, bounded in practice by the deferred signaling rate-limiting.
    peer_room: HashMap<PeerId, String>,
}

/// In-memory registry of active signaling sessions (rooms + their peers). Cheap
/// to `clone` (shared `Arc`): the same handle is given to the connection gate,
/// the id-assignment + disconnect callbacks, and any lister. A poisoned lock
/// degrades to a no-op / empty listing rather than panicking the server.
#[derive(Clone, Default)]
pub struct SessionRegistry(Arc<Mutex<RegistryInner>>);

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Gate: remember the room a just-accepted connection is joining, keyed by
    /// its remote address (the only id available before id assignment).
    fn stash(&self, addr: SocketAddr, room: String) {
        if let Ok(mut g) = self.0.lock() {
            g.pending.insert(addr, room);
        }
    }

    /// Id assignment (fires PRE-upgrade, for every id-assigned connection): STAGE
    /// the `peer → room` mapping, bridging the gate's `addr → room`. The peer is
    /// NOT added to `sessions` yet — only a successfully-UPGRADED peer reaches
    /// [`join`](Self::join) (`on_peer_connected`, the balanced partner of
    /// [`remove`](Self::remove) / `on_peer_disconnected`). So a listed session
    /// only ever holds connected peers: an upgrade that fails after id assignment
    /// never over-reports (it leaves at most a non-listed `peer_room` staging
    /// entry, bounded in practice by the deferred signaling rate-limiting).
    fn bridge(&self, addr: SocketAddr, peer: PeerId) {
        if let Ok(mut g) = self.0.lock()
            && let Some(room) = g.pending.remove(&addr)
        {
            g.peer_room.insert(peer, room);
        }
    }

    /// Peer connected (POST-upgrade): add the staged peer to its session.
    fn join(&self, peer: PeerId) {
        if let Ok(mut g) = self.0.lock()
            && let Some(room) = g.peer_room.get(&peer).cloned()
        {
            g.sessions.entry(room).or_default().insert(peer);
        }
    }

    /// Disconnect: drop the peer from its session, pruning an emptied room.
    fn remove(&self, peer: PeerId) {
        if let Ok(mut g) = self.0.lock()
            && let Some(room) = g.peer_room.remove(&peer)
            && let Some(peers) = g.sessions.get_mut(&room)
        {
            peers.remove(&peer);
            if peers.is_empty() {
                g.sessions.remove(&room);
            }
        }
    }

    /// List active sessions (rooms + peer counts), sorted by room for determinism.
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

    /// Number of peers currently in `room`.
    pub fn peer_count(&self, room: &str) -> usize {
        self.0
            .lock()
            .ok()
            .and_then(|g| g.sessions.get(room).map(HashSet::len))
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

/// Build the scoped uniblox signaling server: matchbox FullMesh (rooms = URL
/// path, so content/schema/lobby/min in the path are isolated structurally) + a
/// gate implementing the ASYMMETRIC version filter (admit `?engine >= min`,
/// reasoned 426/400 rejection otherwise) + the `registry` wired to track
/// sessions (via the gate `origin` → id-assignment `PeerId` → disconnect
/// correlation).
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
    let join_reg = registry.clone();
    let leave_reg = registry;
    SignalingServer::full_mesh_builder(addr.into())
        .on_connection_request(move |meta| {
            let room = meta.path.clone().unwrap_or_default();
            if room.is_empty() {
                return Ok(false); // no room ⇒ bare 401
            }
            // A `~`-shaped path MUST be a well-formed scope AND carry a
            // `?engine=` that clears the declared minimum. A plain path is a
            // legacy room (accepted as-is — no engine gate).
            if room.contains(SCOPE_SEP) {
                let scope = parse_scope(&room).map_err(|e| bad_request(e.to_string()))?;
                let engine =
                    parse_engine(&meta.query_params).map_err(|e| bad_request(e.to_string()))?;
                if engine < scope.min_engine {
                    return Err(engine_too_old(engine, scope.min_engine));
                }
            }
            gate_reg.stash(meta.origin, room);
            Ok(true)
        })
        .on_id_assignment(move |(addr, peer)| bridge_reg.bridge(addr, peer))
        .on_peer_connected(move |peer| join_reg.join(peer))
        .on_peer_disconnected(move |peer| leave_reg.remove(peer))
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
    fn distinct_scopes_are_distinct_keys() {
        // The room string IS the scope key; FullMesh isolates by it, so these
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
