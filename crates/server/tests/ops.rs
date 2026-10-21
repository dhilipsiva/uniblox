//! The Mode-3 server binary is deployable (ADR-0052): it serves `/healthz` and `/metrics` on
//! `UNIBLOX_SERVER_HTTP_PORT`, logs JSON lines with `UNIBLOX_LOG_FORMAT=json`, and exits cleanly once
//! no client has been connected for `UNIBLOX_IDLE_SHUTDOWN_SECS`, so an orchestrator reclaims it.
//! Written before the implementation.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn get(port: u16, path: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let status = raw.split_whitespace().nth(1)?.parse().ok()?;
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    Some((status, body))
}

fn metric(body: &str, name: &str) -> Option<f64> {
    body.lines()
        .find(|l| l.split_whitespace().next() == Some(name))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_serves_health_and_metrics_and_exits_when_idle() {
    let mut signaling = services::build_signaling_server(
        (Ipv4Addr::LOCALHOST, 0),
        services::SessionRegistry::new(),
    );
    let signaling_addr = signaling.bind().expect("bind signaling");
    tokio::spawn(signaling.serve());
    let http_port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_server"))
        .env("UNIBLOX_SIGNALING_URL", format!("ws://{signaling_addr}/m3~1.0~0~ops"))
        .env("UNIBLOX_SERVER_HTTP_PORT", http_port.to_string())
        .env("UNIBLOX_IDLE_SHUTDOWN_SECS", "3")
        .env("UNIBLOX_LOG_FORMAT", "json")
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the server binary");

    let deadline = Instant::now() + Duration::from_secs(30);
    let health = loop {
        if let Some(answer) = get(http_port, "/healthz") {
            break answer;
        }
        assert!(Instant::now() < deadline, "/healthz never answered");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!((health.0, health.1.trim()), (200, "ok"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (status, body) = get(http_port, "/metrics").expect("/metrics");
    assert_eq!(status, 200);
    assert!(metric(&body, "uniblox_server_ticks_total").is_some_and(|t| t > 0.0), "{body}");
    assert_eq!(metric(&body, "uniblox_server_peers"), Some(0.0), "{body}");
    assert!(metric(&body, "uniblox_server_entities").is_some(), "{body}");

    // No client ever connects: the server exits cleanly after the idle timeout.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        assert!(Instant::now() < deadline, "the idle server did not exit");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(status.success(), "idle shutdown is a clean exit: {status}");

    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    let json_lines: Vec<serde_json::Value> = stderr
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert!(
        json_lines
            .iter()
            .any(|v| v["level"].is_string() && v["msg"].as_str().is_some_and(|m| m.contains("idle"))),
        "a JSON log line reports the idle shutdown:\n{stderr}"
    );
}
