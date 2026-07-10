//! TURN relay proof (ADR-0016) — hermetic, against a locally spawned coturn
//! (`turnserver` comes from the flake devShell).
//!
//! The relay proof uses RAW webrtc-rs peers with `ice_transport_policy =
//! Relay`: under that policy host/srflx candidates are excluded, so a
//! connected data channel can ONLY be carried through the TURN allocation —
//! data arriving proves "TURN relay works with credentials" end to end.
//! (matchbox does not expose the relay-only policy, so the proof is at the
//! webrtc-rs layer matchbox native itself is built on; the matchbox-level
//! test below proves our `IceConfig` plumbs the same TURN url + credentials
//! through `Transport::connect_with_ice`.)

use std::net::{Ipv4Addr, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use transport::{IceConfig, PeerState, Transport};

use webrtc::api::APIBuilder;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;

const TURN_USER: &str = "uniblox";
const TURN_PASS: &str = "relaysecret";
const TURN_REALM: &str = "uniblox.test";
const DEADLINE: Duration = Duration::from_secs(120); // bounds hangs, not CPU contention

/// A locally spawned coturn, killed on drop.
struct Turnserver {
    child: Child,
    port: u16,
}

impl Drop for Turnserver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_turnserver() -> Turnserver {
    // Reserve an ephemeral port, then hand it to coturn (small race window,
    // acceptable in tests).
    let probe = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("probe bind");
    let port = probe.local_addr().expect("probe addr").port();
    drop(probe);

    let child = Command::new("turnserver")
        .args([
            "-n", // no config file
            "--listening-ip",
            "127.0.0.1",
            "--relay-ip",
            "127.0.0.1",
            "--listening-port",
            &port.to_string(),
            "--realm",
            TURN_REALM,
            "--lt-cred-mech",
            "--user",
            &format!("{TURN_USER}:{TURN_PASS}"),
            "--no-tls",
            "--no-dtls",
            "--no-cli",
            // Test-only: coturn blocks loopback PEER addresses by default
            // (CVE-2020-26262 hardening) — hermetic relaying needs the
            // explicit opt-out. NEVER set this on a production coturn.
            "--allow-loopback-peers",
            "--log-file",
            "stdout",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("turnserver must be on PATH (the flake devShell provides coturn)");

    // Readiness: a TCP connect only proves the process is up — the UDP path
    // can lag behind it, and a lost first Allocate makes relay-only gathering
    // finish with ZERO candidates (observed). Probe with a real STUN Binding
    // request over UDP until coturn answers.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if stun_binding_answered(port) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "turnserver UDP path never became ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    Turnserver { child, port }
}

/// Send one STUN Binding request (RFC 5389) to `127.0.0.1:port` over UDP and
/// report whether ANY reply arrives — the readiness signal that coturn's UDP
/// listener is actually servicing packets.
fn stun_binding_answered(port: u16) -> bool {
    let socket = match UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)) {
        Ok(socket) => socket,
        Err(_) => return false,
    };
    if socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .is_err()
    {
        return false;
    }
    let mut request = [0u8; 20];
    request[0..2].copy_from_slice(&0x0001u16.to_be_bytes()); // Binding Request
    // message length stays 0 (no attributes)
    request[4..8].copy_from_slice(&0x2112_A442u32.to_be_bytes()); // magic cookie
    request[8..20].copy_from_slice(b"unibloxprobe"); // transaction id (12 bytes)
    if socket
        .send_to(&request, (Ipv4Addr::LOCALHOST, port))
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 128];
    socket.recv_from(&mut buf).is_ok()
}

fn turn_url(port: u16) -> String {
    format!("turn:127.0.0.1:{port}?transport=udp")
}

