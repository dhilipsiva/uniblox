//! The signaling server's HTTP side (ADR-0047): our own axum router and WebSocket handler, so that
//! the WebSocket layer enforces the message and frame limits, every connection holds a slot of the
//! per-IP and global connection caps, an idle connection is closed, and each peer's outbound queue
//! is bounded. The wire protocol is matchbox's (`matchbox_protocol`), so matchbox clients connect
//! unchanged.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use matchbox_protocol::{JsonPeerEvent, JsonPeerRequest, PeerId};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

use crate::limits::{IpLimiter, TokenBucket, per_second};
use crate::{
    ClientIp, JoinTokenKey, MAX_TOKEN_LIFETIME_SECS, SCOPE_SEP, SessionRegistry, SignalingConfig,
    SignalingLimits, TOKEN_PARAM, TOKEN_PATH, TURN_PATH, TurnConfig, bad_request,
    credential_expiry, engine_too_old, parse_engine, parse_next, parse_scope, turn_credential,
    unix_now, validate_room,
};

/// How long a closing peer's writer may take to flush before it is cut off.
const WRITER_GRACE: Duration = Duration::from_secs(1);

/// One message on its way to a peer, with what it counts against.
pub(crate) struct Outbound {
    msg: Message,
    len: usize,
    /// The peer whose signal this is (it holds a share of the queue); `None` for a control event.
    from: Option<PeerId>,
}

/// What one sending peer holds of a receiver's queue.
#[derive(Default)]
struct Held {
    msgs: usize,
    bytes: usize,
}

/// A peer's outbound accounting, shared by its senders and its writer.
#[derive(Default)]
struct Budget {
    /// Bytes queued and not yet written.
    bytes: AtomicUsize,
    /// What each sending peer has queued (its share of this queue, in messages and in bytes).
    per_sender: Mutex<HashMap<PeerId, Held>>,
}

impl Budget {
    /// The writer wrote (or dropped) `out`: release what it held.
    fn release(&self, out: &Outbound) {
        self.bytes.fetch_sub(out.len, Ordering::Relaxed);
        if let Some(from) = out.from
            && let Ok(mut shares) = self.per_sender.lock()
            && let Some(held) = shares.get_mut(&from)
        {
            held.msgs = held.msgs.saturating_sub(1);
            held.bytes = held.bytes.saturating_sub(out.len);
            if held.msgs == 0 {
                shares.remove(&from);
            }
        }
    }
}

/// One peer's outbound side: a queue to its socket writer, bounded in messages and in bytes, with
/// room kept for control events (`NewPeer`, `PeerLeft`) and a share per sending peer. Cheap to
/// clone; the registry holds one per session member.
#[derive(Clone)]
pub(crate) struct PeerTx {
    tx: mpsc::Sender<Outbound>,
    budget: Arc<Budget>,
    max_bytes: usize,
    max_msgs: usize,
}

/// A message's size against the byte budget.
fn msg_len(msg: &Message) -> usize {
    match msg {
        Message::Text(t) => t.len(),
        Message::Binary(b) | Message::Ping(b) | Message::Pong(b) => b.len(),
        Message::Close(_) => 0,
    }
}

impl PeerTx {
    /// A handle whose queue nothing drains (unit tests of the registry).
    #[cfg(test)]
    pub(crate) fn detached() -> (Self, mpsc::Receiver<Outbound>) {
        let (tx, rx) = mpsc::channel(1);
        (
            Self {
                tx,
                budget: Arc::default(),
                max_bytes: usize::MAX,
                max_msgs: 1,
            },
            rx,
        )
    }

