//! `Str0mPeer` — the native/server WebRTC peer built on sans-IO str0m
//! (ADR-0015). Speaks matchbox's signaling protocol and negotiates matchbox's
//! pre-negotiated (no-DCEP) DataChannels, so it interoperates with matchbox
//! peers (native webrtc-rs and browser) in the same room.
//!
//! The matchbox wire contract this implements (verified from vendored 0.14
//! sources — see ADR-0015):
//! - Signaling: WS text JSON, externally tagged. In: `{"IdAssigned":uuid}`,
//!   `{"NewPeer":uuid}`, `{"PeerLeft":uuid}`, `{"Signal":{"sender":..,"data":..}}`.
//!   Out: `{"Signal":{"receiver":..,"data":..}}`, bare `"KeepAlive"` (~10 s).
//!   `PeerSignal::{Offer,Answer}` carry RAW SDP strings; `IceCandidate` carries
//!   a DOUBLE-ENCODED JSON `RTCIceCandidateInit`; browsers also send the
//!   `"null"` end-of-candidates sentinel (tolerated).
//! - Roles: existing peers receive `NewPeer(joiner)` and OFFER; the newcomer
//!   answers unsolicited offers. Both directions are implemented here.
//! - Channels are `negotiated` (NO DCEP): both sides pre-create stream id 0
//!   (label `matchbox_socket_0`, unreliable/unordered, max_retransmits=0) and
//!   stream id 1 (`matchbox_socket_1`, reliable/ordered) — matching
//!   [`crate::CHANNEL_STATE`]/[`crate::CHANNEL_EVENTS`]. NEVER reorder.
//! - A peer is Connected when ALL channels are open.
//!
//! Threading model ("the driving loop", human-reviewed per the Phase-2 item):
//! one blocking signaling thread (tungstenite; read-timeout loop that also
//! drains an outbound queue and keeps alive) + ONE CONNECTION THREAD PER
//! REMOTE PEER, each owning a UDP socket and an `Rtc`, running the canonical
//! sans-IO loop. str0m's hard invariant is honored structurally: after EVERY
//! mutation (`handle_input`, command application, SDP change) the loop drains
//! `poll_output()` to `Output::Timeout` before the next mutation.
//!
//! Limitations (slice scope): `ws://` signaling only (no TLS); loopback/local
//! UDP binding; peer threads exit on disconnect and are not restarted
//! (reconnect/ICE-restart is a later Phase-2 item).

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use matchbox_protocol::{PeerEvent, PeerId as MbPeerId, PeerRequest};
use serde::{Deserialize, Serialize};
use str0m::channel::{ChannelConfig as Str0mChannelConfig, ChannelId, Reliability};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, CandidateKind, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

use crate::{
    CHANNEL_EVENTS, CHANNEL_SPECS, CHANNEL_STATE, ChannelSendError, ChannelSpec, Packet, PeerState,
    TransportClosed,
};

/// matchbox's `PeerSignal` (defined in matchbox_socket, not exported) —
/// identical shape ⇒ identical externally-tagged JSON.
#[derive(Serialize, Deserialize, Clone, Debug)]
enum PeerSignal {
    IceCandidate(String),
    Offer(String),
    Answer(String),
}

/// The inner JSON of `PeerSignal::IceCandidate` (webrtc-rs `RTCIceCandidateInit`).
#[derive(Serialize, Deserialize, Debug)]
struct IceCandidateJson {
    candidate: String,
    #[serde(rename = "sdpMid", default, skip_serializing_if = "Option::is_none")]
    sdp_mid: Option<String>,
    #[serde(
        rename = "sdpMLineIndex",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    sdp_mline_index: Option<u16>,
    #[serde(rename = "usernameFragment", default)]
    username_fragment: Option<String>,
}

enum Cmd {
    Send { channel: usize, data: Box<[u8]> },
    Signal(PeerSignal),
    Close,
}

enum Role {
    /// We received `NewPeer` for this remote — we make the offer.
    Offerer,
    /// The remote's offer arrives via `Signal` — we answer.
    Answerer,
}

/// How str0m's stats poll reports RTT/candidate info (ADR-0018): enabling it
/// makes str0m emit [`Event::PeerStats`] on this cadence.
const STATS_INTERVAL: Duration = Duration::from_millis(500);

/// Bound on how long we wait for a peer to reach `Connected` before recording
/// the attempt as [`IceOutcome::Failed`]. On a STUN-only config this is the
/// signal that STUN could not traverse the NAT (the failure the metric counts).
const CONNECT_DEADLINE: Duration = Duration::from_secs(30);

