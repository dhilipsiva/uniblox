//! str0m ↔ native matchbox (webrtc-rs) interop — the flagged
//! highest-uncertainty pairing, validated hermetically (ADR-0015). Locked
//! FIRST (TDD).
//!
//! Two tests cover BOTH role directions (matchbox's full-mesh rule: existing
//! peers offer to newcomers):
//! - matchbox joins first, str0m second → matchbox OFFERS, str0m ANSWERS
//!   (str0m = DTLS client).
//! - str0m joins first, matchbox second → str0m OFFERS, matchbox ANSWERS
//!   (str0m = DTLS server).
//!
//! Each exchanges distinct markers on BOTH channels in BOTH directions and
//! asserts payloads arrive on the channel they were sent on (state markers are
//! re-sent per poll — channel 0 is genuinely max_retransmits=0; events are
//! sent once — channel 1 is reliable).

use std::net::Ipv4Addr;
use std::time::Duration;

use matchbox_signaling::SignalingServer;
use transport::{CHANNEL_EVENTS, CHANNEL_STATE, Packet, PeerId, PeerState, Str0mPeer, Transport};

const DEADLINE: Duration = Duration::from_secs(120); // bounds hangs, not CPU contention
const POLL: Duration = Duration::from_millis(20);

fn start_signaling() -> String {
    let mut server = SignalingServer::full_mesh_builder((Ipv4Addr::LOCALHOST, 0)).build();
    let addr = server.bind().expect("signaling server must bind");
    tokio::spawn(server.serve());
    format!("ws://{addr}/str0m_interop")
}

/// Both peer types expose the same surface; the tests drive them uniformly.
trait PeerLike {
    fn poll(&mut self) -> Vec<(PeerId, PeerState)>;
    fn send_on(&mut self, channel: usize, to: PeerId, data: Box<[u8]>);
    fn recv_on(&mut self, channel: usize) -> Vec<(PeerId, Packet)>;
}

impl PeerLike for Transport {
    fn poll(&mut self) -> Vec<(PeerId, PeerState)> {
        self.poll_peers().expect("matchbox transport open")
    }
    fn send_on(&mut self, channel: usize, to: PeerId, data: Box<[u8]>) {
        let result = match channel {
            CHANNEL_STATE => self.send_state(to, data),
            CHANNEL_EVENTS => self.send_event(to, data),
            other => panic!("no such channel: {other}"),
        };
        result.expect("matchbox send");
    }
    fn recv_on(&mut self, channel: usize) -> Vec<(PeerId, Packet)> {
        match channel {
            CHANNEL_STATE => self.recv_state(),
            CHANNEL_EVENTS => self.recv_events(),
            other => panic!("no such channel: {other}"),
        }
    }
}

impl PeerLike for Str0mPeer {
    fn poll(&mut self) -> Vec<(PeerId, PeerState)> {
        self.poll_peers().expect("str0m peer open")
    }
    fn send_on(&mut self, channel: usize, to: PeerId, data: Box<[u8]>) {
        let result = match channel {
            CHANNEL_STATE => self.send_state(to, data),
            CHANNEL_EVENTS => self.send_event(to, data),
            other => panic!("no such channel: {other}"),
        };
        result.expect("str0m send");
    }
    fn recv_on(&mut self, channel: usize) -> Vec<(PeerId, Packet)> {
        match channel {
            CHANNEL_STATE => self.recv_state(),
            CHANNEL_EVENTS => self.recv_events(),
            other => panic!("no such channel: {other}"),
        }
    }
}