    fn enqueue(&self, msg: Message, from: Option<PeerId>, byte_cap: usize) -> bool {
        let len = msg_len(&msg);
        if self.budget.bytes.fetch_add(len, Ordering::Relaxed) + len > byte_cap {
            self.budget.bytes.fetch_sub(len, Ordering::Relaxed);
            return false;
        }
        if self.tx.try_send(Outbound { msg, len, from }).is_err() {
            self.budget.bytes.fetch_sub(len, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Queue a control event (`IdAssigned`, `NewPeer`, `PeerLeft`): it may use the whole queue,
    /// including the room signals cannot take, so a flooded peer still learns who comes and goes.
    pub(crate) fn send_control(&self, msg: Message) -> bool {
        self.enqueue(msg, None, self.max_bytes)
    }

    /// Queue `from`'s signal; whether it was queued. It is dropped when `from` already holds its share
    /// (a quarter of the queue's messages, and a quarter of its bytes) or when only the control
    /// reserve (an eighth of each) is left. A sender holding nothing may always queue one signal, so a
    /// byte share smaller than a signal cannot shut it out. The relay charges a dropped signal to its
    /// SENDER, never to this receiver, whose queue a sender can fill at will; a receiver that does not
    /// read is found by its writer stalling instead.
    pub(crate) fn send_signal(&self, from: PeerId, msg: Message) -> bool {
        let share_msgs = (self.max_msgs / 4).max(1);
        let share_bytes = self.max_bytes / 4;
        let reserve = (self.max_msgs / 8).max(1);
        if self.tx.capacity() <= reserve {
            return false;
        }
        let len = msg_len(&msg);
        let Ok(mut shares) = self.budget.per_sender.lock() else {
            return false;
        };
        let held = shares.entry(from).or_default();
        if held.msgs >= share_msgs
            || (held.msgs > 0 && held.bytes.saturating_add(len) > share_bytes)
        {
            return false;
        }
        let byte_cap = self.max_bytes - self.max_bytes / 8;
        if !self.enqueue(msg, Some(from), byte_cap) {
            if held.msgs == 0 {
                shares.remove(&from);
            }
            return false;
        }
        held.msgs += 1;
        held.bytes += len;
        true
    }
}

/// Send one event to many peers (call with the registry lock released).
pub(crate) fn broadcast(peers: &[PeerTx], event: &JsonPeerEvent) {
    if peers.is_empty() {
        return;
    }
    let text = event.to_string();
    for peer in peers {
        let _ = peer.send_control(Message::Text(text.clone().into()));
    }
}

/// Open connections per client IP and in total.
#[derive(Debug, Default)]
struct ConnCounts {
    per_ip: HashMap<IpAddr, usize>,
    total: usize,
}

/// A connection's slot in the connection caps; released when dropped (the connection ended, or its
/// upgrade never completed).
struct ConnSlot {
    counts: Arc<Mutex<ConnCounts>>,
    ip: IpAddr,
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        if let Ok(mut c) = self.counts.lock() {
            c.total = c.total.saturating_sub(1);
            if let Some(n) = c.per_ip.get_mut(&self.ip) {
                *n -= 1;
                if *n == 0 {
                    c.per_ip.remove(&self.ip);
                }
            }
        }
    }
}

/// What the gate admitted a connection with.
struct Admit {
    next: Option<usize>,
    slot: ConnSlot,
}

/// Everything the handlers share.
struct Shared {
    registry: SessionRegistry,
    auth: Option<JoinTokenKey>,
    issue_tokens: bool,
    token_lifetime: Duration,
    turn: Option<TurnConfig>,
    limits: SignalingLimits,
    client_ip: ClientIp,
    rate: IpLimiter,
    conns: Arc<Mutex<ConnCounts>>,
}

fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        "too many connection attempts from this address; retry later",
    )
        .into_response()
}

fn unauthorized(reason: impl std::fmt::Display) -> Response {
    (StatusCode::UNAUTHORIZED, reason.to_string()).into_response()
}

impl Shared {
    /// The connection gate, in order: the per-IP attempt rate (429), the room (400), the join token
    /// when a key is set (401), the version filter (426/400), the `?next` cap (400), the session cap
    /// (503), then a slot in the connection caps (429 per IP, 503 in total).
    // The `Err` is the reasoned refusal sent to the client; its size is the point.
    #[allow(clippy::result_large_err)]
    fn gate(
        &self,
        origin: SocketAddr,
        room: &str,
        query: &HashMap<String, String>,
        headers: &HeaderMap,
    ) -> Result<Admit, Response> {
        let ip = self.client_ip.key(origin, headers);
        if !self.rate.allow(ip, &self.limits) {
            return Err(too_many_requests());
        }
        validate_room(room).map_err(bad_request)?;
        if let Some(key) = &self.auth {
            let token = query
                .get(TOKEN_PARAM)
                .ok_or_else(|| unauthorized("missing join token"))?;
            key.verify(token, room, unix_now()).map_err(unauthorized)?;
        }
        if room.contains(SCOPE_SEP) {
            let scope = parse_scope(room).map_err(|e| bad_request(e.to_string()))?;
            let engine = parse_engine(query).map_err(|e| bad_request(e.to_string()))?;
            if engine < scope.min_engine {
                return Err(engine_too_old(engine, scope.min_engine));
            }
        }
        let next = parse_next(query).map_err(|e| bad_request(e.to_string()))?;
        match next {
            Some(n) if n > self.limits.max_session_peers => {
                return Err(bad_request(format!(
                    "next={n} is above this server's session cap of {}",
                    self.limits.max_session_peers
                )));
            }
            None if self.registry.peer_count(room) >= self.limits.max_session_peers => {
                return Err((StatusCode::SERVICE_UNAVAILABLE, "session full").into_response());
            }
            _ => {}
        }
        let slot = self.take_slot(ip)?;
        Ok(Admit { next, slot })
    }