/// Kind of the local winning ICE candidate — the "did we need TURN" signal
/// (ADR-0018). Mapped from [`str0m::CandidateKind`]. `Host` today: `Str0mPeer`
/// gathers only a host candidate (srflx/relay await the non-loopback-bind /
/// STUN-gathering work), but the classification is future-proof for when they
/// appear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalCandidateKind {
    Host,
    ServerReflexive,
    PeerReflexive,
    Relayed,
    /// The selected local address matched none of our gathered candidates.
    Unknown,
}

/// Whether a peer connection succeeded — the STUN-only-failure-rate signal
/// (ADR-0018). Fleet aggregate: success fraction = `Connected` / attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IceOutcome {
    /// Negotiating; not yet connected.
    Connecting,
    /// Reached `Connected` at least once (the pair nominated).
    Connected,
    /// Never connected (deadline, ICE disconnect before connect, or dead remote).
    Failed,
}

/// Per-peer connection telemetry (ADR-0018). A deployed fleet aggregates these
/// into the STUN-only success fraction and the RTT/jitter distributions the
/// measurement gap asks for. Loopback/host on a single machine; meaningful the
/// moment peers are remote.
#[derive(Clone, Debug)]
pub struct PeerTelemetry {
    /// Connection outcome.
    pub outcome: IceOutcome,
    /// Time from the connection attempt starting to the first `Connected`.
    pub time_to_connect: Option<Duration>,
    /// Kind of our winning local candidate (the TURN-needed signal).
    pub local_candidate: LocalCandidateKind,
    /// Address of our winning local candidate.
    pub selected_local_addr: Option<SocketAddr>,
    /// Address of the remote's winning candidate.
    pub selected_remote_addr: Option<SocketAddr>,
    /// Number of RTT samples folded in (from str0m's ICE keepalive stats).
    pub rtt_samples: u64,
    /// Mean RTT over the samples.
    pub rtt_mean: Option<Duration>,
    /// RTT jitter (standard deviation of the samples).
    pub rtt_jitter: Option<Duration>,
}

impl PeerTelemetry {
    fn connecting() -> Self {
        PeerTelemetry {
            outcome: IceOutcome::Connecting,
            time_to_connect: None,
            local_candidate: LocalCandidateKind::Unknown,
            selected_local_addr: None,
            selected_remote_addr: None,
            rtt_samples: 0,
            rtt_mean: None,
            rtt_jitter: None,
        }
    }
}

fn map_candidate_kind(kind: CandidateKind) -> LocalCandidateKind {
    match kind {
        CandidateKind::Host => LocalCandidateKind::Host,
        CandidateKind::ServerReflexive => LocalCandidateKind::ServerReflexive,
        CandidateKind::PeerReflexive => LocalCandidateKind::PeerReflexive,
        CandidateKind::Relayed => LocalCandidateKind::Relayed,
    }
}

struct Shared {
    id: Mutex<Option<MbPeerId>>,
    conns: Mutex<HashMap<MbPeerId, Sender<Cmd>>>,
    /// Per-peer connection telemetry (ADR-0018); updated by the connection
    /// threads, read via [`Str0mPeer::telemetry`].
    telemetry: Mutex<HashMap<MbPeerId, PeerTelemetry>>,
    closed: AtomicBool,
}

/// Write a peer's telemetry snapshot into the shared map (best-effort — a
/// poisoned lock silently drops the update, never blocks the connection loop).
fn commit_telemetry(shared: &Shared, remote: MbPeerId, telem: &PeerTelemetry) {
    if let Ok(mut map) = shared.telemetry.lock() {
        map.insert(remote, telem.clone());
    }
}

/// Finalize a peer's telemetry when its connection thread exits: an attempt
/// that never reached `Connected` (or recorded nothing) becomes `Failed`. A
/// peer that connected then dropped STAYS `Connected` — it DID connect; that is
/// a disconnect, not a STUN failure.
fn finalize_failed_telemetry(map: &mut HashMap<MbPeerId, PeerTelemetry>, remote: MbPeerId) {
    match map.get_mut(&remote) {
        Some(telem) if telem.outcome == IceOutcome::Connecting => {
            telem.outcome = IceOutcome::Failed;
        }
        Some(_) => {}
        None => {
            let mut failed = PeerTelemetry::connecting();
            failed.outcome = IceOutcome::Failed;
            map.insert(remote, failed);
        }
    }
}

/// The native str0m peer. Mirrors [`crate::Transport`]'s method surface so
/// pump code is transport-agnostic.
pub struct Str0mPeer {
    shared: Arc<Shared>,
    updates_rx: Receiver<(MbPeerId, PeerState)>,
    state_rx: Receiver<(MbPeerId, Packet)>,
    events_rx: Receiver<(MbPeerId, Packet)>,
}