/// Poll until exactly one peer is Connected; return it.
async fn wait_connected(p: &mut impl PeerLike) -> PeerId {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut connected = std::collections::HashSet::new();
    loop {
        for (peer, state) in p.poll() {
            match state {
                PeerState::Connected => {
                    connected.insert(peer);
                }
                PeerState::Disconnected => {
                    connected.remove(&peer);
                }
            }
        }
        if let Some(&peer) = connected.iter().next() {
            return peer;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a Connected peer"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Full bidirectional exchange between two connected peers.
async fn exchange_both_channels(
    a: &mut impl PeerLike,
    a_peer: PeerId, // b's id as seen by a
    b: &mut impl PeerLike,
    b_peer: PeerId, // a's id as seen by b
) {
    // Events (reliable): send exactly once.
    a.send_on(CHANNEL_EVENTS, a_peer, (*b"a->b event").into());
    b.send_on(CHANNEL_EVENTS, b_peer, (*b"b->a event").into());

    // a -> b state (unreliable; re-sent until received).
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let (from, payload) = loop {
        a.send_on(CHANNEL_STATE, a_peer, (*b"a->b state").into());
        let _ = a.poll();
        let _ = b.poll();
        let mut got = b.recv_on(CHANNEL_STATE);
        if !got.is_empty() {
            break got.remove(0);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a->b state never arrived"
        );
        tokio::time::sleep(POLL).await;
    };
    assert_eq!(from, b_peer);
    assert_eq!(&payload[..], b"a->b state");

    // b -> a state.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let (from, payload) = loop {
        b.send_on(CHANNEL_STATE, b_peer, (*b"b->a state").into());
        let _ = a.poll();
        let _ = b.poll();
        let mut got = a.recv_on(CHANNEL_STATE);
        if !got.is_empty() {
            break got.remove(0);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "b->a state never arrived"
        );
        tokio::time::sleep(POLL).await;
    };
    assert_eq!(from, a_peer);
    assert_eq!(&payload[..], b"b->a state");

    // Events arrive on the events channel (reliable — no resend needed).
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let (from, payload) = loop {
        let _ = a.poll();
        let _ = b.poll();
        let mut got = b.recv_on(CHANNEL_EVENTS);
        if !got.is_empty() {
            break got.remove(0);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a->b event never arrived"
        );
        tokio::time::sleep(POLL).await;
    };
    assert_eq!(from, b_peer);
    assert_eq!(&payload[..], b"a->b event");

    let deadline = tokio::time::Instant::now() + DEADLINE;
    let (from, payload) = loop {
        let _ = a.poll();
        let _ = b.poll();
        let mut got = a.recv_on(CHANNEL_EVENTS);
        if !got.is_empty() {
            break got.remove(0);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "b->a event never arrived"
        );
        tokio::time::sleep(POLL).await;
    };
    assert_eq!(from, a_peer);
    assert_eq!(&payload[..], b"b->a event");
}

/// matchbox joins FIRST → matchbox receives NewPeer(str0m) → matchbox OFFERS,
/// str0m ANSWERS (str0m = DTLS client).
#[tokio::test(flavor = "multi_thread")]
async fn str0m_answers_matchbox_offer() {
    let room = start_signaling();

    let (mut mb, mb_loop) = Transport::connect_hermetic(&room);
    tokio::spawn(mb_loop);
    // Ensure matchbox is registered in the room before str0m joins.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while mb.id().is_none() {
        assert!(tokio::time::Instant::now() < deadline, "no matchbox id");
        tokio::time::sleep(POLL).await;
    }

    let mut st = Str0mPeer::connect(&room);

    let st_seen_by_mb = wait_connected(&mut mb).await;
    let mb_seen_by_st = wait_connected(&mut st).await;
    assert_eq!(Some(mb_seen_by_st), mb.id(), "str0m must see matchbox's id");
    assert_eq!(Some(st_seen_by_mb), st.id(), "matchbox must see str0m's id");

    exchange_both_channels(&mut mb, st_seen_by_mb, &mut st, mb_seen_by_st).await;
}

/// str0m ↔ str0m: both ends are ours — proves our candidate ENCODE round-trips
/// through our own DECODE (the matchbox pairings only prove one side each),
/// plus offer/answer between two str0m instances (server↔server pairing).
#[tokio::test(flavor = "multi_thread")]
async fn str0m_to_str0m() {
    let room = start_signaling();

    let mut first = Str0mPeer::connect(&room);
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while first.id().is_none() {
        assert!(tokio::time::Instant::now() < deadline, "no first str0m id");
        tokio::time::sleep(POLL).await;
    }

    let mut second = Str0mPeer::connect(&room);

    let second_seen_by_first = wait_connected(&mut first).await;
    let first_seen_by_second = wait_connected(&mut second).await;
    assert_eq!(Some(first_seen_by_second), first.id());
    assert_eq!(Some(second_seen_by_first), second.id());

    exchange_both_channels(
        &mut first,
        second_seen_by_first,
        &mut second,
        first_seen_by_second,
    )
    .await;
}

/// str0m joins FIRST → str0m receives NewPeer(matchbox) → str0m OFFERS,
/// matchbox ANSWERS (str0m = DTLS server).
#[tokio::test(flavor = "multi_thread")]
async fn str0m_offers_matchbox_answers() {
    let room = start_signaling();

    let mut st = Str0mPeer::connect(&room);
    // Ensure str0m is registered in the room before matchbox joins.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while st.id().is_none() {
        assert!(tokio::time::Instant::now() < deadline, "no str0m id");
        tokio::time::sleep(POLL).await;
    }

    let (mut mb, mb_loop) = Transport::connect_hermetic(&room);
    tokio::spawn(mb_loop);

    let mb_seen_by_st = wait_connected(&mut st).await;
    let st_seen_by_mb = wait_connected(&mut mb).await;
    assert_eq!(Some(mb_seen_by_st), mb.id(), "str0m must see matchbox's id");
    assert_eq!(Some(st_seen_by_mb), st.id(), "matchbox must see str0m's id");

    exchange_both_channels(&mut st, mb_seen_by_st, &mut mb, st_seen_by_mb).await;
}