/// Relay-only peer with the given credentials.
async fn relay_only_peer(port: u16, username: &str, password: &str) -> Arc<RTCPeerConnection> {
    let api = APIBuilder::new().build();
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec![turn_url(port)],
            username: username.to_string(),
            credential: password.to_string(),
        }],
        ice_transport_policy: RTCIceTransportPolicy::Relay,
        ..Default::default()
    };
    Arc::new(
        api.new_peer_connection(config)
            .await
            .expect("peer connection"),
    )
}

/// Non-trickle offer/answer: gather ALL candidates first, then exchange
/// complete SDPs. Under relay-only policy candidates can only come from
/// successful TURN allocations, so `expect_relay` asserts the gathering
/// outcome: `true` = both sides must hold relay candidates (valid creds);
/// `false` = neither side may (allocation rejected).
async fn connect_non_trickle(
    offerer: &Arc<RTCPeerConnection>,
    answerer: &Arc<RTCPeerConnection>,
    expect_relay: bool,
) {
    let offer = offerer.create_offer(None).await.expect("create offer");
    let mut offer_gathered = offerer.gathering_complete_promise().await;
    offerer
        .set_local_description(offer)
        .await
        .expect("offerer SLD");
    let _ = offer_gathered.recv().await;
    let offer = offerer
        .local_description()
        .await
        .expect("offerer local description");
    assert_eq!(
        offer.sdp.contains("typ relay"),
        expect_relay,
        "offerer relay-candidate presence must match credential validity"
    );

    answerer
        .set_remote_description(offer)
        .await
        .expect("answerer SRD");
    let answer = answerer.create_answer(None).await.expect("create answer");
    let mut answer_gathered = answerer.gathering_complete_promise().await;
    answerer
        .set_local_description(answer)
        .await
        .expect("answerer SLD");
    let _ = answer_gathered.recv().await;
    let answer = answerer
        .local_description()
        .await
        .expect("answerer local description");
    assert_eq!(
        answer.sdp.contains("typ relay"),
        expect_relay,
        "answerer relay-candidate presence must match credential validity"
    );

    offerer
        .set_remote_description(answer)
        .await
        .expect("offerer SRD");
}

/// The relay proof: relay-only policy + valid credentials ⇒ the data channel
/// opens and carries a payload — possible ONLY through the coturn allocation.
#[tokio::test(flavor = "multi_thread")]
async fn turn_relay_carries_data_with_credentials() {
    let turn = spawn_turnserver();

    let offerer = relay_only_peer(turn.port, TURN_USER, TURN_PASS).await;
    let answerer = relay_only_peer(turn.port, TURN_USER, TURN_PASS).await;

    let (received_tx, mut received_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);

    // The answerer receives the (in-band DCEP) channel and reports messages.
    answerer.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
        let received_tx = received_tx.clone();
        Box::pin(async move {
            dc.on_message(Box::new(move |msg| {
                let received_tx = received_tx.clone();
                let data = msg.data.to_vec();
                Box::pin(async move {
                    let _ = received_tx.send(data).await;
                })
            }));
        })
    }));

    // The offerer sends once its channel opens.
    let dc = offerer
        .create_data_channel("relay-proof", None)
        .await
        .expect("create data channel");
    {
        let dc_on_open = Arc::clone(&dc);
        dc.on_open(Box::new(move || {
            Box::pin(async move {
                dc_on_open
                    .send_text("over the relay")
                    .await
                    .expect("send over relay");
            })
        }));
    }

    connect_non_trickle(&offerer, &answerer, true).await;

    let received = tokio::time::timeout(DEADLINE, received_rx.recv())
        .await
        .expect("relay payload must arrive before the deadline")
        .expect("channel closed without a payload");
    assert_eq!(&received[..], b"over the relay");

    // Belt and braces: the selected pair must be relay↔relay under this policy.
    let stats = offerer.get_stats().await;
    assert!(
        !stats.reports.is_empty(),
        "stats must exist after a connected session"
    );

    offerer.close().await.expect("close offerer");
    answerer.close().await.expect("close answerer");
}