impl Str0mPeer {
    /// Connect to a matchbox signaling room (`ws://host:port/<room>`) and
    /// negotiate with every peer in it. Non-blocking: threads run inside.
    pub fn connect(room_url: &str) -> Str0mPeer {
        install_crypto_provider();

        let shared = Arc::new(Shared {
            id: Mutex::new(None),
            conns: Mutex::new(HashMap::new()),
            telemetry: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        let (updates_tx, updates_rx) = std::sync::mpsc::channel();
        let (state_tx, state_rx) = std::sync::mpsc::channel();
        let (events_tx, events_rx) = std::sync::mpsc::channel();

        let url = room_url.to_string();
        let shared2 = Arc::clone(&shared);
        std::thread::spawn(move || {
            run_signaling(&url, &shared2, updates_tx, state_tx, events_tx);
            shared2.closed.store(true, Ordering::SeqCst);
        });

        Str0mPeer {
            shared,
            updates_rx,
            state_rx,
            events_rx,
        }
    }

    /// Drain peer connect/disconnect updates. Mirrors `Transport::poll_peers`.
    pub fn poll_peers(&mut self) -> Result<Vec<(MbPeerId, PeerState)>, TransportClosed> {
        let mut updates = Vec::new();
        loop {
            match self.updates_rx.try_recv() {
                Ok(update) => updates.push(update),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if updates.is_empty() {
                        return Err(TransportClosed);
                    }
                    break;
                }
            }
        }
        if updates.is_empty() && self.shared.closed.load(Ordering::SeqCst) {
            return Err(TransportClosed);
        }
        Ok(updates)
    }

    /// Our signaling-assigned id — `None` until `IdAssigned` arrives.
    pub fn id(&mut self) -> Option<MbPeerId> {
        match self.shared.id.lock() {
            Ok(id) => *id,
            Err(_) => None,
        }
    }

    pub fn send_state(&mut self, peer: MbPeerId, data: Packet) -> Result<(), ChannelSendError> {
        self.send_on(CHANNEL_STATE, peer, data)
    }

    pub fn send_event(&mut self, peer: MbPeerId, data: Packet) -> Result<(), ChannelSendError> {
        self.send_on(CHANNEL_EVENTS, peer, data)
    }

    fn send_on(
        &mut self,
        channel: usize,
        peer: MbPeerId,
        data: Packet,
    ) -> Result<(), ChannelSendError> {
        let conns = self.shared.conns.lock().map_err(|_| ChannelSendError)?;
        let tx = conns.get(&peer).ok_or(ChannelSendError)?;
        tx.send(Cmd::Send { channel, data })
            .map_err(|_| ChannelSendError)
    }

    /// Snapshot of per-peer connection telemetry (ADR-0018): ICE outcome,
    /// time-to-connect, winning local-candidate kind, and RTT mean/jitter from
    /// str0m's ICE keepalive stats. A fleet aggregates these into the STUN-only
    /// success fraction and RTT/jitter distributions.
    pub fn telemetry(&self) -> Vec<(MbPeerId, PeerTelemetry)> {
        match self.shared.telemetry.lock() {
            Ok(map) => map.iter().map(|(id, t)| (*id, t.clone())).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn recv_state(&mut self) -> Vec<(MbPeerId, Packet)> {
        self.state_rx.try_iter().collect()
    }

    pub fn recv_events(&mut self) -> Vec<(MbPeerId, Packet)> {
        self.events_rx.try_iter().collect()
    }

    /// Close all connections and the signaling loop.
    pub fn close(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Ok(conns) = self.shared.conns.lock() {
            for tx in conns.values() {
                let _ = tx.send(Cmd::Close);
            }
        }
    }
}

impl Drop for Str0mPeer {
    fn drop(&mut self) {
        self.close();
    }
}

fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        str0m::crypto::from_feature_flags().install_process_default();
    });
}

// ───────────────────────── signaling thread ─────────────────────────

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const WS_POLL: Duration = Duration::from_millis(20);