    #[allow(clippy::result_large_err)]
    fn take_slot(&self, ip: IpAddr) -> Result<ConnSlot, Response> {
        let Ok(mut c) = self.conns.lock() else {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "server busy").into_response());
        };
        if c.total >= self.limits.max_connections {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "server full").into_response());
        }
        if c.per_ip.get(&ip).copied().unwrap_or(0) >= self.limits.max_connections_per_ip {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "too many open connections from this address",
            )
                .into_response());
        }
        *c.per_ip.entry(ip).or_insert(0) += 1;
        c.total += 1;
        Ok(ConnSlot {
            counts: Arc::clone(&self.conns),
            ip,
        })
    }

    fn issue(
        &self,
        peer: SocketAddr,
        headers: &HeaderMap,
        query: &HashMap<String, String>,
    ) -> Response {
        let Some(key) = self.auth.as_ref().filter(|_| self.issue_tokens) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if !self
            .rate
            .allow(self.client_ip.key(peer, headers), &self.limits)
        {
            return too_many_requests();
        }
        let Some(room) = query.get("room") else {
            return bad_request("missing room");
        };
        if let Err(reason) = validate_room(room) {
            return bad_request(reason);
        }
        let expires = unix_now()
            + self
                .token_lifetime
                .as_secs()
                .clamp(1, MAX_TOKEN_LIFETIME_SECS);
        let body = serde_json::json!({ "token": key.mint(room, expires), "expires": expires });
        (
            [
                (header::CONTENT_TYPE, "application/json"),
                // A token is a bearer credential: never cached, by the browser or a shared cache.
                (header::CACHE_CONTROL, "no-store"),
                // The hosted demo's page may be served from another origin.
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            body.to_string(),
        )
            .into_response()
    }
}

impl Shared {
    /// A time-limited TURN credential (ADR-0050), only for a client with a valid join token for the
    /// room it names. 404 without TURN or auth configured.
    fn turn_credentials(
        &self,
        peer: SocketAddr,
        headers: &HeaderMap,
        query: &HashMap<String, String>,
    ) -> Response {
        let (Some(turn), Some(key)) = (&self.turn, &self.auth) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if !self
            .rate
            .allow(self.client_ip.key(peer, headers), &self.limits)
        {
            return too_many_requests();
        }
        let Some(room) = query.get("room") else {
            return bad_request("missing room");
        };
        if let Err(reason) = validate_room(room) {
            return bad_request(reason);
        }
        let Some(token) = query.get(TOKEN_PARAM) else {
            return unauthorized("missing join token");
        };
        let now = unix_now();
        let token_expiry = match key.verify_expiry(token, room, now) {
            Ok(expiry) => expiry,
            Err(err) => return unauthorized(err),
        };
        // On a grid, so one token keeps one username for a while; never past the token (turn.rs).
        let expiry = credential_expiry(now, turn.ttl, token_expiry);
        let ttl = expiry.saturating_sub(now);
        // The label is derived from the token, so redeeming a token again within a grid step yields the
        // same username, and coturn's per-user quota binds per token.
        let digest = Sha256::digest(token.as_bytes());
        let label: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        let credential = turn_credential(&turn.secret, &label, expiry);
        let body = serde_json::json!({
            "urls": turn.urls,
            "username": credential.username,
            "credential": credential.credential,
            "ttl": ttl,
        });
        (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            body.to_string(),
        )
            .into_response()
    }
}

async fn turn(
    State(s): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    with_cors(s.turn_credentials(peer, &headers, &query))
}

/// `GET /`: no room. Counted against the attempt rate like any other connection.
async fn no_room(
    State(s): State<Arc<Shared>>,
    ConnectInfo(origin): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !s.rate.allow(s.client_ip.key(origin, &headers), &s.limits) {
        return too_many_requests();
    }
    StatusCode::UNAUTHORIZED.into_response()
}

