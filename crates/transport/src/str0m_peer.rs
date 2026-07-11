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
//! Resilience (ADR-0019): transient ICE `Disconnected` is tolerated (not fatal);
//! the offerer initiates an in-place `ice_restart` if it persists (channels
//! survive); the signaling WS reconnects with backoff without tearing down live
//! connections; a hard failure triggers a bounded full reconnect.
//! Limitations (slice scope): `ws://` signaling only (no TLS); loopback/local
//! UDP binding.

use std::collections::{HashMap, HashSet};
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
    Send {
        channel: usize,
        data: Box<[u8]>,
    },
    Signal(PeerSignal),
    /// Force an ICE restart on this connection (ADR-0019) — ops hook + the
    /// hermetic test trigger for the restart mechanism.
    IceRestart,
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq)]
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

/// After ICE goes `Disconnected` (a transient, self-recovering state), wait
/// this long for it to self-heal before the offerer initiates an ICE restart
/// (ADR-0019).
const ICE_RESTART_GRACE: Duration = Duration::from_secs(2);

/// If a `Disconnected` episode has not recovered by this point (even after an
/// ICE restart), give up the connection → the full-reconnect fallback.
const DISCONNECT_HARD_DEADLINE: Duration = Duration::from_secs(10);

/// Signaling WS reconnect backoff (ADR-0019): starts here, doubles, capped.
const SIGNALING_BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const SIGNALING_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Full-reconnect attempts before giving a peer up (ADR-0019). Bounds the
/// re-establish loop so a permanently-gone peer cannot spin forever.
const MAX_RECONNECT_ATTEMPTS: u32 = 5;

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
    /// Times this connection recovered from a transient `Disconnected` back to
    /// `Connected`/`Completed` (ADR-0019) — the network-resilience signal.
    pub reconnects: u32,
    /// Times this connection initiated an ICE restart (ADR-0019).
    pub ice_restarts: u32,
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
            reconnects: 0,
            ice_restarts: 0,
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

/// Fleet-aggregated connection metrics (ADR-0018) — what a session/fleet
/// computes from many [`PeerTelemetry`] records to fill the measurement gap:
/// the STUN-only success fraction, the winning-candidate-kind breakdown, and
/// the RTT/jitter distribution. Pure over the telemetry — no network needed to
/// compute or test; only the DATA needs real peers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FleetMetrics {
    /// Peers that reached `Connected`.
    pub connected: usize,
    /// Peers whose attempt `Failed`.
    pub failed: usize,
    /// Peers still `Connecting` (non-terminal; excluded from the success fraction).
    pub connecting: usize,
    /// `Connected / (Connected + Failed)` — the STUN-only success fraction.
    /// `None` until there is at least one terminal outcome.
    pub success_fraction: Option<f64>,
    /// Connected peers that won on a `Host` candidate.
    pub host: usize,
    /// Connected peers that won on a server-reflexive (STUN) candidate.
    pub server_reflexive: usize,
    /// Connected peers that won on a peer-reflexive candidate.
    pub peer_reflexive: usize,
    /// Connected peers that won on a relayed (TURN) candidate — the "needed TURN" count.
    pub relayed: usize,
    /// Connected peers whose winning candidate could not be classified.
    pub unknown_kind: usize,
    /// Smallest per-peer mean RTT among connected peers with a sample.
    pub rtt_min: Option<Duration>,
    /// Mean of per-peer mean RTTs.
    pub rtt_mean: Option<Duration>,
    /// Median (p50) of per-peer mean RTTs.
    pub rtt_p50: Option<Duration>,
    /// p95 of per-peer mean RTTs.
    pub rtt_p95: Option<Duration>,
    /// Largest per-peer mean RTT.
    pub rtt_max: Option<Duration>,
    /// Mean of per-peer RTT jitter over peers that reported jitter.
    pub jitter_mean: Option<Duration>,
    /// Total transient-disconnect recoveries across all peers (ADR-0019).
    pub total_reconnects: u32,
    /// Total ICE restarts initiated across all peers (ADR-0019).
    pub total_ice_restarts: u32,
}

