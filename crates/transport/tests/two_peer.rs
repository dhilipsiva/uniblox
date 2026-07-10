//! Two native peers connect P2P through an in-process signaling server and
//! exchange data on BOTH channels (0 = unreliable state, 1 = reliable events).
//!
//! Written FIRST (TDD, locked by the tests/-guard hook). This is the automated
//! core proof for the transport item's acceptance: real WebRTC datachannels +
//! real room-based signaling, hermetic (empty ICE config → loopback host
//! candidates only, no STUN/no network), bounded by a hard timeout so a broken
//! handshake fails rather than hangs.
//!
//! The state channel is genuinely unreliable (`max_retransmits: 0`), so state
//! markers are RE-SENT each poll until received — a single lost datagram must
//! not flake the test. Events (reliable channel) are sent exactly once.

use std::net::Ipv4Addr;
use std::time::Duration;

use matchbox_signaling::SignalingServer;
use transport::{CHANNEL_EVENTS, CHANNEL_STATE, PeerState, Transport};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Spin an in-process full-mesh signaling server on an ephemeral port and
/// return its ws room URL.
fn start_signaling() -> String {
    let mut server = SignalingServer::full_mesh_builder((Ipv4Addr::LOCALHOST, 0)).build();
    let addr = server.bind().expect("signaling server must bind");
    tokio::spawn(server.serve());
    format!("ws://{addr}/two_peer_test")
}

/// Poll a transport until it reports exactly `n` connected peers (accumulating
/// Connected/Disconnected updates), or panic on timeout.
async fn wait_for_peers(t: &mut Transport, n: usize) {
    let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    let mut connected = std::collections::HashSet::new();
    loop {
        for (peer, state) in t.poll_peers().expect("transport must stay open") {
            match state {
                PeerState::Connected => {
                    connected.insert(peer);
                }
                PeerState::Disconnected => {
                    connected.remove(&peer);
                }
            }
        }
        if connected.len() == n {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {n} connected peer(s); have {}",
            connected.len()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Receive one packet on the receiver's given channel, or panic on timeout.
/// `resend` (sender-side) runs every poll iteration — pass the state-channel
/// send for unreliable delivery (idempotent; first arrival wins), or a no-op
/// for the reliable events channel.
async fn recv_one(
    rx: &mut Transport,
    channel: usize,
    mut resend: impl FnMut(&mut Transport),
    tx: &mut Transport,
) -> (transport::PeerId, transport::Packet) {
    let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        // Keep both peers' state machinery pumped while waiting.
        let _ = rx.poll_peers().expect("receiver must stay open");
        let _ = tx.poll_peers().expect("sender must stay open");
        resend(tx);
        let mut got = match channel {
            CHANNEL_STATE => rx.recv_state(),
            CHANNEL_EVENTS => rx.recv_events(),
            other => panic!("no such channel: {other}"),
        };
        if !got.is_empty() {
            return got.remove(0);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a packet on channel {channel}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The acceptance core: two peers, one room, data flows BOTH directions on
/// BOTH channels, and each payload arrives on the channel it was sent on.
#[tokio::test]
async fn two_peers_exchange_on_both_channels() {
    let room_url = start_signaling();

    let (mut a, a_loop) = Transport::connect_hermetic(&room_url);
    let (mut b, b_loop) = Transport::connect_hermetic(&room_url);
    tokio::spawn(a_loop);
    tokio::spawn(b_loop);

    // Both peers must see each other via the signaling handshake + ICE (loopback).
    wait_for_peers(&mut a, 1).await;
    wait_for_peers(&mut b, 1).await;

    let b_id = a.connected_peers().next().expect("a must see b");
    let a_id = b.connected_peers().next().expect("b must see a");

    // Distinct markers per channel so cross-channel delivery would be caught.
    // Events (reliable): send exactly once. State (unreliable): re-sent in the
    // recv loop below.
    a.send_event(b_id, (*b"a->b event").into())
        .expect("send_event a->b");
    b.send_event(a_id, (*b"b->a event").into())
        .expect("send_event b->a");

    let (from, payload) = recv_one(
        &mut b,
        CHANNEL_STATE,
        |tx| {
            let _ = tx.send_state(b_id, (*b"a->b state").into());
        },
        &mut a,
    )
    .await;
    assert_eq!(from, a_id);
    assert_eq!(&payload[..], b"a->b state");

    let (from, payload) = recv_one(&mut b, CHANNEL_EVENTS, |_| {}, &mut a).await;
    assert_eq!(from, a_id);
    assert_eq!(&payload[..], b"a->b event");

    let (from, payload) = recv_one(
        &mut a,
        CHANNEL_STATE,
        |tx| {
            let _ = tx.send_state(a_id, (*b"b->a state").into());
        },
        &mut b,
    )
    .await;
    assert_eq!(from, b_id);
    assert_eq!(&payload[..], b"b->a state");

    let (from, payload) = recv_one(&mut a, CHANNEL_EVENTS, |_| {}, &mut b).await;
    assert_eq!(from, b_id);
    assert_eq!(&payload[..], b"b->a event");

    // Own ids are assigned by signaling and distinct.
    let (id_a, id_b) = (a.id(), b.id());
    assert!(id_a.is_some() && id_b.is_some());
    assert_ne!(id_a, id_b);
}
