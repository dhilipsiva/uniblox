//! Operations endpoints (ADR-0052): `/healthz` answers while the server serves, `/metrics` exposes
//! Prometheus text gauges and counters, and neither name can be used as a room. Written before the
//! implementation.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures::StreamExt;
use matchbox_protocol::JsonPeerEvent;
use services::{SessionRegistry, build_signaling_server, validate_room};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

async fn boot() -> SocketAddr {
    let mut server = build_signaling_server((Ipv4Addr::LOCALHOST, 0), SessionRegistry::new());
    let addr = server.bind().expect("bind");
    tokio::spawn(server.serve());
    addr
}

async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.expect("tcp connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("http timeout")
        .expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status line");
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/// The value of an unlabelled metric in Prometheus text.
fn metric(body: &str, name: &str) -> Option<f64> {
    body.lines()
        .find(|l| l.split_whitespace().next() == Some(name))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

#[tokio::test]
async fn healthz_and_metrics_answer() {
    let addr = boot().await;
    let (status, body) = get(addr, "/healthz").await;
    assert_eq!((status, body.trim()), (200, "ok"));
    let (status, body) = get(addr, "/metrics").await;
    assert_eq!(status, 200);
    for name in [
        "uniblox_signaling_connections",
        "uniblox_signaling_sessions",
        "uniblox_signaling_peers",
        "uniblox_signaling_frames_dropped_total",
        "uniblox_signaling_refused_total",
        "uniblox_signaling_tokens_issued_total",
        "uniblox_signaling_turn_credentials_total",
    ] {
        assert!(metric(&body, name).is_some(), "{name} in:\n{body}");
    }
    assert_eq!(metric(&body, "uniblox_signaling_connections"), Some(0.0));

    let (mut ws, _) = connect_async(format!("ws://{addr}/m2~1.0~0~ops?engine=1"))
        .await
        .expect("connect");
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(t) = msg
            && matches!(t.as_str().parse(), Ok(JsonPeerEvent::IdAssigned(_)))
        {
            break;
        }
    }
    let mut seen = None;
    for _ in 0..50 {
        let (_, body) = get(addr, "/metrics").await;
        seen = Some(body.clone());
        if metric(&body, "uniblox_signaling_peers") == Some(1.0) {
            assert_eq!(metric(&body, "uniblox_signaling_connections"), Some(1.0));
            assert_eq!(metric(&body, "uniblox_signaling_sessions"), Some(1.0));
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the peer never showed in /metrics: {seen:?}");
}

#[tokio::test]
async fn a_refused_connection_is_counted() {
    let addr = boot().await;
    // A malformed scope is refused at the gate.
    let _ = connect_async(format!("ws://{addr}/m9~bad~scope~x?engine=1")).await;
    let (_, body) = get(addr, "/metrics").await;
    assert_eq!(
        metric(&body, "uniblox_signaling_refused_total"),
        Some(1.0),
        "{body}"
    );
}

#[test]
fn healthz_and_metrics_are_not_rooms() {
    for room in ["healthz", "metrics"] {
        assert!(validate_room(room).is_err(), "{room}");
    }
    assert!(validate_room("arena").is_ok());
}
