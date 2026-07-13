//! uniblox signaling service (ADR-0037): room-based WebRTC SDP/ICE signaling
//! with `{mode, version}` scoping and an in-memory session registry.
//!
//! The scope is encoded in the ROOM PATH — `<mode>~<engine>.<content>.<schema>~<lobby>`
//! (e.g. `m1~1.2.3~arena`). matchbox's FullMesh topology rooms peers strictly by
//! the path string, so peers with a different mode/version land in a DIFFERENT
//! room and are never matched — scoping is enforced structurally (offers/answers
//! only relay within one room). A connection gate rejects malformed scoped joins;
//! a plain single-token path (no `~`) is accepted as a legacy room (keeps the
//! `uniblox-demo` demo working). The [`SessionRegistry`] tracks + lists sessions.
//!
//! Deferred to later Phase-5 items: a custom `SignalingTopology` with
//! client-specified `?next=N` session-SIZE grouping; the ASYMMETRIC version
//! filter (engine ≥ minimum; content/schema exact — needs grouping
//! compatible-but-not-identical peers); a shared Redis/Postgres registry for
//! horizontal scale; and signaling-DoS rate-limiting/auth.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use matchbox_protocol::PeerId;
use matchbox_signaling::SignalingServer;
use protocol::VersionTriple;

/// The scope delimiter within a single room-path segment.
const SCOPE_SEP: char = '~';

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

/// A parsed matchmaking scope: two peers match only if their FULL scope (mode +
/// version + lobby) is identical (exact match — the asymmetric version filter is
/// a later item).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub mode: Mode,
    pub version: VersionTriple,
    pub lobby: String,
}

/// Why a `~`-shaped room path is not a valid scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeError {
    /// Not exactly `<mode>~<triple>~<lobby>`.
    Shape,
    /// The mode tag is not one of `m1` / `m2` / `m3`.
    Mode,
    /// The version triple is not three dot-separated `u32`s.
    Version,
    /// The lobby segment is empty.
    Lobby,
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ScopeError::Shape => "scope must be <mode>~<engine>.<content>.<schema>~<lobby>",
            ScopeError::Mode => "mode must be m1, m2, or m3",
            ScopeError::Version => "version must be three dot-separated u32s",
            ScopeError::Lobby => "lobby must be non-empty",
        })
    }
}

impl std::error::Error for ScopeError {}

/// Parse a scoped room path. Only meaningful for a path containing [`SCOPE_SEP`]
/// — a plain path (no `~`) is a legacy room, not a scope.
pub fn parse_scope(path: &str) -> Result<Scope, ScopeError> {
    let mut it = path.split(SCOPE_SEP);
    let mode = it.next().ok_or(ScopeError::Shape)?;
    let triple = it.next().ok_or(ScopeError::Shape)?;
    let lobby = it.next().ok_or(ScopeError::Shape)?;
    if it.next().is_some() {
        return Err(ScopeError::Shape); // more than three parts
    }
    let mode = Mode::parse(mode).ok_or(ScopeError::Mode)?;
    let version = parse_triple(triple)?;
    if lobby.is_empty() {
        return Err(ScopeError::Lobby);
    }
    Ok(Scope {
        mode,
        version,
        lobby: lobby.to_string(),
    })
}

fn parse_triple(s: &str) -> Result<VersionTriple, ScopeError> {
    let mut it = s.split('.');
    let engine = it.next().ok_or(ScopeError::Version)?;
    let content = it.next().ok_or(ScopeError::Version)?;
    let schema = it.next().ok_or(ScopeError::Version)?;
    if it.next().is_some() {
        return Err(ScopeError::Version); // more than three parts
    }
    Ok(VersionTriple {
        engine: engine.parse().map_err(|_| ScopeError::Version)?,
        content: content.parse().map_err(|_| ScopeError::Version)?,
        schema: schema.parse().map_err(|_| ScopeError::Version)?,
    })
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

/// Build the scoped uniblox signaling server: matchbox FullMesh (rooms = URL
/// path, so a mode/version scope in the path is isolated structurally) + a gate
/// that rejects malformed scoped joins + the `registry` wired to track sessions
/// (via the gate `origin` → id-assignment `PeerId` → disconnect correlation).
// The gate must return matchbox's `Result<bool, axum::Response>` (a large `Err`);
// we only ever return `Ok`, so `result_large_err` doesn't apply.
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
                return Ok(false); // no room ⇒ reject
            }
            // A `~`-shaped path MUST be a well-formed scope; a plain path is a
            // legacy room (accepted as-is).
            if room.contains(SCOPE_SEP) && parse_scope(&room).is_err() {
                return Ok(false); // malformed scope ⇒ 401, never enters a room
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

    fn triple(e: u32, c: u32, s: u32) -> VersionTriple {
        VersionTriple {
            engine: e,
            content: c,
            schema: s,
        }
    }

    #[test]
    fn parses_a_valid_scope() {
        assert_eq!(
            parse_scope("m1~1.2.3~arena"),
            Ok(Scope {
                mode: Mode::Standalone,
                version: triple(1, 2, 3),
                lobby: "arena".to_string(),
            })
        );
        assert_eq!(parse_scope("m2~10.0.7~lobby").unwrap().mode, Mode::P2p);
        assert_eq!(parse_scope("m3~0.0.0~x").unwrap().mode, Mode::Server);
    }

    #[test]
    fn rejects_malformed_scopes() {
        assert_eq!(parse_scope("m1~1.2.3"), Err(ScopeError::Shape)); // too few parts
        assert_eq!(parse_scope("m1~1.2.3~a~b"), Err(ScopeError::Shape)); // too many
        assert_eq!(parse_scope("x1~1.2.3~a"), Err(ScopeError::Mode)); // bad mode
        assert_eq!(parse_scope("m1~1.2~a"), Err(ScopeError::Version)); // short triple
        assert_eq!(parse_scope("m1~1.2.3.4~a"), Err(ScopeError::Version)); // long triple
        assert_eq!(parse_scope("m1~a.b.c~a"), Err(ScopeError::Version)); // non-numeric
        assert_eq!(parse_scope("m1~1.2.3~"), Err(ScopeError::Lobby)); // empty lobby
        assert_eq!(parse_scope("garbage"), Err(ScopeError::Shape)); // no delimiters
    }

    #[test]
    fn distinct_scopes_are_distinct_keys() {
        // The room string IS the scope key; FullMesh isolates by it, so these
        // never share a room (mode / version / lobby each discriminate).
        for (a, b) in [
            ("m1~1.2.3~arena", "m2~1.2.3~arena"), // mode differs
            ("m1~1.2.3~arena", "m1~9.2.3~arena"), // engine differs
            ("m1~1.2.3~arena", "m1~1.2.3~other"), // lobby differs
        ] {
            assert_ne!(a, b);
            assert_ne!(parse_scope(a).unwrap(), parse_scope(b).unwrap());
        }
    }
}