impl FleetMetrics {
    /// Aggregate per-peer telemetry into fleet metrics (ADR-0018). A
    /// session/fleet collects [`Str0mPeer::telemetry`] across peers and feeds
    /// them here to produce the STUN-only success fraction + RTT/jitter
    /// distributions the measurement gap asks for.
    pub fn aggregate(peers: &[PeerTelemetry]) -> FleetMetrics {
        let mut m = FleetMetrics::default();
        // Each CONNECTED peer contributes its own mean RTT to the distribution.
        let mut rtt_means: Vec<Duration> = Vec::new();
        let mut jitter_sum_us = 0.0;
        let mut jitter_count: u64 = 0;

        for p in peers {
            m.total_reconnects += p.reconnects;
            m.total_ice_restarts += p.ice_restarts;
            match p.outcome {
                IceOutcome::Connected => {
                    m.connected += 1;
                    match p.local_candidate {
                        LocalCandidateKind::Host => m.host += 1,
                        LocalCandidateKind::ServerReflexive => m.server_reflexive += 1,
                        LocalCandidateKind::PeerReflexive => m.peer_reflexive += 1,
                        LocalCandidateKind::Relayed => m.relayed += 1,
                        LocalCandidateKind::Unknown => m.unknown_kind += 1,
                    }
                    if let Some(rtt) = p.rtt_mean {
                        rtt_means.push(rtt);
                    }
                    if let Some(jitter) = p.rtt_jitter {
                        jitter_sum_us += jitter.as_secs_f64() * 1e6;
                        jitter_count += 1;
                    }
                }
                IceOutcome::Failed => m.failed += 1,
                IceOutcome::Connecting => m.connecting += 1,
            }
        }

        let terminal = m.connected + m.failed;
        if terminal > 0 {
            m.success_fraction = Some(m.connected as f64 / terminal as f64);
        }

        if !rtt_means.is_empty() {
            rtt_means.sort_unstable();
            let n = rtt_means.len();
            let sum_us: f64 = rtt_means.iter().map(|d| d.as_secs_f64() * 1e6).sum();
            m.rtt_min = Some(rtt_means[0]);
            m.rtt_max = Some(rtt_means[n - 1]);
            m.rtt_mean = Some(Duration::from_secs_f64(sum_us / n as f64 / 1e6));
            m.rtt_p50 = Some(percentile(&rtt_means, 50));
            m.rtt_p95 = Some(percentile(&rtt_means, 95));
        }
        if jitter_count > 0 {
            m.jitter_mean = Some(Duration::from_secs_f64(
                jitter_sum_us / jitter_count as f64 / 1e6,
            ));
        }

        m
    }
}

/// Nearest-rank percentile of a NON-EMPTY sorted slice (`p` in `1..=100`).
fn percentile(sorted: &[Duration], p: usize) -> Duration {
    let n = sorted.len();
    // rank = ceil(p/100 · n), 1-indexed, clamped to [1, n].
    let rank = (p * n).div_ceil(100).clamp(1, n);
    sorted[rank - 1]
}

struct Shared {
    id: Mutex<Option<MbPeerId>>,
    conns: Mutex<HashMap<MbPeerId, Sender<Cmd>>>,
    /// Per-peer connection telemetry (ADR-0018); updated by the connection
    /// threads, read via [`Str0mPeer::telemetry`].
    telemetry: Mutex<HashMap<MbPeerId, PeerTelemetry>>,
    /// Peers currently in the room (ADR-0019): added on `NewPeer` / first
    /// inbound Offer, removed on `PeerLeft`. The full-reconnect fallback only
    /// re-establishes to a peer that is still present.
    present: Mutex<HashSet<MbPeerId>>,
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
            present: Mutex::new(HashSet::new()),
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