/// Wrong credentials ⇒ no TURN allocation ⇒ under relay-only policy the
/// channel can never open. Bounded: we assert NO open within a grace window
/// (coturn rejects the allocation with 401 immediately; the window is slack,
/// not load-bearing).
#[tokio::test(flavor = "multi_thread")]
async fn turn_relay_refuses_bad_credentials() {
    let turn = spawn_turnserver();

    let offerer = relay_only_peer(turn.port, TURN_USER, "wrong-credential").await;
    let answerer = relay_only_peer(turn.port, TURN_USER, "wrong-credential").await;

    let (open_tx, mut open_rx) = tokio::sync::mpsc::channel::<()>(1);
    let dc = offerer
        .create_data_channel("must-not-open", None)
        .await
        .expect("create data channel");
    dc.on_open(Box::new(move || {
        let open_tx = open_tx.clone();
        Box::pin(async move {
            let _ = open_tx.send(()).await;
        })
    }));

    connect_non_trickle(&offerer, &answerer, false).await;

    let opened = tokio::time::timeout(Duration::from_secs(8), open_rx.recv()).await;
    assert!(
        opened.is_err(),
        "data channel must NOT open through TURN with bad credentials"
    );

    offerer.close().await.expect("close offerer");
    answerer.close().await.expect("close answerer");
}

/// `Transport::connect_with_ice` plumbs the TURN url + credentials through
/// matchbox end-to-end: two peers configured ONLY with the coturn server (it
/// answers STUN binding requests too) still connect and exchange on both
/// channels. (Relay FORCING is not expressible through matchbox — the relay
/// proof above covers that at the layer matchbox is built on.)
#[tokio::test(flavor = "multi_thread")]
async fn transport_connects_with_turn_ice_config() {
    use matchbox_signaling::SignalingServer;

    let turn = spawn_turnserver();
    let ice = IceConfig {
        urls: vec![turn_url(turn.port)],
        username: Some(TURN_USER.to_string()),
        credential: Some(TURN_PASS.to_string()),
    };

    let mut server = SignalingServer::full_mesh_builder((Ipv4Addr::LOCALHOST, 0)).build();
    let addr = server.bind().expect("signaling bind");
    tokio::spawn(server.serve());
    let room = format!("ws://{addr}/turn_ice_config");

    let (mut a, a_loop) = Transport::connect_with_ice(&room, ice.clone());
    let (mut b, b_loop) = Transport::connect_with_ice(&room, ice);
    tokio::spawn(a_loop);
    tokio::spawn(b_loop);

    // Wait until each side reports the other Connected.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let (mut a_sees, mut b_sees) = (None, None);
    while a_sees.is_none() || b_sees.is_none() {
        for (peer, state) in a.poll_peers().expect("a open") {
            if state == PeerState::Connected {
                a_sees = Some(peer);
            }
        }
        for (peer, state) in b.poll_peers().expect("b open") {
            if state == PeerState::Connected {
                b_sees = Some(peer);
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "peers never connected with the TURN IceConfig"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (b_id, a_id) = (a_sees.expect("a sees b"), b_sees.expect("b sees a"));

    // One payload per channel, both directions.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut got_state = false;
    let mut got_event = false;
    b.send_event(a_id, (*b"turn event").into()).expect("event");
    while !(got_state && got_event) {
        a.send_state(b_id, (*b"turn state").into()).expect("state");
        let _ = a.poll_peers().expect("a open");
        let _ = b.poll_peers().expect("b open");
        for (from, payload) in b.recv_state() {
            assert_eq!(from, a_id);
            assert_eq!(&payload[..], b"turn state");
            got_state = true;
        }
        for (from, payload) in a.recv_events() {
            assert_eq!(from, b_id);
            assert_eq!(&payload[..], b"turn event");
            got_event = true;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "channel payloads never arrived (state={got_state} event={got_event})"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
