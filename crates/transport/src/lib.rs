//! `transport` — the thin two-channel WebRTC-DataChannel abstraction over
//! matchbox (browser + native). **WebRTC DataChannels only — no media, no SFU.**
//!
//! Exactly two channels, in this fixed order (a settled invariant):
//! - [`CHANNEL_STATE`] (0): unreliable/unordered (`{ordered:false, max_retransmits:Some(0)}`)
//!   — last-write-wins entity state at the network tick.
//! - [`CHANNEL_EVENTS`] (1): reliable/ordered (`{ordered:true, max_retransmits:None}`)
//!   — durable events, ownership handoffs, anti-entropy resync.
//!
//! [`PeerId`] here is matchbox's transport-level UUID (assigned by the signaling
//! server) — distinct from `protocol::PeerId`. Mapping between them is a
//! session-layer concern that lands with replication/session join, not here.
//!
//! Kept deliberately THIN: the custom replication protocol builds on this seam,
//! and the native/server str0m abstraction (Phase 2) slots in behind the same API.

#[cfg(not(target_arch = "wasm32"))]
use matchbox_socket::RtcIceServerConfig;
use matchbox_socket::{WebRtcSocket, WebRtcSocketBuilder}; // only connect_hermetic (native-only) uses it

pub use matchbox_socket::{Error as TransportError, Packet, PeerId, PeerState};

/// The native/server str0m peer (ADR-0015) — sans-IO WebRTC, matchbox-interoperable.
#[cfg(not(target_arch = "wasm32"))]
mod str0m_peer;
#[cfg(not(target_arch = "wasm32"))]
pub use str0m_peer::Str0mPeer;

/// The message-loop future returned by [`Transport::connect`]. It drives
/// signaling + WebRTC and MUST be polled (natively: spawn it on the executor;
/// on wasm: `wasm_bindgen_futures::spawn_local`). It resolves only on
/// disconnect/error — if it is never polled, nothing happens at all.
pub type MessageLoopFuture = matchbox_socket::MessageLoopFuture;

/// Channel 0 — unreliable/unordered: entity-state snapshots.
pub const CHANNEL_STATE: usize = 0;
/// Channel 1 — reliable/ordered: durable events, handoffs, resync.
pub const CHANNEL_EVENTS: usize = 1;

/// Error sending on a channel: the channel is closed/taken or the index is
/// invalid (the two-channel layout is fixed, so the latter is a programmer error
/// surfaced as a `Result`, never a panic).
#[derive(Debug)]
pub struct ChannelSendError;

impl std::fmt::Display for ChannelSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transport channel unavailable or closed")
    }
}

impl std::error::Error for ChannelSendError {}

/// The transport's message loop has ended (signaling lost / socket closed /
/// loop future dropped). The socket is dead; callers should tear down the
/// session or reconnect — polling again will keep returning this.
#[derive(Debug)]
pub struct TransportClosed;

impl std::fmt::Display for TransportClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transport closed: message loop has ended")
    }
}

impl std::error::Error for TransportClosed {}

/// The two-channel P2P transport: a thin wrapper over a matchbox
/// [`WebRtcSocket`] with the settled channel layout baked in.
pub struct Transport {
    socket: WebRtcSocket,
}

impl Transport {
    /// Connect to a room (`ws://host:port/<room>`) with the two settled
    /// channels and matchbox's default ICE servers (public STUN) — the normal
    /// path for real sessions.
    pub fn connect(room_url: impl Into<String>) -> (Self, MessageLoopFuture) {
        Self::build(WebRtcSocketBuilder::new(room_url))
    }

    /// Connect with an EMPTY ICE server list: loopback/LAN host candidates
    /// only, no STUN, no outbound network — for hermetic native tests and
    /// offline local sessions.
    ///
    /// NATIVE-ONLY: matchbox's wasm path passes the ICE config straight into
    /// `RTCPeerConnection`, and browsers reject an ICE server entry with no
    /// URLs (`SyntaxError: ICE server parsing failed: Empty uri`). Browser
    /// callers use [`Transport::connect`]; on localhost, host candidates
    /// still connect (STUN is additional, not required).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn connect_hermetic(room_url: impl Into<String>) -> (Self, MessageLoopFuture) {
        Self::build(
            WebRtcSocketBuilder::new(room_url).ice_server(RtcIceServerConfig {
                urls: vec![],
                username: None,
                credential: None,
            }),
        )
    }

    fn build(builder: WebRtcSocketBuilder) -> (Self, MessageLoopFuture) {
        // Channel index = insertion order: unreliable first (0), reliable second (1).
        let (socket, loop_fut) = builder
            .add_unreliable_channel()
            .add_reliable_channel()
            .build();
        (Self { socket }, loop_fut)
    }

    /// Drain peer connect/disconnect updates. Call once per tick — this drives
    /// matchbox's peer bookkeeping. Returns [`TransportClosed`] once the
    /// message loop has ended (matchbox's `update_peers` would PANIC there —
    /// we use the non-panicking variant so a disconnect can never crash the
    /// per-tick path).
    pub fn poll_peers(&mut self) -> Result<Vec<(PeerId, PeerState)>, TransportClosed> {
        self.socket.try_update_peers().map_err(|_| TransportClosed)
    }

    /// Currently connected peers.
    pub fn connected_peers(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.socket.connected_peers()
    }

    /// Our own transport id — `None` until the signaling server has assigned it.
    pub fn id(&mut self) -> Option<PeerId> {
        self.socket.id()
    }

    /// Send an unreliable state snapshot (channel 0) to a peer.
    pub fn send_state(&mut self, peer: PeerId, packet: Packet) -> Result<(), ChannelSendError> {
        self.send_on(CHANNEL_STATE, peer, packet)
    }

    /// Send a reliable event (channel 1) to a peer.
    pub fn send_event(&mut self, peer: PeerId, packet: Packet) -> Result<(), ChannelSendError> {
        self.send_on(CHANNEL_EVENTS, peer, packet)
    }

    fn send_on(
        &mut self,
        channel: usize,
        peer: PeerId,
        packet: Packet,
    ) -> Result<(), ChannelSendError> {
        let ch = self
            .socket
            .get_channel_mut(channel)
            .map_err(|_| ChannelSendError)?;
        ch.try_send(packet, peer).map_err(|_| ChannelSendError)
    }

    /// Drain received state snapshots (channel 0).
    pub fn recv_state(&mut self) -> Vec<(PeerId, Packet)> {
        self.recv_on(CHANNEL_STATE)
    }

    /// Drain received events (channel 1).
    pub fn recv_events(&mut self) -> Vec<(PeerId, Packet)> {
        self.recv_on(CHANNEL_EVENTS)
    }

    fn recv_on(&mut self, channel: usize) -> Vec<(PeerId, Packet)> {
        match self.socket.get_channel_mut(channel) {
            Ok(ch) => ch.receive(),
            Err(_) => Vec::new(),
        }
    }

    /// Close the socket (all channels).
    pub fn close(&mut self) {
        self.socket.close()
    }
}