async fn join(
    State(s): State<Arc<Shared>>,
    ConnectInfo(origin): ConnectInfo<SocketAddr>,
    Path(room): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let admit = match s.gate(origin, &room, &query, &headers) {
        Ok(admit) => admit,
        Err(refusal) => return refusal,
    };
    let limits = s.limits;
    ws.max_message_size(limits.max_signal_bytes)
        .max_frame_size(limits.max_signal_bytes)
        .max_write_buffer_size(limits.max_write_buffer_bytes)
        .on_upgrade(move |socket| run_peer(s, socket, room, admit))
}

/// The HTTP endpoints may be called from a page on another origin (the hosted demo): every answer,
/// refusals included, carries the CORS header so the page can read it.
fn with_cors(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::HeaderValue::from_static("*"),
    );
    response
}

async fn token(
    State(s): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    with_cors(s.issue(peer, &headers, &query))
}

/// One peer's connection: register it, announce it, relay its signals within its session, and
/// clean up when it ends (it closed, went idle, flooded, or stopped reading).
async fn run_peer(s: Arc<Shared>, socket: WebSocket, room: String, admit: Admit) {
    let Admit { next, slot: _slot } = admit;
    let limits = s.limits;
    let peer = PeerId(Uuid::new_v4());
    let (mut sink, mut stream) = socket.split();
    let max_msgs = limits.max_outbound_queue.max(1);
    let (tx, mut rx) = mpsc::channel::<Outbound>(max_msgs);
    let budget = Arc::new(Budget::default());
    let kill = Arc::new(Notify::new());
    let writer_kill = Arc::clone(&kill);
    let writer_budget = Arc::clone(&budget);
    let stall = limits.writer_stall_timeout;
    let mut writer = tokio::spawn(async move {
        while let Some(mut out) = rx.recv().await {
            let msg = std::mem::replace(&mut out.msg, Message::Close(None));
            let sent = tokio::time::timeout(stall, sink.send(msg)).await;
            writer_budget.release(&out);
            match sent {
                Ok(Ok(())) => {}
                Ok(Err(_)) => break,
                Err(_) => {
                    tracing::warn!("disconnecting signaling peer {peer:?}: it stopped reading");
                    break;
                }
            }
        }
        // The socket is gone or stalled: end the peer's read loop first (a close to a peer that does
        // not read would block), then close, bounded.
        writer_kill.notify_one();
        let _ = tokio::time::timeout(stall, sink.close()).await;
    });
    let me = PeerTx {
        tx,
        budget,
        max_bytes: limits.max_outbound_bytes,
        max_msgs,
    };
    let _ = me.send_control(Message::Text(
        JsonPeerEvent::IdAssigned(peer).to_string().into(),
    ));

    if let Some((key, existing)) =
        s.registry
            .join_local(&room, peer, me.clone(), next, limits.max_session_peers)
    {
        broadcast(&existing, &JsonPeerEvent::NewPeer(peer));
        // Hold no other member's sender beyond this point: it would keep its socket open.
        drop(existing);
        s.registry.mirror_join(&key, peer).await;

        relay(&s, peer, &mut stream, &kill).await;

        if let Some((key, remaining)) = s.registry.leave_local(peer) {
            broadcast(&remaining, &JsonPeerEvent::PeerLeft(peer));
            drop(remaining);
            s.registry.mirror_leave(&key, peer).await;
        }
    }
    // Our last sender: the writer drains, closes the socket and ends; one that is stuck on a peer
    // that does not read is cut off.
    drop(me);
    drop(stream);
    if tokio::time::timeout(WRITER_GRACE, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

/// The relay loop: every frame spends a token of the connection's bucket (dropped past it, the
/// connection closed after `abuse_close_after` net drops); a `Signal` reaches its receiver only
/// inside the sender's session, never the sender itself.
async fn relay(
    s: &Shared,
    peer: PeerId,
    stream: &mut futures::stream::SplitStream<WebSocket>,
    kill: &Notify,
) {
    let limits = s.limits;
    let refill = per_second(limits.signals_per_sec);
    let mut bucket = TokenBucket::full(limits.signals_burst, Instant::now());
    let mut dropped: u32 = 0;
    loop {
        let frame = tokio::select! {
            biased;
            () = kill.notified() => return,
            frame = tokio::time::timeout(limits.idle_timeout, stream.next()) => frame,
        };
        let msg = match frame {
            Err(_) => {
                tracing::info!("closing idle signaling peer {peer:?}");
                return;
            }
            Ok(None | Some(Err(_))) => return,
            Ok(Some(Ok(msg))) => msg,
        };
        let charge = |dropped: &mut u32| {
            *dropped += 1;
            *dropped >= limits.abuse_close_after
        };
        if !bucket.try_take(limits.signals_burst, refill, Instant::now()) {
            if charge(&mut dropped) {
                tracing::warn!("closing signaling peer {peer:?}: {dropped} frames over its limits");
                return;
            }
            continue;
        }
        // A relayed signal either reached its receiver's queue (earning back one drop) or overflowed it
        // (charged as a drop). Every other frame (a keep-alive, a malformed one, a signal to oneself or
        // to no one) does neither: only delivering earns a drop back.
        let mut delivered = false;
        let mut overflowed = false;
        match msg {
            Message::Text(text) => match text.as_str().parse::<JsonPeerRequest>() {
                Ok(JsonPeerRequest::Signal { receiver, data }) => {
                    if receiver != peer
                        && let Some(target) = s.registry.target(peer, receiver)
                    {
                        let event = JsonPeerEvent::Signal { sender: peer, data }.to_string();
                        // Re-encoding can grow a signal (number formatting); never relay past the cap.
                        if event.len() > limits.max_signal_bytes
                            || !target.send_signal(peer, Message::Text(event.into()))
                        {
                            overflowed = true;
                        } else {
                            delivered = true;
                        }
                    }
                    // A signal to oneself, or to an unknown or cross-session receiver, silently
                    // drops: the isolation boundary.
                }
                Ok(JsonPeerRequest::KeepAlive) | Err(_) => {}
            },
            Message::Close(_) => return,
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) => {}
        }
        if overflowed {
            if charge(&mut dropped) {
                tracing::warn!("closing signaling peer {peer:?}: it overflows other peers' queues");
                return;
            }
        } else if delivered {
            dropped = dropped.saturating_sub(1);
        }
    }
}

/// The uniblox signaling server: bind, then serve. Mirrors matchbox's `SignalingServer` API.
pub struct SignalingServer {
    addr: SocketAddr,
    router: Router,
    listener: Option<std::net::TcpListener>,
}

impl SignalingServer {
    /// Bind the listening socket; returns the bound address (useful with port 0).
    pub fn bind(&mut self) -> std::io::Result<SocketAddr> {
        let listener = std::net::TcpListener::bind(self.addr)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        self.listener = Some(listener);
        Ok(addr)
    }

    /// The bound address, once bound.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.listener.as_ref().and_then(|l| l.local_addr().ok())
    }

    /// Serve until the listener fails, binding first if needed.
    pub async fn serve(mut self) -> std::io::Result<()> {
        let listener = match self.listener.take() {
            Some(listener) => listener,
            None => {
                self.bind()?;
                self.listener
                    .take()
                    .ok_or_else(|| std::io::Error::other("bind produced no listener"))?
            }
        };
        let listener = tokio::net::TcpListener::from_std(listener)?;
        axum::serve(
            listener,
            self.router
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    }
}

/// Assemble the server (see [`crate::build_signaling_server_with`]).
pub(crate) fn build(
    addr: SocketAddr,
    registry: SessionRegistry,
    config: SignalingConfig,
) -> SignalingServer {
    if config.issue_tokens && config.auth.is_none() {
        tracing::warn!("issue_tokens needs a join-token key; the token endpoint is off");
    }
    let shared = Arc::new(Shared {
        registry,
        auth: config.auth,
        issue_tokens: config.issue_tokens,
        token_lifetime: config.token_lifetime,
        turn: config.turn,
        limits: config.limits,
        client_ip: config.client_ip,
        rate: IpLimiter::default(),
        conns: Arc::default(),
    });
    let router = Router::new()
        .route("/", get(no_room))
        .route(TOKEN_PATH, get(token))
        .route(TURN_PATH, get(turn))
        .route("/{room}", get(join))
        .with_state(shared);
    SignalingServer {
        addr,
        router,
        listener: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer_tx(max_msgs: usize, max_bytes: usize) -> (PeerTx, mpsc::Receiver<Outbound>) {
        let (tx, rx) = mpsc::channel(max_msgs);
        let peer = PeerTx {
            tx,
            budget: Arc::default(),
            max_bytes,
            max_msgs,
        };
        (peer, rx)
    }

    fn text(len: usize) -> Message {
        Message::Text("x".repeat(len).into())
    }

    /// A sender's share is a quarter of the queue in messages and in bytes, and writing a message
    /// gives back both, even while the sender still has others queued (a reading peer never sees
    /// that state long enough for the integration tests to catch a leak).
    #[test]
    fn a_share_binds_in_messages_and_bytes_and_is_given_back() {
        let a = PeerId(Uuid::from_u128(1));
        let c = PeerId(Uuid::from_u128(2));
        // A share of 16 messages and 16 KiB.
        let (peer, mut rx) = peer_tx(64, 64 * 1024);
        for _ in 0..4 {
            assert!(peer.send_signal(a, text(4096)));
        }
        assert!(!peer.send_signal(a, text(4096)), "A's byte share is full");
        assert!(peer.send_signal(c, text(4096)), "C has its own share");
        let written = rx.try_recv().expect("A's first signal");
        peer.budget.release(&written);
        assert!(
            peer.send_signal(a, text(4096)),
            "writing gave back its bytes"
        );
        for _ in 0..16 {
            let _ = peer.send_signal(c, text(1));
        }
        assert!(!peer.send_signal(c, text(1)), "C's message share is full");
    }

    fn sender(n: u128) -> PeerId {
        PeerId(Uuid::from_u128(n))
    }

    /// Signals stop with an eighth of the queue's messages left, which control events may use.
    #[test]
    fn signals_leave_the_message_reserve_to_control_events() {
        // A share of 16 messages; the byte budget is out of reach.
        let (peer, _rx) = peer_tx(64, 64 << 20);
        let mut admitted = 0;
        for s in 0..5 {
            for _ in 0..16 {
                admitted += usize::from(peer.send_signal(sender(s), text(1)));
            }
        }
        assert_eq!(admitted, 56, "signals stop at the reserve");
        for _ in 0..8 {
            assert!(peer.send_control(text(1)), "control events use the reserve");
        }
        assert!(!peer.send_control(text(1)), "and the queue is then full");
    }

    /// Signals stop with an eighth of the byte budget left, which control events may use.
    #[test]
    fn signals_leave_the_byte_reserve_to_control_events() {
        // A byte share of 16 KiB; the signal byte cap is 56 KiB; the message count is out of reach.
        let (peer, _rx) = peer_tx(1024, 64 * 1024);
        let mut admitted = 0;
        for s in 0..5 {
            for _ in 0..4 {
                admitted += usize::from(peer.send_signal(sender(s), text(4096)));
            }
        }
        assert_eq!(admitted, 14, "56 KiB of 4 KiB signals");
        assert!(
            peer.send_control(text(4096)),
            "a control event fits in the reserve"
        );
    }

    /// The reserve check and the enqueue happen under one lock, so senders racing at the edge of the
    /// reserve admit one signal between them, not several.
    #[test]
    fn concurrent_senders_cannot_take_the_reserve() {
        for trial in 0..300 {
            // 55 of 64 queued: one more signal fits before the reserve of 8.
            let (peer, _rx) = peer_tx(64, 64 << 20);
            for i in 0..55 {
                assert!(peer.send_signal(sender(1000 + i / 16), text(1)));
            }
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
            let racers: Vec<_> = (0..16)
                .map(|n| {
                    let (peer, barrier) = (peer.clone(), std::sync::Arc::clone(&barrier));
                    std::thread::spawn(move || {
                        barrier.wait();
                        peer.send_signal(sender(n), text(1))
                    })
                })
                .collect();
            let admitted = racers
                .into_iter()
                .filter_map(|r| r.join().ok())
                .filter(|&ok| ok)
                .count();
            assert_eq!(admitted, 1, "trial {trial}: the reserve was entered");
        }
    }

    /// A sender holding nothing may queue one signal larger than its byte share, so a small budget
    /// cannot shut a sender out; the overall byte cap still bounds it.
    #[test]
    fn an_empty_share_admits_one_signal() {
        let a = PeerId(Uuid::from_u128(1));
        // A byte share of 1 KiB; the signal cap is 3.5 KiB.
        let (peer, _rx) = peer_tx(64, 4096);
        assert!(peer.send_signal(a, text(2048)));
        assert!(!peer.send_signal(a, text(10)), "the share is spent");
        let b = PeerId(Uuid::from_u128(2));
        assert!(
            !peer.send_signal(b, text(2048)),
            "the overall cap still binds"
        );
    }
}