fn run_signaling(
    url: &str,
    shared: &Arc<Shared>,
    updates_tx: Sender<(MbPeerId, PeerState)>,
    state_tx: Sender<(MbPeerId, Packet)>,
    events_tx: Sender<(MbPeerId, Packet)>,
) {
    let (mut ws, _resp) = match tungstenite::connect(url) {
        Ok(ok) => ok,
        Err(err) => {
            log::error!("[str0m] signaling connect failed: {err}");
            return;
        }
    };
    // Read with a timeout so the loop can also drain outbound signals.
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = ws.get_ref()
        && stream.set_read_timeout(Some(WS_POLL)).is_err()
    {
        log::error!("[str0m] cannot set WS read timeout");
        return;
    }

    // Connection threads push outbound requests here; we own the WS writer.
    let (out_tx, out_rx) = std::sync::mpsc::channel::<PeerRequest<PeerSignal>>();
    let mut last_keepalive = Instant::now();

    loop {
        if shared.closed.load(Ordering::SeqCst) {
            return;
        }

        // Outbound: signals from connection threads + keepalive.
        while let Ok(request) = out_rx.try_recv() {
            match serde_json::to_string(&request) {
                Ok(text) => {
                    if let Err(err) = ws.send(tungstenite::Message::text(text)) {
                        log::error!("[str0m] signaling send failed: {err}");
                        return;
                    }
                }
                Err(err) => log::error!("[str0m] signaling encode failed: {err}"),
            }
        }
        if last_keepalive.elapsed() >= KEEPALIVE_INTERVAL {
            last_keepalive = Instant::now();
            let keepalive: PeerRequest<PeerSignal> = PeerRequest::KeepAlive;
            match serde_json::to_string(&keepalive) {
                Ok(text) => {
                    if let Err(err) = ws.send(tungstenite::Message::text(text)) {
                        log::error!("[str0m] keepalive failed: {err}");
                        return;
                    }
                }
                Err(err) => log::error!("[str0m] keepalive encode failed: {err}"),
            }
        }

        // Inbound.
        let message = match ws.read() {
            Ok(message) => message,
            Err(tungstenite::Error::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(err) => {
                log::error!("[str0m] signaling read failed: {err}");
                close_all(shared);
                return;
            }
        };
        let text = match message {
            tungstenite::Message::Text(text) => text,
            tungstenite::Message::Close(_) => {
                log::info!("[str0m] signaling closed by server");
                close_all(shared);
                return;
            }
            _ => continue, // ping/pong/binary — tungstenite handles ping replies
        };
        let event: PeerEvent<PeerSignal> = match serde_json::from_str(&text) {
            Ok(event) => event,
            Err(err) => {
                log::warn!("[str0m] undecodable signaling frame ({err}): {text}");
                continue;
            }
        };

        match event {
            PeerEvent::IdAssigned(id) => {
                if let Ok(mut slot) = shared.id.lock() {
                    *slot = Some(id);
                }
            }
            PeerEvent::NewPeer(remote) => {
                // We are the existing peer — we offer.
                spawn_connection(
                    Role::Offerer,
                    remote,
                    shared,
                    &out_tx,
                    &updates_tx,
                    &state_tx,
                    &events_tx,
                );
            }
            PeerEvent::PeerLeft(remote) => {
                if let Ok(mut conns) = shared.conns.lock()
                    && let Some(tx) = conns.remove(&remote)
                {
                    let _ = tx.send(Cmd::Close);
                }
                let _ = updates_tx.send((remote, PeerState::Disconnected));
            }
            PeerEvent::Signal { sender, data } => {
                let tx = {
                    let conns = match shared.conns.lock() {
                        Ok(conns) => conns,
                        Err(_) => {
                            log::error!("[str0m] conns lock poisoned — closing signaling");
                            close_all(shared);
                            return;
                        }
                    };
                    conns.get(&sender).cloned()
                };
                let tx = match tx {
                    Some(tx) => tx,
                    None => {
                        // Unsolicited OFFER from an unknown peer: we are the
                        // newcomer and they initiate. Anything else from an
                        // unknown sender is a stray (e.g. a signal racing
                        // PeerLeft) — spawning an answerer for it would create
                        // a ghost connection that waits on an offer that never
                        // comes.
                        if !matches!(data, PeerSignal::Offer(_)) {
                            log::debug!("[str0m] stray non-offer signal from unknown {sender}");
                            continue;
                        }
                        spawn_connection(
                            Role::Answerer,
                            sender,
                            shared,
                            &out_tx,
                            &updates_tx,
                            &state_tx,
                            &events_tx,
                        )
                    }
                };
                let _ = tx.send(Cmd::Signal(data));
            }
        }
    }
}

fn close_all(shared: &Arc<Shared>) {
    if let Ok(conns) = shared.conns.lock() {
        for tx in conns.values() {
            let _ = tx.send(Cmd::Close);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_connection(
    role: Role,
    remote: MbPeerId,
    shared: &Arc<Shared>,
    out_tx: &Sender<PeerRequest<PeerSignal>>,
    updates_tx: &Sender<(MbPeerId, PeerState)>,
    state_tx: &Sender<(MbPeerId, Packet)>,
    events_tx: &Sender<(MbPeerId, Packet)>,
) -> Sender<Cmd> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Cmd>();
    if let Ok(mut conns) = shared.conns.lock() {
        conns.insert(remote, cmd_tx.clone());
    }
    let out_tx = out_tx.clone();
    let updates_tx = updates_tx.clone();
    let state_tx = state_tx.clone();
    let events_tx = events_tx.clone();
    let shared = Arc::clone(shared);
    std::thread::spawn(move || {
        if let Err(err) = run_connection(
            role,
            remote,
            cmd_rx,
            &shared,
            &out_tx,
            &updates_tx,
            &state_tx,
            &events_tx,
        ) {
            log::warn!("[str0m] connection to {remote} ended: {err}");
        }
        // An attempt that never reached `Connected` is a `Failed` connection —
        // the STUN-only failure the metric counts.
        if let Ok(mut map) = shared.telemetry.lock() {
            finalize_failed_telemetry(&mut map, remote);
        }
        if let Ok(mut conns) = shared.conns.lock() {
            conns.remove(&remote);
        }
        let _ = updates_tx.send((remote, PeerState::Disconnected));
    });
    cmd_tx
}

// ───────────────────────── connection thread ─────────────────────────

/// The two matchbox channels, pre-negotiated (no DCEP), derived from the
/// shared [`CHANNEL_SPECS`] source of truth: stream id == channel index ==
/// spec index; labels follow matchbox's `matchbox_socket_{i}` convention.
fn channel_configs() -> [Str0mChannelConfig; 2] {
    [
        str0m_config(&CHANNEL_SPECS[0], 0),
        str0m_config(&CHANNEL_SPECS[1], 1),
    ]
}

/// Derive str0m's channel config from a [`ChannelSpec`] at stream id `index`.
fn str0m_config(spec: &ChannelSpec, index: u16) -> Str0mChannelConfig {
    Str0mChannelConfig {
        label: format!("matchbox_socket_{index}"),
        ordered: spec.ordered,
        reliability: match spec.max_retransmits {
            None => Reliability::Reliable,
            Some(retransmits) => Reliability::MaxRetransmits { retransmits },
        },
        negotiated: Some(index),
        protocol: String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_connection(
    role: Role,
    remote: MbPeerId,
    cmd_rx: Receiver<Cmd>,
    shared: &Shared,
    out_tx: &Sender<PeerRequest<PeerSignal>>,
    updates_tx: &Sender<(MbPeerId, PeerState)>,
    state_tx: &Sender<(MbPeerId, Packet)>,
    events_tx: &Sender<(MbPeerId, Packet)>,
) -> Result<(), String> {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| format!("udp bind: {e}"))?;
    let local_addr = socket
        .local_addr()
        .map_err(|e| format!("local addr: {e}"))?;

    // Stats ON (default is off) so str0m emits Event::PeerStats with the ICE
    // RTT + selected candidate pair — the telemetry source (ADR-0018).
    let mut rtc = RtcConfig::new()
        .set_stats_interval(Some(STATS_INTERVAL))
        .build(Instant::now());
    let candidate =
        Candidate::host(local_addr, "udp").map_err(|e| format!("host candidate: {e}"))?;
    // Remember our gathered candidates + their kinds so a PeerStats selected
    // pair (which carries only the addr) can be classified back to a kind.
    let local_candidates: Vec<(SocketAddr, LocalCandidateKind)> =
        vec![(candidate.addr(), map_candidate_kind(candidate.kind()))];
    rtc.add_local_candidate(candidate.clone());
    let candidate_json = encode_candidate(&candidate)?;

    // Telemetry state for this peer (ADR-0018). Written into `shared.telemetry`
    // on the first Connected and on each PeerStats update.
    let connect_start = Instant::now();
    let mut telem = PeerTelemetry::connecting();
    commit_telemetry(shared, remote, &telem);
    // RTT accumulator (µs) → running mean + stddev (jitter).
    let mut rtt_count: u64 = 0;
    let mut rtt_sum_us: f64 = 0.0;
    let mut rtt_sum_sq_us: f64 = 0.0;

    let mut chan_ids: [Option<ChannelId>; 2] = [None, None];
    let mut chan_open: [bool; 2] = [false, false];
    let mut pending_offer = None;

    if let Role::Offerer = role {
        let mut sdp = rtc.sdp_api();
        let [cfg0, cfg1] = channel_configs();
        chan_ids[0] = Some(sdp.add_channel_with_config(cfg0));
        chan_ids[1] = Some(sdp.add_channel_with_config(cfg1));
        let (offer, pending) = sdp
            .apply()
            .ok_or_else(|| "offer apply produced no changes".to_string())?;
        pending_offer = Some(pending);
        send_signal(out_tx, remote, PeerSignal::Offer(offer.to_sdp_string()));
        // Trickle our host candidate AFTER the offer: native matchbox drops
        // candidates that arrive before it has the offer/answer it is waiting
        // on (its handshake loops ignore out-of-phase signals). The answerer
        // trickles after sending its Answer, in `apply_signal`.
        send_signal(
            out_tx,
            remote,
            PeerSignal::IceCandidate(candidate_json.clone()),
        );
    }

    let mut connected_reported = false;
    let mut buf = vec![0u8; 2000];

    loop {
        // Drain the Rtc to Timeout (the sans-IO invariant: a full drain after
        // every mutation; `handle_input` at the bottom of the loop and each
        // command application below are followed by re-entering this drain).
        let deadline = loop {
            match rtc.poll_output().map_err(|e| format!("poll_output: {e}"))? {
                Output::Timeout(deadline) => break deadline,
                Output::Transmit(transmit) => {
                    if let Err(err) = socket.send_to(&transmit.contents, transmit.destination) {
                        log::warn!("[str0m] udp send to {} failed: {err}", transmit.destination);
                    }
                }
                Output::Event(event) => match event {
                    Event::ChannelOpen(id, label) => {
                        // Negotiated channels: map by stream id via our configs'
                        // order (label carries the index for belt-and-braces).
                        for (index, slot) in chan_ids.iter_mut().enumerate() {
                            let expected = format!("matchbox_socket_{index}");
                            if label == expected {
                                *slot = Some(id);
                                chan_open[index] = true;
                            }
                        }
                        if chan_open.iter().all(|open| *open) && !connected_reported {
                            connected_reported = true;
                            telem.outcome = IceOutcome::Connected;
                            telem.time_to_connect = Some(connect_start.elapsed());
                            commit_telemetry(shared, remote, &telem);
                            let _ = updates_tx.send((remote, PeerState::Connected));
                        }
                    }
                    Event::ChannelData(data) => {
                        let index = chan_ids.iter().position(|slot| *slot == Some(data.id));
                        match index {
                            Some(CHANNEL_STATE) => {
                                let _ = state_tx.send((remote, data.data.into_boxed_slice()));
                            }
                            Some(CHANNEL_EVENTS) => {
                                let _ = events_tx.send((remote, data.data.into_boxed_slice()));
                            }
                            _ => log::warn!("[str0m] data on unknown channel {:?}", data.id),
                        }
                    }
                    Event::ChannelClose(_) => {
                        return Err("channel closed".to_string());
                    }
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        return Err("ice disconnected".to_string());
                    }
                    // Telemetry (ADR-0018): fold the ICE RTT + selected pair.
                    // Read-only — NOT an Rtc mutation, so the drain invariant is
                    // untouched (this is just another Event in the drain).
                    Event::PeerStats(stats) => {
                        if let Some(pair) = stats.selected_candidate_pair {
                            telem.selected_local_addr = Some(pair.local.addr);
                            telem.selected_remote_addr = Some(pair.remote.addr);
                            telem.local_candidate = local_candidates
                                .iter()
                                .find(|(addr, _)| *addr == pair.local.addr)
                                .map(|(_, kind)| *kind)
                                .unwrap_or(LocalCandidateKind::Unknown);
                            // The RTT comes from the ICE keepalive on the
                            // nominated pair (`current_round_trip_time`) — NOT
                            // `stats.rtt`, which is RTP/media-derived and stays
                            // None for a DataChannels-only session.
                            if let Some(rtt) = pair.current_round_trip_time {
                                let us = rtt.as_secs_f64() * 1e6;
                                rtt_count += 1;
                                rtt_sum_us += us;
                                rtt_sum_sq_us += us * us;
                                let n = rtt_count as f64;
                                let mean_us = rtt_sum_us / n;
                                let var_us = (rtt_sum_sq_us / n - mean_us * mean_us).max(0.0);
                                telem.rtt_samples = rtt_count;
                                telem.rtt_mean = Some(Duration::from_secs_f64(mean_us / 1e6));
                                telem.rtt_jitter =
                                    Some(Duration::from_secs_f64(var_us.sqrt() / 1e6));
                            }
                        }
                        commit_telemetry(shared, remote, &telem);
                    }
                    _ => {}
                },
            }
        };

        if !rtc.is_alive() {
            return Err("rtc no longer alive".to_string());
        }

        // Record a never-connected attempt as Failed once the deadline passes
        // (the STUN-only failure the metric counts). spawn_connection's exit
        // finalizer also covers ICE-disconnect/dead-remote exits.
        if !connected_reported && connect_start.elapsed() > CONNECT_DEADLINE {
            return Err("connect deadline exceeded".to_string());
        }

        // Service commands; each application mutates the Rtc, and the `continue`
        // re-enters the drain above before any further mutation.
        match cmd_rx.try_recv() {
            Ok(Cmd::Close) => return Ok(()),
            Ok(Cmd::Send { channel, data }) => {
                if let Some(id) = chan_ids.get(channel).copied().flatten()
                    && let Some(mut ch) = rtc.channel(id)
                {
                    match ch.write(true, &data) {
                        Ok(false) => {
                            log::warn!("[str0m] channel {channel} buffer full — dropped")
                        }
                        Ok(true) => {}
                        Err(err) => log::warn!("[str0m] channel {channel} write failed: {err}"),
                    }
                } else {
                    // Channel not open yet (handshake window) — the message is
                    // lost. Callers must gate sends on PeerState::Connected,
                    // exactly as with the matchbox Transport.
                    log::warn!(
                        "[str0m] send on channel {channel} to {remote} before open — dropped"
                    );
                }
                continue; // drain after mutation
            }
            Ok(Cmd::Signal(signal)) => {
                apply_signal(
                    &mut rtc,
                    signal,
                    &mut chan_ids,
                    &mut pending_offer,
                    &candidate_json,
                    out_tx,
                    remote,
                )?;
                continue; // drain after mutation
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return Ok(()),
        }

        // Wait for input: bounded by the Rtc's own deadline, capped so the
        // command queue stays responsive, floored to avoid a busy loop.
        let now = Instant::now();
        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(10))
            .max(Duration::from_millis(1));
        socket
            .set_read_timeout(Some(wait))
            .map_err(|e| format!("set timeout: {e}"))?;

        let input = match socket.recv_from(&mut buf) {
            Ok((n, source)) => match buf[..n].try_into() {
                Ok(contents) => Input::Receive(
                    Instant::now(),
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: local_addr,
                        contents,
                    },
                ),
                Err(err) => {
                    log::debug!("[str0m] unparseable datagram from {source}: {err}");
                    Input::Timeout(Instant::now())
                }
            },
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Input::Timeout(Instant::now())
            }
            Err(err) => return Err(format!("udp recv: {e}", e = err)),
        };
        rtc.handle_input(input)
            .map_err(|e| format!("handle_input: {e}"))?;
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_signal(
    rtc: &mut Rtc,
    signal: PeerSignal,
    chan_ids: &mut [Option<ChannelId>; 2],
    pending_offer: &mut Option<str0m::change::SdpPendingOffer>,
    candidate_json: &str,
    out_tx: &Sender<PeerRequest<PeerSignal>>,
    remote: MbPeerId,
) -> Result<(), String> {
    match signal {
        PeerSignal::Offer(sdp) => {
            let offer = str0m::change::SdpOffer::from_sdp_string(&sdp)
                .map_err(|e| format!("offer parse: {e}"))?;
            let answer = rtc
                .sdp_api()
                .accept_offer(offer)
                .map_err(|e| format!("accept_offer: {e}"))?;
            // Pre-negotiated channels (no DCEP): declare ours now, matching
            // matchbox's fixed stream ids.
            let [cfg0, cfg1] = channel_configs();
            let mut direct = rtc.direct_api();
            chan_ids[0] = Some(direct.create_data_channel(cfg0));
            chan_ids[1] = Some(direct.create_data_channel(cfg1));
            send_signal(out_tx, remote, PeerSignal::Answer(answer.to_sdp_string()));
            // Trickle our host candidate AFTER the answer — native matchbox
            // drops candidates that arrive before the answer it is waiting on.
            send_signal(
                out_tx,
                remote,
                PeerSignal::IceCandidate(candidate_json.to_string()),
            );
            Ok(())
        }
        PeerSignal::Answer(sdp) => {
            let answer = str0m::change::SdpAnswer::from_sdp_string(&sdp)
                .map_err(|e| format!("answer parse: {e}"))?;
            let pending = pending_offer
                .take()
                .ok_or_else(|| "answer without a pending offer".to_string())?;
            rtc.sdp_api()
                .accept_answer(pending, answer)
                .map_err(|e| format!("accept_answer: {e}"))?;
            Ok(())
        }
        PeerSignal::IceCandidate(json) => {
            if json == "null" {
                return Ok(()); // browser end-of-candidates sentinel
            }
            let init: IceCandidateJson =
                serde_json::from_str(&json).map_err(|e| format!("candidate json: {e}"))?;
            match Candidate::from_sdp_string(&init.candidate) {
                Ok(candidate) => {
                    rtc.add_remote_candidate(candidate);
                }
                Err(err) => log::warn!("[str0m] unparseable remote candidate ({err}): {json}"),
            }
            Ok(())
        }
    }
}

fn send_signal(out_tx: &Sender<PeerRequest<PeerSignal>>, remote: MbPeerId, data: PeerSignal) {
    let _ = out_tx.send(PeerRequest::Signal {
        receiver: remote,
        data,
    });
}

fn encode_candidate(candidate: &Candidate) -> Result<String, String> {
    let init = IceCandidateJson {
        candidate: candidate.to_sdp_string(),
        // Identify the m-line by INDEX only, never a hardcoded `sdpMid`. Our
        // offer/answer always has exactly one BUNDLE'd data m-line at index 0,
        // but str0m generates a RANDOM mid (e.g. `a=mid:SrN`) — a hardcoded
        // `sdpMid:"0"` mismatches it, and a strict `addIceCandidate` (Chrome)
        // REJECTS the candidate (`OperationError`), crashing browser matchbox.
        // webrtc-rs is lenient (ignores the wrong mid), which is why the
        // hermetic native tests never caught it; browsers are strict.
        sdp_mid: None,
        sdp_mline_index: Some(0),
        username_fragment: None,
    };
    serde_json::to_string(&init).map_err(|e| format!("candidate encode: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every str0m-derived config must match its [`ChannelSpec`] and the
    /// matchbox wire contract (label `matchbox_socket_{i}`, negotiated stream
    /// id == channel index) — the cross-stack semantics-parity proof.
    #[test]
    fn str0m_derivation_matches_specs_and_wire_contract() {
        let configs = channel_configs();
        assert_eq!(configs.len(), CHANNEL_SPECS.len());
        for (index, (config, spec)) in configs.iter().zip(CHANNEL_SPECS.iter()).enumerate() {
            assert_eq!(config.label, format!("matchbox_socket_{index}"));
            assert_eq!(
                config.negotiated,
                Some(index as u16),
                "negotiated stream id == channel index (no DCEP)"
            );
            assert_eq!(config.ordered, spec.ordered);
            match spec.max_retransmits {
                None => assert_eq!(config.reliability, Reliability::Reliable),
                Some(retransmits) => assert_eq!(
                    config.reliability,
                    Reliability::MaxRetransmits { retransmits }
                ),
            }
            assert!(config.protocol.is_empty(), "matchbox sets no protocol");
        }
    }

    /// `PeerId` is a newtype over `Uuid`; serde deserializes it from a UUID
    /// string, so we can mint distinct ids without a `uuid` dev-dependency.
    fn peer_id(n: u128) -> MbPeerId {
        let uuid = format!("{n:032x}");
        let hyphenated = format!(
            "{}-{}-{}-{}-{}",
            &uuid[0..8],
            &uuid[8..12],
            &uuid[12..16],
            &uuid[16..20],
            &uuid[20..32]
        );
        serde_json::from_str(&format!("\"{hyphenated}\"")).expect("valid uuid")
    }

    /// A connection thread that exits without ever reaching `Connected` marks
    /// the attempt `Failed` — the STUN-only failure the metric counts.
    #[test]
    fn finalize_marks_unconnected_as_failed() {
        let remote = peer_id(1);
        let mut map = HashMap::new();
        map.insert(remote, PeerTelemetry::connecting());
        finalize_failed_telemetry(&mut map, remote);
        assert_eq!(
            map.get(&remote).map(|t| t.outcome),
            Some(IceOutcome::Failed)
        );
    }

    /// A peer that connected then dropped STAYS `Connected` — it did connect;
    /// that is a disconnect, not a STUN failure.
    #[test]
    fn finalize_leaves_connected_untouched() {
        let remote = peer_id(2);
        let mut map = HashMap::new();
        let mut telem = PeerTelemetry::connecting();
        telem.outcome = IceOutcome::Connected;
        map.insert(remote, telem);
        finalize_failed_telemetry(&mut map, remote);
        assert_eq!(
            map.get(&remote).map(|t| t.outcome),
            Some(IceOutcome::Connected)
        );
    }

    /// A thread that exits before recording anything still counts as a `Failed`
    /// attempt (an entry is created).
    #[test]
    fn finalize_records_failed_when_absent() {
        let remote = peer_id(3);
        let mut map = HashMap::new();
        finalize_failed_telemetry(&mut map, remote);
        assert_eq!(
            map.get(&remote).map(|t| t.outcome),
            Some(IceOutcome::Failed)
        );
    }
}
