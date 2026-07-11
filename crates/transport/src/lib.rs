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

use matchbox_socket::{ChannelConfig, RtcIceServerConfig, WebRtcSocket, WebRtcSocketBuilder};

pub use matchbox_socket::{Error as TransportError, Packet, PeerId, PeerState};

/// The native/server str0m peer (ADR-0015) — sans-IO WebRTC, matchbox-interoperable.
#[cfg(not(target_arch = "wasm32"))]
mod str0m_peer;
#[cfg(not(target_arch = "wasm32"))]
pub use str0m_peer::{IceOutcome, LocalCandidateKind, PeerTelemetry, Str0mPeer};

/// The message-loop future returned by [`Transport::connect`]. It drives
/// signaling + WebRTC and MUST be polled (natively: spawn it on the executor;
/// on wasm: `wasm_bindgen_futures::spawn_local`). It resolves only on
/// disconnect/error — if it is never polled, nothing happens at all.
pub type MessageLoopFuture = matchbox_socket::MessageLoopFuture;

/// Channel 0 — unreliable/unordered: entity-state snapshots.
pub const CHANNEL_STATE: usize = 0;
/// Channel 1 — reliable/ordered: durable events, handoffs, resync.
pub const CHANNEL_EVENTS: usize = 1;

/// One channel's delivery semantics (reliability/ordering/retransmit) — the
/// single source of truth both stacks derive their configs from: the matchbox
/// path in [`Transport`] and the str0m path in `Str0mPeer`. Semantics must
/// match on both sides for the negotiated (no-DCEP) channels to interoperate.
///
/// `maxPacketLifeTime` is deliberately unexpressed: matchbox 0.14 cannot set
/// it, and the WebRTC spec forbids setting both it and `maxRetransmits` — the
/// "at most one" constraint holds by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelSpec {
    /// Whether messages are guaranteed to arrive in order.
    pub ordered: bool,
    /// `None` = reliable (unlimited retransmits); `Some(n)` = unreliable,
    /// giving up after `n` retransmit attempts.
    pub max_retransmits: Option<u16>,
}

/// The settled two-channel layout. Array index = channel index = insertion
/// order = negotiated stream id. NEVER reorder the entries; the count is
/// fixed at two (a settled invariant — parameterize semantics here, never
/// the layout).
pub const CHANNEL_SPECS: [ChannelSpec; 2] = [
    // CHANNEL_STATE: last-write-wins snapshots — stale retransmits are useless.
    ChannelSpec {
        ordered: false,
        max_retransmits: Some(0),
    },
    // CHANNEL_EVENTS: durable events/handoffs/resync — must all arrive, in order.
    ChannelSpec {
        ordered: true,
        max_retransmits: None,
    },
];

/// ICE server policy for a session (ADR-0016). The tier decides it:
/// - **Free modes (1/2 P2P): STUN-only** — NAT discovery via public STUN,
///   no relay. Sessions that STUN cannot connect fail (the measured
///   failure rate is a fleet metric — see TODO).
/// - **Mode 3 (paid): STUN + TURN relay** — coturn, with **paid-only,
///   per-session credentials** provisioned by the platform at session join
///   (credential issuance is Phase-7 platform work; this type just carries
///   them).
///
/// Wasm-safe plain data; maps onto matchbox's `RtcIceServerConfig`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IceConfig {
    /// `stun:`/`turn:` URLs, all handed to the WebRTC stack as one server
    /// entry (matchbox exposes exactly one).
    pub urls: Vec<String>,
    /// TURN username (`None` for STUN-only).
    pub username: Option<String>,
    /// TURN credential (`None` for STUN-only).
    pub credential: Option<String>,
}

impl IceConfig {
    /// The free-tier default: public STUN, no relay. Identical servers to
    /// matchbox's own default (what plain [`Transport::connect`] uses).
    pub fn stun_only() -> Self {
        let defaults = RtcIceServerConfig::default();
        IceConfig {
            urls: defaults.urls,
            username: None,
            credential: None,
        }
    }