    /// Force an in-place ICE restart on the connection to `peer` (ADR-0019):
    /// re-gather + re-nominate a candidate pair WITHOUT tearing down DTLS/SCTP,
    /// so the channels (and buffered data) survive. Errors if the peer is
    /// unknown/closed. Ops recovery hook; also drives the mechanism's test.
    pub fn request_ice_restart(&mut self, peer: MbPeerId) -> Result<(), ChannelSendError> {
        let conns = self.shared.conns.lock().map_err(|_| ChannelSendError)?;
        let tx = conns.get(&peer).ok_or(ChannelSendError)?;
        tx.send(Cmd::IceRestart).map_err(|_| ChannelSendError)
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

/// Why a signaling session ended (ADR-0019).
enum SignalingExit {
    /// The `Str0mPeer` was explicitly closed — stop.
    Closed,
    /// The WS dropped (blip / server restart) — reconnect with backoff. Live
    /// WebRTC connections are NOT torn down.
    Dropped,
}

fn run_signaling(
    url: &str,
    shared: &Arc<Shared>,
    updates_tx: Sender<(MbPeerId, PeerState)>,
    state_tx: Sender<(MbPeerId, Packet)>,
    events_tx: Sender<(MbPeerId, Packet)>,
) {
    // The outbound queue lives ACROSS reconnects (ADR-0019): per-peer connection
    // threads hold `out_tx` clones and keep sending re-offers/candidates; a
    // signaling reconnect flushes them on the new WS.
    let (out_tx, out_rx) = std::sync::mpsc::channel::<PeerRequest<PeerSignal>>();
    let mut backoff = SIGNALING_BACKOFF_INITIAL;

    loop {
        if shared.closed.load(Ordering::SeqCst) {
            return;
        }
        match connect_and_serve(
            url,
            shared,
            &out_tx,
            &out_rx,
            &updates_tx,
            &state_tx,
            &events_tx,
            &mut backoff,
        ) {
            SignalingExit::Closed => return,
            SignalingExit::Dropped => {
                // A signaling blip must NOT kill live connections — no
                // `close_all` here. Back off and reconnect.
                if shared.closed.load(Ordering::SeqCst) {
                    return;
                }
                log::warn!("[str0m] signaling dropped — reconnecting in {backoff:?}");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(SIGNALING_BACKOFF_MAX);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn connect_and_serve(
    url: &str,
    shared: &Arc<Shared>,
    out_tx: &Sender<PeerRequest<PeerSignal>>,
    out_rx: &Receiver<PeerRequest<PeerSignal>>,
    updates_tx: &Sender<(MbPeerId, PeerState)>,
    state_tx: &Sender<(MbPeerId, Packet)>,
    events_tx: &Sender<(MbPeerId, Packet)>,
    backoff: &mut Duration,
) -> SignalingExit {
    let (mut ws, _resp) = match tungstenite::connect(url) {
        Ok(ok) => ok,
        Err(err) => {
            log::error!("[str0m] signaling connect failed: {err}");
            return SignalingExit::Dropped;
        }
    };
    // Connected — reset the backoff so a healthy session doesn't inherit growth.
    *backoff = SIGNALING_BACKOFF_INITIAL;
    // Read with a timeout so the loop can also drain outbound signals.
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = ws.get_ref()
        && stream.set_read_timeout(Some(WS_POLL)).is_err()
    {
        log::error!("[str0m] cannot set WS read timeout");
        return SignalingExit::Dropped;
    }

    let mut last_keepalive = Instant::now();

    loop {
        if shared.closed.load(Ordering::SeqCst) {
            return SignalingExit::Closed;
        }

        // Outbound: signals from connection threads + keepalive.
        while let Ok(request) = out_rx.try_recv() {
            match serde_json::to_string(&request) {
                Ok(text) => {
                    if let Err(err) = ws.send(tungstenite::Message::text(text)) {
                        log::warn!("[str0m] signaling send failed: {err}");
                        return SignalingExit::Dropped;
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
                        log::warn!("[str0m] keepalive failed: {err}");
                        return SignalingExit::Dropped;
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
                log::warn!("[str0m] signaling read failed: {err}");
                return SignalingExit::Dropped;
            }
        };
        let text = match message {
            tungstenite::Message::Text(text) => text,
            tungstenite::Message::Close(_) => {
                // Server closed (blip / restart) — reconnect, don't tear down.
                log::info!("[str0m] signaling closed by server — will reconnect");
                return SignalingExit::Dropped;
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
                mark_present(shared, remote, true);
                // We are the existing peer — we offer.
                spawn_connection(
                    Role::Offerer,
                    remote,
                    0,
                    shared,
                    out_tx,
                    updates_tx,
                    state_tx,
                    events_tx,
                );
            }
            PeerEvent::PeerLeft(remote) => {
                mark_present(shared, remote, false);
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
                            return SignalingExit::Closed;
                        }
                    };
                    conns.get(&sender).cloned()
                };
                let tx = match tx {
                    Some(tx) => tx,
                    None => {
                        // Unsolicited OFFER from an unknown peer: we are the
                        // newcomer (or the peer is re-offering after a reconnect,
                        // ADR-0019) and they initiate. Anything else from an
                        // unknown sender is a stray (e.g. a signal racing
                        // PeerLeft) — spawning an answerer for it would create
                        // a ghost connection that waits on an offer that never
                        // comes.
                        if !matches!(data, PeerSignal::Offer(_)) {
                            log::debug!("[str0m] stray non-offer signal from unknown {sender}");
                            continue;
                        }
                        mark_present(shared, sender, true);
                        spawn_connection(
                            Role::Answerer,
                            sender,
                            0,
                            shared,
                            out_tx,
                            updates_tx,
                            state_tx,
                            events_tx,
                        )
                    }
                };
                let _ = tx.send(Cmd::Signal(data));
            }
        }
    }
}

/// Add/remove a peer from the room-presence set (ADR-0019).
fn mark_present(shared: &Shared, remote: MbPeerId, present: bool) {
    if let Ok(mut set) = shared.present.lock() {
        if present {
            set.insert(remote);
        } else {
            set.remove(&remote);
        }
    }
}

/// Backoff before the Nth full-reconnect attempt (ADR-0019): exponential,
/// capped.
fn reconnect_backoff(attempt: u32) -> Duration {
    (SIGNALING_BACKOFF_INITIAL * 2u32.pow(attempt.min(4))).min(SIGNALING_BACKOFF_MAX)
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
    attempt: u32,
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
        let result = run_connection(
            role,
            remote,
            cmd_rx,
            &shared,
            &out_tx,
            &updates_tx,
            &state_tx,
            &events_tx,
        );
        let failed = result.is_err();
        if let Err(err) = result {
            log::warn!("[str0m] connection to {remote} ended: {err}");
        }
        // The connection dropped: remove the routing entry and tell the consumer
        // (so it pauses sending). A reconnect, if any, will re-report Connected.
        if let Ok(mut conns) = shared.conns.lock() {
            conns.remove(&remote);
        }
        let _ = updates_tx.send((remote, PeerState::Disconnected));

        // Full-reconnect fallback (ADR-0019): only the OFFERER re-establishes
        // (glare avoidance — the answerer recovers via the offerer's re-offer,
        // through the unknown-sender-Offer path), only on a hard failure, only
        // while the peer is still in the room, bounded by MAX_RECONNECT_ATTEMPTS.
        if failed
            && role == Role::Offerer
            && attempt < MAX_RECONNECT_ATTEMPTS
            && !shared.closed.load(Ordering::SeqCst)
        {
            std::thread::sleep(reconnect_backoff(attempt));
            let still_present = shared
                .present
                .lock()
                .map(|p| p.contains(&remote))
                .unwrap_or(false);
            if still_present && !shared.closed.load(Ordering::SeqCst) {
                log::info!("[str0m] reconnecting to {remote} (attempt {})", attempt + 1);
                spawn_connection(
                    Role::Offerer,
                    remote,
                    attempt + 1,
                    &shared,
                    &out_tx,
                    &updates_tx,
                    &state_tx,
                    &events_tx,
                );
                return; // retrying — do NOT finalize as Failed
            }
        }

        // Gave up (or a clean close): an attempt that never reached `Connected`
        // is a `Failed` connection (the STUN-only failure the metric counts).
        if let Ok(mut map) = shared.telemetry.lock() {
            finalize_failed_telemetry(&mut map, remote);
        }
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

    // Reconnect state (ADR-0019): `Disconnected` is a TRANSIENT, self-recovering
    // ICE state — we tolerate it (don't tear down) and, as the offerer, ICE-restart
    // in place if it persists. `disconnected_since` is the current episode start;
    // `restart_sent` gates one restart per episode.
    let is_offerer = matches!(role, Role::Offerer);
    let mut disconnected_since: Option<Instant> = None;
    let mut restart_sent = false;

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
                    // ICE state (ADR-0019): `Disconnected` is TRANSIENT — start a
                    // recovery episode instead of tearing down; returning to
                    // `Connected`/`Completed` clears it and counts a recovery.
                    Event::IceConnectionStateChange(state) => match state {
                        IceConnectionState::Disconnected => {
                            if disconnected_since.is_none() {
                                disconnected_since = Some(Instant::now());
                                // New episode: allow a fresh auto-restart even if
                                // an explicit `request_ice_restart` already set
                                // `restart_sent` (that restart need not have
                                // produced a `Disconnected`, so the recovery
                                // reset may never have fired).
                                restart_sent = false;
                                log::info!(
                                    "[str0m] ice disconnected from {remote} — awaiting recovery"
                                );
                            }
                        }
                        IceConnectionState::Connected | IceConnectionState::Completed => {
                            let was_disconnected = disconnected_since.take().is_some();
                            if was_disconnected {
                                restart_sent = false;
                                telem.reconnects += 1;
                                commit_telemetry(shared, remote, &telem);
                                log::info!("[str0m] ice recovered to {remote}");
                            }
                        }
                        _ => {}
                    },
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

        // Reconnect handling (ADR-0019): during a `Disconnected` episode, the
        // offerer ICE-restarts once after the self-heal grace; a hard deadline
        // gives up → the full-reconnect fallback in spawn_connection.
        if let Some(since) = disconnected_since {
            let down = since.elapsed();
            if should_initiate_restart(is_offerer, Some(down), restart_sent) {
                restart_sent = true;
                if initiate_ice_restart(
                    &mut rtc,
                    &mut pending_offer,
                    &candidate_json,
                    out_tx,
                    remote,
                ) {
                    telem.ice_restarts += 1;
                    commit_telemetry(shared, remote, &telem);
                    log::info!("[str0m] initiated ICE restart to {remote}");
                }
                continue; // SDP change is a mutation → re-enter the drain
            }
            if down > DISCONNECT_HARD_DEADLINE {
                return Err("ice restart did not recover".to_string());
            }
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
            Ok(Cmd::IceRestart) => {
                // Explicit ICE restart (ADR-0019): ops hook + the mechanism's
                // test trigger. Same in-place restart as the automatic path.
                // Only once connected — restarting mid-handshake would clobber
                // the initial `pending_offer` and break the first negotiation.
                if connected_reported
                    && initiate_ice_restart(
                        &mut rtc,
                        &mut pending_offer,
                        &candidate_json,
                        out_tx,
                        remote,
                    )
                {
                    telem.ice_restarts += 1;
                    restart_sent = true;
                    commit_telemetry(shared, remote, &telem);
                }
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

/// Whether the offerer should initiate an ICE restart now (ADR-0019): only the
/// offerer initiates (glare avoidance — the answerer recovers via the re-offer),
/// only once per disconnect episode, and only after the self-heal grace.
fn should_initiate_restart(
    is_offerer: bool,
    disconnected_for: Option<Duration>,
    restart_sent: bool,
) -> bool {
    is_offerer && !restart_sent && disconnected_for.is_some_and(|d| d > ICE_RESTART_GRACE)
}

/// Initiate an in-place ICE restart (ADR-0019): schedule the restart (keep
/// local candidates), produce a new offer, send it + re-trickle the candidate.
/// The DTLS/SCTP association and its channels are NOT torn down. `apply()` is
/// an `Rtc` mutation — the caller must re-enter the drain (`continue`) after.
/// Returns whether an offer was produced.
fn initiate_ice_restart(
    rtc: &mut Rtc,
    pending_offer: &mut Option<str0m::change::SdpPendingOffer>,
    candidate_json: &str,
    out_tx: &Sender<PeerRequest<PeerSignal>>,
    remote: MbPeerId,
) -> bool {
    let mut sdp = rtc.sdp_api();
    sdp.ice_restart(true);
    if let Some((offer, pending)) = sdp.apply() {
        *pending_offer = Some(pending);
        send_signal(out_tx, remote, PeerSignal::Offer(offer.to_sdp_string()));
        send_signal(
            out_tx,
            remote,
            PeerSignal::IceCandidate(candidate_json.to_string()),
        );
        true
    } else {
        false
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
            // Pre-negotiated channels (no DCEP): declare ours on the FIRST offer
            // only. A re-offer (ICE restart, ADR-0019) reuses the existing
            // channels — `accept_offer` handles the ICE-restart creds change;
            // recreating the channels would break them.
            if chan_ids[0].is_none() {
                let [cfg0, cfg1] = channel_configs();
                let mut direct = rtc.direct_api();
                chan_ids[0] = Some(direct.create_data_channel(cfg0));
                chan_ids[1] = Some(direct.create_data_channel(cfg1));
            }
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

    /// Build a telemetry record with a given outcome, candidate kind, and RTT
    /// (ms) — for the fleet-aggregation tests.
    fn telem(outcome: IceOutcome, kind: LocalCandidateKind, rtt_ms: Option<f64>) -> PeerTelemetry {
        let mut t = PeerTelemetry::connecting();
        t.outcome = outcome;
        t.local_candidate = kind;
        t.rtt_mean = rtt_ms.map(|ms| Duration::from_secs_f64(ms / 1e3));
        t.rtt_jitter = rtt_ms.map(|_| Duration::from_millis(1));
        t
    }

    /// Empty input yields all-zero metrics with no fraction/distribution.
    #[test]
    fn aggregate_empty() {
        let m = FleetMetrics::aggregate(&[]);
        assert_eq!(m, FleetMetrics::default());
        assert_eq!(m.success_fraction, None);
        assert_eq!(m.rtt_mean, None);
    }

    /// The STUN-only success fraction is Connected / (Connected + Failed);
    /// `Connecting` peers are excluded from the fraction.
    #[test]
    fn aggregate_success_fraction_excludes_connecting() {
        let peers = [
            telem(IceOutcome::Connected, LocalCandidateKind::Host, Some(10.0)),
            telem(IceOutcome::Connected, LocalCandidateKind::Host, Some(20.0)),
            telem(IceOutcome::Connected, LocalCandidateKind::Host, Some(30.0)),
            telem(IceOutcome::Failed, LocalCandidateKind::Unknown, None),
            telem(IceOutcome::Connecting, LocalCandidateKind::Unknown, None),
        ];
        let m = FleetMetrics::aggregate(&peers);
        assert_eq!(m.connected, 3);
        assert_eq!(m.failed, 1);
        assert_eq!(m.connecting, 1);
        // 3 / (3 + 1) = 0.75 — the connecting peer is not counted.
        assert_eq!(m.success_fraction, Some(0.75));
    }

    /// The candidate-kind breakdown counts only CONNECTED peers, per kind.
    #[test]
    fn aggregate_candidate_kind_breakdown() {
        let peers = [
            telem(IceOutcome::Connected, LocalCandidateKind::Host, Some(5.0)),
            telem(
                IceOutcome::Connected,
                LocalCandidateKind::ServerReflexive,
                Some(5.0),
            ),
            telem(
                IceOutcome::Connected,
                LocalCandidateKind::Relayed,
                Some(5.0),
            ),
            // A failed peer's kind is NOT counted in the breakdown.
            telem(IceOutcome::Failed, LocalCandidateKind::Host, None),
        ];
        let m = FleetMetrics::aggregate(&peers);
        assert_eq!(m.host, 1);
        assert_eq!(m.server_reflexive, 1);
        assert_eq!(m.relayed, 1);
        assert_eq!(m.peer_reflexive, 0);
        assert_eq!(m.unknown_kind, 0);
    }

    /// The RTT distribution (min/mean/p50/p95/max) is over the per-peer means
    /// of connected peers; nearest-rank percentiles.
    #[test]
    fn aggregate_rtt_distribution() {
        // Per-peer mean RTTs 1..=10 ms.
        let peers: Vec<_> = (1..=10)
            .map(|ms| {
                telem(
                    IceOutcome::Connected,
                    LocalCandidateKind::Host,
                    Some(ms as f64),
                )
            })
            .collect();
        let m = FleetMetrics::aggregate(&peers);
        let ms = |d: Option<Duration>| d.map(|d| (d.as_secs_f64() * 1e3).round() as i64);
        assert_eq!(ms(m.rtt_min), Some(1));
        assert_eq!(ms(m.rtt_max), Some(10));
        assert_eq!(ms(m.rtt_mean), Some(6)); // (1..=10).mean() = 5.5 → rounds to 6
        // nearest-rank: p50 = ceil(0.5·10)=5th = 5 ms; p95 = ceil(0.95·10)=10th = 10 ms.
        assert_eq!(ms(m.rtt_p50), Some(5));
        assert_eq!(ms(m.rtt_p95), Some(10));
        assert!(m.jitter_mean.is_some());
    }

    /// All-failed: success fraction is 0.0, no RTT distribution.
    #[test]
    fn aggregate_all_failed() {
        let peers = [
            telem(IceOutcome::Failed, LocalCandidateKind::Unknown, None),
            telem(IceOutcome::Failed, LocalCandidateKind::Unknown, None),
        ];
        let m = FleetMetrics::aggregate(&peers);
        assert_eq!(m.success_fraction, Some(0.0));
        assert_eq!(m.rtt_mean, None);
    }

    /// The offerer initiates an ICE restart only after the grace, only once
    /// per episode, and the answerer never initiates (glare avoidance).
    #[test]
    fn restart_decision() {
        let past_grace = Some(ICE_RESTART_GRACE + Duration::from_millis(1));
        let within_grace = Some(Duration::from_millis(1));
        // Offerer, past grace, not yet sent → restart.
        assert!(should_initiate_restart(true, past_grace, false));
        // Still within the self-heal grace → wait.
        assert!(!should_initiate_restart(true, within_grace, false));
        // Already restarted this episode → don't repeat.
        assert!(!should_initiate_restart(true, past_grace, true));
        // Answerer never initiates.
        assert!(!should_initiate_restart(false, past_grace, false));
        // Not disconnected → nothing to do.
        assert!(!should_initiate_restart(true, None, false));
    }

    /// Reconnect backoff is exponential and capped at SIGNALING_BACKOFF_MAX.
    #[test]
    fn reconnect_backoff_is_bounded() {
        assert_eq!(reconnect_backoff(0), SIGNALING_BACKOFF_INITIAL);
        assert_eq!(reconnect_backoff(1), SIGNALING_BACKOFF_INITIAL * 2);
        assert!(reconnect_backoff(3) >= reconnect_backoff(1));
        // Far-out attempts saturate at the cap, never grow unbounded.
        assert_eq!(reconnect_backoff(100), SIGNALING_BACKOFF_MAX);
    }

    /// Nearest-rank percentile edge cases: single element, and p100 → max.
    #[test]
    fn percentile_edges() {
        let one = [Duration::from_millis(7)];
        assert_eq!(percentile(&one, 50), Duration::from_millis(7));
        assert_eq!(percentile(&one, 95), Duration::from_millis(7));
        let ten: Vec<_> = (1..=10).map(Duration::from_millis).collect();
        assert_eq!(percentile(&ten, 100), Duration::from_millis(10));
        assert_eq!(percentile(&ten, 10), Duration::from_millis(1));
    }
}