    /// Mode-3 paid tier: the given `turn:` URLs (+ credentials) alongside the
    /// STUN defaults. Credentials come from the platform per session — never
    /// bake long-lived TURN secrets into a client.
    pub fn with_turn(
        turn_urls: impl IntoIterator<Item = String>,
        username: impl Into<String>,
        credential: impl Into<String>,
    ) -> Self {
        let mut config = IceConfig::stun_only();
        config.urls.extend(turn_urls);
        config.username = Some(username.into());
        config.credential = Some(credential.into());
        config
    }
}

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

    /// Connect with an explicit ICE policy ([`IceConfig`]): STUN-only for the
    /// free tiers, STUN+TURN with per-session credentials for Mode 3.
    pub fn connect_with_ice(
        room_url: impl Into<String>,
        ice: IceConfig,
    ) -> (Self, MessageLoopFuture) {
        Self::build(
            WebRtcSocketBuilder::new(room_url).ice_server(RtcIceServerConfig {
                urls: ice.urls,
                username: ice.username,
                credential: ice.credential,
            }),
        )
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
        // Channel index = insertion order = CHANNEL_SPECS index.
        let (socket, loop_fut) = CHANNEL_SPECS
            .iter()
            .fold(builder, |b, spec| b.add_channel(matchbox_config(spec)))
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

/// Derive matchbox's channel config from a [`ChannelSpec`].
fn matchbox_config(spec: &ChannelSpec) -> ChannelConfig {
    ChannelConfig {
        ordered: spec.ordered,
        max_retransmits: spec.max_retransmits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks the settled two-channel layout: any change to count, order, or
    /// semantics must fail here loudly and be a deliberate decision.
    #[test]
    fn channel_specs_lock_the_settled_layout() {
        assert_eq!(CHANNEL_SPECS.len(), 2, "exactly two channels — settled");
        let state = CHANNEL_SPECS[CHANNEL_STATE];
        assert!(!state.ordered, "state channel is unordered");
        assert_eq!(
            state.max_retransmits,
            Some(0),
            "state channel never retransmits (LWW snapshots)"
        );
        let events = CHANNEL_SPECS[CHANNEL_EVENTS];
        assert!(events.ordered, "events channel is ordered");
        assert_eq!(
            events.max_retransmits, None,
            "events channel is reliable (unlimited retransmits)"
        );
    }

    /// Free tier = STUN-only: matchbox's default servers, no credentials.
    /// Mode 3 = the same plus TURN urls and per-session credentials.
    #[test]
    fn ice_config_tiers() {
        let free = IceConfig::stun_only();
        assert_eq!(free.urls, RtcIceServerConfig::default().urls);
        assert_eq!(free.username, None);
        assert_eq!(free.credential, None);

        let paid = IceConfig::with_turn(
            vec!["turn:relay.example:3478?transport=udp".to_string()],
            "session-user",
            "session-pass",
        );
        assert_eq!(&paid.urls[..free.urls.len()], &free.urls[..], "STUN kept");
        assert_eq!(
            paid.urls.last().map(String::as_str),
            Some("turn:relay.example:3478?transport=udp")
        );
        assert_eq!(paid.username.as_deref(), Some("session-user"));
        assert_eq!(paid.credential.as_deref(), Some("session-pass"));
    }

    /// The matchbox derivation must equal what matchbox's own
    /// `unreliable()`/`reliable()` helpers produced before parameterization —
    /// the semantic no-change proof.
    #[test]
    fn matchbox_derivation_matches_matchbox_helpers() {
        let derived_state = matchbox_config(&CHANNEL_SPECS[CHANNEL_STATE]);
        let helper_state = ChannelConfig::unreliable();
        assert_eq!(derived_state.ordered, helper_state.ordered);
        assert_eq!(derived_state.max_retransmits, helper_state.max_retransmits);

        let derived_events = matchbox_config(&CHANNEL_SPECS[CHANNEL_EVENTS]);
        let helper_events = ChannelConfig::reliable();
        assert_eq!(derived_events.ordered, helper_events.ordered);
        assert_eq!(
            derived_events.max_retransmits,
            helper_events.max_retransmits
        );
    }
}
