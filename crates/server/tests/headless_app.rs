//! M3 ★ / M4 — the REAL headless server App (bevy_app + bevy_time assembly):
//! fixed 64 Hz simulation in FixedUpdate, network pump in Update on its own
//! wall-clock cadence (network tick ≠ fixed tick). An external raw client
//! converges against it over real transport. Locked FIRST (TDD). ADR-0014.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use engine_core::{Owner, Position, Velocity};
use matchbox_signaling::SignalingServer;
use protocol::PeerId;
use replication::Replication;
use server::{NET_INTERVAL, TickCount, build_server_app, build_server_app_focused};
use transport::{PeerState, Transport};

const DEADLINE: Duration = Duration::from_secs(120); // generous: bounds hangs, not CPU contention (full parallel suite runs several e2e binaries)
const POLL: Duration = Duration::from_millis(5);

fn start_signaling() -> String {
    let mut server = SignalingServer::full_mesh_builder((Ipv4Addr::LOCALHOST, 0)).build();
    let addr = server.bind().expect("signaling server must bind");
    tokio::spawn(server.serve());
    format!("ws://{addr}/headless_app_test")
}

/// A raw test client (no App): world + replication + transport pump. A full
/// symmetric peer — it receives AND (when it owns entities) sends state, and
/// acks the streams it applies. Counts received/sent state messages so a test
/// can watch a confirmed value go quiet (ADR-0020 delta baseline over the pump).
struct Client {
    local: PeerId,
    world: World,
    repl: Replication,
    transport: Transport,
    received_state_msgs: usize,
    state_msgs_sent: usize,
}

impl Client {
    async fn connect(room: &str) -> Client {
        let (mut transport, loop_fut) = Transport::connect_hermetic(room);
        tokio::spawn(loop_fut);
        let deadline = tokio::time::Instant::now() + DEADLINE;
        let uuid = loop {
            if let Some(id) = transport.id() {
                break id;
            }
            assert!(tokio::time::Instant::now() < deadline, "no signaling id");
            tokio::time::sleep(POLL).await;
        };
        let id = PeerId::from_uuid_bytes(*uuid.0.as_bytes());
        let mut world = World::new();
        engine_core::insert_sim(&mut world, id, 1.0 / 64.0);
        let repl = Replication::new(&mut world);
        Client {
            local: id,
            world,
            repl,
            transport,
            received_state_msgs: 0,
            state_msgs_sent: 0,
        }
    }

    /// Spawn a client-OWNED entity so this peer replicates it to the server
    /// (Mode-2-shaped: the server holds a proxy it does not simulate). Used to
    /// drive the server's ack-routing (`net_pump` `drain_acks`).
    fn spawn_owned_entity(&mut self, pos: Position, vel: Velocity) -> Entity {
        engine_core::spawn_owned(&mut self.world, self.local, pos, vel)
    }

    fn pump(&mut self) {
        let peers: Vec<_> = self.transport.poll_peers().expect("transport open");
        for (peer, state) in peers {
            if matches!(state, PeerState::Connected) {
                // Track the peer so collect_all produces a per-peer outbox for
                // it (a client that owns nothing sends an empty outbox — inert).
                self.repl
                    .on_peer_connected(PeerId::from_uuid_bytes(*peer.0.as_bytes()));
            }
        }
        for (from, bytes) in self.transport.recv_events() {
            let from = PeerId::from_uuid_bytes(*from.0.as_bytes());
            self.repl.apply_events(&mut self.world, from, &bytes);
        }
        for (from, bytes) in self.transport.recv_state() {
            self.received_state_msgs += 1;
            let from = PeerId::from_uuid_bytes(*from.0.as_bytes());
            self.repl.apply_state(&mut self.world, from, &bytes);
        }

        // Ack the streams we applied (ADR-0020) so each SENDER advances its delta
        // baseline and a confirmed value goes quiet — the missing client-side
        // wiring, mirroring the server's net_pump. Map protocol id → transport peer.
        let connected: Vec<_> = self.transport.connected_peers().collect();
        for (target, ack) in self.repl.drain_acks() {
            if let Some(peer) = connected
                .iter()
                .find(|p| PeerId::from_uuid_bytes(*p.0.as_bytes()) == target)
            {
                let _ = self.transport.send_event(*peer, ack);
            }
        }

        // Anti-entropy resync sends (ADR-0024), mirroring the server's net_pump.
        // Only drain_resync_requests fires here (the client received the server's
        // Digest and asks for the fix); collect_resync / drain_resync_responses are
        // empty + inert (this client owns nothing) — sent for symmetry with a real
        // client pump, which must adopt the same three sends. Receive needs nothing
        // new: recv_events → apply_events already applies the incoming Digest +
        // ResyncSpawn.
        let mut resync: Vec<(PeerId, Box<[u8]>)> = self.repl.drain_resync_requests();
        resync.extend(self.repl.drain_resync_responses(&mut self.world));
        resync.extend(self.repl.collect_resync(&mut self.world));
        for (target, bytes) in resync {
            if let Some(peer) = connected
                .iter()
                .find(|p| PeerId::from_uuid_bytes(*p.0.as_bytes()) == target)
            {
                let _ = self.transport.send_event(*peer, bytes);
            }
        }

        // Coordinator ownership arbitration (ADR-0025 A / ADR-0028): if WE are the
        // coordinator, arbitrate queued claims into OwnershipCommit/ClaimRejected —
        // mirroring net_pump's drain_commits so a Client can act as the coordinator.
        for (target, bytes) in self.repl.drain_commits(&mut self.world) {
            if let Some(peer) = connected
                .iter()
                .find(|p| PeerId::from_uuid_bytes(*p.0.as_bytes()) == target)
            {
                let _ = self.transport.send_event(*peer, bytes);
            }
        }

        // Send our OWNED entities' state (Mode-2-shaped) to each tracked peer.
        // A client that owns nothing yields empty outboxes — transparent to the
        // Mode-3 receive-only tests; drives the server's ack-routing in return.
        // NB: unlike net_pump this has NO NET_INTERVAL accumulator — it sends at
        // poll rate (~200 Hz), which amplifies an unconfirmed value's re-sends
        // (do not read state_msgs_sent as a ~20 Hz cadence — auditor N3).
        for (target, out) in self.repl.collect_all(&mut self.world) {
            let Some(peer) = connected
                .iter()
                .find(|p| PeerId::from_uuid_bytes(*p.0.as_bytes()) == target)
            else {
                continue;
            };
            if let Some(state) = out.state {
                self.state_msgs_sent += 1;
                let _ = self.transport.send_state(*peer, state);
            }
            for ev in out.events {
                let _ = self.transport.send_event(*peer, ev);
            }
        }
    }

    fn server_proxies(&mut self, server_id: PeerId) -> Vec<Position> {
        let mut found: Vec<(u32, Position)> = self
            .world
            .query::<(Entity, &Owner, &Position)>()
            .iter(&self.world)
            .filter(|(_, o, _)| o.0 == server_id)
            .map(|(e, _, p)| (e.index_u32(), *p))
            .collect();
        found.sort_by_key(|(idx, _)| *idx);
        found.into_iter().map(|(_, p)| p).collect()
    }

    /// The single server-owned proxy Entity (panics if not exactly one) — for
    /// injecting a divergence directly into the proxy's `Position`.
    fn server_proxy_entity(&mut self, server_id: PeerId) -> Entity {
        let found: Vec<Entity> = self
            .world
            .query::<(Entity, &Owner)>()
            .iter(&self.world)
            .filter(|(_, o)| o.0 == server_id)
            .map(|(e, _)| e)
            .collect();
        assert_eq!(found.len(), 1, "expected exactly one server-owned proxy");
        found[0]
    }

    /// CLAIM an entity via the coordinator (ADR-0025 A) — route the
    /// `ClaimOwnership` to whoever is the coordinator (or record locally if we are).
    fn claim(&mut self, entity: Entity) {
        if let Some((target, bytes)) = self.repl.claim_ownership(&mut self.world, entity) {
            let connected: Vec<_> = self.transport.connected_peers().collect();
            if let Some(peer) = connected
                .iter()
                .find(|p| PeerId::from_uuid_bytes(*p.0.as_bytes()) == target)
            {
                let _ = self.transport.send_event(*peer, bytes);
            }
        }
    }

    fn owner_of(&self, entity: Entity) -> Option<PeerId> {
        self.world.get::<Owner>(entity).map(|o| o.0)
    }

    /// Close the transport (simulate this peer leaving the session).
    fn close(&mut self) {
        self.transport.close();
    }
}

/// Connect a server transport to in-process signaling and wait for its
/// signaling id (the protocol `PeerId`).
async fn connect_server_transport(room: &str) -> (Transport, PeerId) {
    let (mut transport, loop_fut) = Transport::connect_hermetic(room);
    tokio::spawn(loop_fut);
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let uuid = loop {
        if let Some(id) = transport.id() {
            break id;
        }
        assert!(tokio::time::Instant::now() < deadline, "no signaling id");
        tokio::time::sleep(POLL).await;
    };
    (transport, PeerId::from_uuid_bytes(*uuid.0.as_bytes()))
}

/// Boot a real server App against in-process signaling and return it with its
/// protocol id (the App is driven manually via `app.update()` — no blocking
/// `run()` in tests).
async fn boot_server(room: &str, entities: usize) -> (bevy_app::App, PeerId) {
    let (transport, server_id) = connect_server_transport(room).await;
    (build_server_app(transport, server_id, entities), server_id)
}

/// Boot a FOCUSED server App (ADR-0023 c): each connecting client gets a
/// server-owned avatar it controls and an AOI focused on that avatar with the
/// given radius. Zero demo entities — spawn the scene manually.
async fn boot_focused_server(room: &str, focus_radius: f32) -> (bevy_app::App, PeerId) {
    let (transport, server_id) = connect_server_transport(room).await;
    (
        build_server_app_focused(transport, server_id, 0, focus_radius),
        server_id,
    )
}

/// M3 ★ the headless App: client converges to the server's entities; the
/// fixed clock drives the sim at ≈64 Hz; every server entity is server-owned.
#[tokio::test(flavor = "multi_thread")]
async fn headless_app_converges_and_ticks_at_64hz() {
    let room = start_signaling();
    let (mut app, server_id) = boot_server(&room, 2).await;
    let mut client = Client::connect(&room).await;

    // Phase 1: converge (two advancing observation points on entity 0).
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut first_obs: Option<f32> = None;
    loop {
        app.update();
        client.pump();
        let proxies = client.server_proxies(server_id);
        if proxies.len() == 2 {
            match first_obs {
                None => first_obs = Some(proxies[0].x),
                Some(first) if proxies[0].x > first + 0.5 => break,
                _ => {}
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "client never converged against the headless app"
        );
        tokio::time::sleep(POLL).await;
    }

    // Phase 2: the fixed clock drives the sim — ≈64 FixedUpdate ticks per
    // wall-second (soft bounds; the manual update loop runs much faster than
    // 64/s, so Time<Fixed> must be doing the regulation).
    let t0 = app.world().resource::<TickCount>().0;
    let wall_start = Instant::now();
    while wall_start.elapsed() < Duration::from_secs(2) {
        // 2s window: halves variance vs 1s — Time<Virtual> max_delta (250ms) permanently drops ticks on stalls (auditor F3)
        app.update();
        client.pump();
        tokio::time::sleep(POLL).await;
    }
    let elapsed = wall_start.elapsed().as_secs_f64();
    let ticks = app.world().resource::<TickCount>().0 - t0;
    let rate = ticks as f64 / elapsed;
    assert!(
        (45.0..=90.0).contains(&rate),
        "FixedUpdate must self-regulate to ~64 Hz; measured {rate:.1} Hz ({ticks} ticks in {elapsed:.2}s)"
    );

    // Mode-3 shape: every simulated entity on the server is server-owned.
    let world = app.world_mut();
    let owners: Vec<PeerId> = world.query::<&Owner>().iter(world).map(|o| o.0).collect();
    assert_eq!(owners.len(), 2);
    assert!(owners.iter().all(|o| *o == server_id));
}

/// M4 — cadence decoupling: state messages arrive at the ~20 Hz network
/// interval, NOT the 64 Hz fixed tick (entities move every fixed tick, so an
/// undivided pump would send ~64/s).
#[tokio::test(flavor = "multi_thread")]
async fn network_cadence_decoupled_from_fixed_tick() {
    let room = start_signaling();
    let (mut app, server_id) = boot_server(&room, 1).await;
    let mut client = Client::connect(&room).await;

    // Wait until state is flowing at all.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while client.server_proxies(server_id).is_empty() {
        app.update();
        client.pump();
        assert!(
            tokio::time::Instant::now() < deadline,
            "no state ever flowed"
        );
        tokio::time::sleep(POLL).await;
    }

    // Measure received state messages over ~1 wall-second.
    client.received_state_msgs = 0;
    let wall_start = Instant::now();
    while wall_start.elapsed() < Duration::from_secs(2) {
        // 2s window: halves variance vs 1s — Time<Virtual> max_delta (250ms) permanently drops ticks on stalls (auditor F3)
        app.update();
        client.pump();
        tokio::time::sleep(POLL).await;
    }
    let elapsed = wall_start.elapsed().as_secs_f64();
    let rate = client.received_state_msgs as f64 / elapsed;
    let net_hz = 1.0 / NET_INTERVAL.as_secs_f64();
    assert!(
        rate >= net_hz * 0.5 && rate <= net_hz * 1.75,
        "state msgs must track the ~{net_hz:.0} Hz network interval, not the 64 Hz \
         fixed tick; measured {rate:.1}/s"
    );
    assert!(
        rate < 45.0,
        "state cadence must be decoupled from the 64 Hz fixed tick; measured {rate:.1}/s"
    );
}

/// Ack round-trip over the REAL pump (ADR-0020): a confirmed value goes quiet.
/// Both directions are driven:
///   • the client acks the server's stationary entity ⇒ the server's per-peer
///     delta baseline confirms ⇒ the server stops re-sending it; and
///   • the client OWNS a stationary entity it replicates to the server ⇒ the
///     server's ack-routing (`net_pump` `drain_acks` → protocol-id→transport-peer
///     → `send_event`, previously Mode-3-dead) confirms the client's baseline ⇒
///     the client stops re-sending it.
/// This is the integration coverage for the ack wiring the `two_world` unit tests
/// prove in isolation. It FAILS if either `drain_acks` send is removed: an
/// unconfirmed stationary value re-sends at the ~20 Hz net interval (~40 msgs in
/// the 2 s window), a confirmed one is ~0.
#[tokio::test(flavor = "multi_thread")]
async fn ack_round_trip_confirms_and_goes_quiet() {
    let room = start_signaling();
    // No moving demo entities: a moving value re-sends every tick regardless of
    // acks, masking the "goes quiet" signal. Spawn ONE stationary server entity
    // (after build — collect_all re-queries the world, so it is picked up).
    let (mut app, server_id) = boot_server(&room, 0).await;
    engine_core::spawn_owned(
        app.world_mut(),
        server_id,
        Position { x: 5.0, y: 5.0 },
        Velocity { x: 0.0, y: 0.0 },
    );
    let mut client = Client::connect(&room).await;
    let client_local = client.local;
    // The client OWNS a stationary entity → replicates it to the server, exercising
    // the server's ack-routing on the return path.
    client.spawn_owned_entity(Position { x: -3.0, y: 7.0 }, Velocity { x: 0.0, y: 0.0 });

    // Phase 1: converge — state has flowed BOTH ways (client holds the server's
    // proxy AND the server holds the client's proxy).
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        app.update();
        client.pump();
        let client_has_server = client.server_proxies(server_id).len() == 1;
        let world = app.world_mut();
        let server_has_client = world
            .query::<&Owner>()
            .iter(world)
            .any(|o| o.0 == client_local);
        if client_has_server && server_has_client {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "state never flowed both directions (client_has_server={client_has_server})"
        );
        tokio::time::sleep(POLL).await;
    }

    // Phase 2: settle — let both acks round-trip so both delta baselines confirm.
    let settle_end = Instant::now() + Duration::from_secs(1);
    while Instant::now() < settle_end {
        app.update();
        client.pump();
        tokio::time::sleep(POLL).await;
    }

    // Phase 3: observe — a CONFIRMED stationary value is not re-sent, either way.
    let recv_baseline = client.received_state_msgs;
    let sent_baseline = client.state_msgs_sent;
    let obs_end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < obs_end {
        app.update();
        client.pump();
        tokio::time::sleep(POLL).await;
    }
    let recv_delta = client.received_state_msgs - recv_baseline;
    let sent_delta = client.state_msgs_sent - sent_baseline;

    // Non-vacuity: a value can go quiet ONLY after state flowed on the counted
    // channel AND was acked (confirmation causality) — so a plateau at a POSITIVE
    // baseline is a confirmed value, not an absent stream. Make that explicit in
    // the test rather than leaning on the (Spawn-satisfiable) position sanity below
    // (auditor N2): both directions must have carried ≥1 state-channel message.
    assert!(
        recv_baseline > 0 && sent_baseline > 0,
        "state must have flowed on BOTH channels before it could go quiet \
         (recv={recv_baseline}, sent={sent_baseline})"
    );

    // The discriminator: unconfirmed ⇒ the value re-sends every net tick, confirmed
    // ⇒ ~0. The small margin absorbs an in-flight re-send racing the confirmation.
    assert!(
        recv_delta <= 2,
        "server's stationary entity must go quiet once the client acks it; got \
         {recv_delta} state msgs in 2s (unconfirmed ~40 at the 20 Hz net interval — \
         client→server ack broken)"
    );
    assert!(
        sent_delta <= 2,
        "client's stationary entity must go quiet once the server acks it; got \
         {sent_delta} state msgs in 2s (unconfirmed ~hundreds at poll rate — \
         net_pump ack-routing broken)"
    );

    // Sanity: the client actually converged (it holds the server's proxy at its
    // stationary position) — the quiet is a confirmed value, not an empty stream.
    let proxies = client.server_proxies(server_id);
    assert_eq!(proxies.len(), 1, "client must still hold the server proxy");
    assert!(
        (proxies[0].x - 5.0).abs() < 0.1 && (proxies[0].y - 5.0).abs() < 0.1,
        "server proxy must rest at its stationary position, got {:?}",
        proxies[0]
    );
}

/// A real per-client AOI focus over the pump (ADR-0023 c): a FOCUSED server gives
/// each client a server-owned avatar it controls and focuses that client's AOI on
/// it. The client sees only entities within its focus radius — an entity far
/// outside NEVER leaks (existence gating / read-cheat over the real pump).
#[tokio::test(flavor = "multi_thread")]
async fn focused_server_withholds_out_of_focus_entities() {
    let room = start_signaling();
    let (mut app, server_id) = boot_focused_server(&room, 10.0).await;
    // Stationary demo entities: one NEAR a lane-0 avatar's focus (origin), one
    // very FAR (outside every possible focus).
    engine_core::spawn_owned(
        app.world_mut(),
        server_id,
        Position { x: 2.0, y: 0.0 },
        Velocity { x: 0.0, y: 0.0 },
    );
    engine_core::spawn_owned(
        app.world_mut(),
        server_id,
        Position { x: 1.0e6, y: 0.0 },
        Velocity { x: 0.0, y: 0.0 },
    );
    let mut client = Client::connect(&room).await;

    // The single client → lane 0 → avatar at (0,0) → focus around the origin.
    // Converge to exactly its avatar + the near entity (both within radius 10).
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        app.update();
        client.pump();
        if client.server_proxies(server_id).len() == 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "client never converged to its focus set (has {})",
            client.server_proxies(server_id).len()
        );
        tokio::time::sleep(POLL).await;
    }

    // The far entity must NEVER leak across a settling window (existence gating).
    let obs_end = Instant::now() + Duration::from_secs(1);
    while Instant::now() < obs_end {
        app.update();
        client.pump();
        assert!(
            client
                .server_proxies(server_id)
                .iter()
                .all(|p| p.x < 1000.0),
            "an out-of-focus entity must NEVER leak (existence gating): {:?}",
            client.server_proxies(server_id)
        );
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        client.server_proxies(server_id).len(),
        2,
        "client holds exactly its avatar + the near entity"
    );
}

/// Two focused clients see DISJOINT sets (ADR-0023 c): each has its own avatar in
/// a disjoint focus, so the near entity reaches only the lane-0 client and the
/// far entity reaches neither. Asserts disjointness (not exact per-client
/// identity — lane assignment is nondeterministic and ControlledBy isn't on the
/// wire).
#[tokio::test(flavor = "multi_thread")]
async fn two_focused_clients_see_disjoint_sets() {
    let room = start_signaling();
    let (mut app, server_id) = boot_focused_server(&room, 10.0).await;
    engine_core::spawn_owned(
        app.world_mut(),
        server_id,
        Position { x: 2.0, y: 0.0 },
        Velocity { x: 0.0, y: 0.0 },
    );
    engine_core::spawn_owned(
        app.world_mut(),
        server_id,
        Position { x: 1.0e6, y: 0.0 },
        Velocity { x: 0.0, y: 0.0 },
    );
    let mut c1 = Client::connect(&room).await;
    let mut c2 = Client::connect(&room).await;

    // Converge: each client holds at least its avatar; the lane-0 client also
    // holds the near entity (so the total across both is ≥ 3).
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        app.update();
        c1.pump();
        c2.pump();
        let (n1, n2) = (
            c1.server_proxies(server_id).len(),
            c2.server_proxies(server_id).len(),
        );
        if n1 >= 1 && n2 >= 1 && n1 + n2 >= 3 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "clients never converged to their foci (c1={n1}, c2={n2})"
        );
        tokio::time::sleep(POLL).await;
    }

    // Settle.
    let settle = Instant::now() + Duration::from_secs(1);
    while Instant::now() < settle {
        app.update();
        c1.pump();
        c2.pump();
        tokio::time::sleep(POLL).await;
    }

    let p1 = c1.server_proxies(server_id);
    let p2 = c2.server_proxies(server_id);
    // Neither client ever holds the far entity (read-cheat / existence gating).
    assert!(
        p1.iter().all(|p| p.x < 1000.0) && p2.iter().all(|p| p.x < 1000.0),
        "no client may see the out-of-focus far entity: {p1:?} / {p2:?}"
    );
    // The two focus sets are DISJOINT (distinct avatars in disjoint foci; the
    // near entity only in the lane-0 client's set). Compare by rounded position.
    let round = |ps: &[Position]| -> HashSet<(i64, i64)> {
        ps.iter()
            .map(|p| (p.x.round() as i64, p.y.round() as i64))
            .collect()
    };
    let (s1, s2) = (round(&p1), round(&p2));
    // Non-vacuity: disjointness is trivially true for empty sets — assert both
    // clients actually hold their (stationary, in-focus) avatars at assertion
    // time, so the disjoint check is meaningful (auditor F2).
    assert!(
        !s1.is_empty() && !s2.is_empty(),
        "both clients must hold their focus set: {s1:?} / {s2:?}"
    );
    assert!(
        s1.is_disjoint(&s2),
        "per-client foci must be disjoint: {s1:?} vs {s2:?}"
    );
}

/// Anti-entropy resync heals an injected desync over the REAL pump (ADR-0024):
/// a stationary server entity is confirmed+quiet (so the delta stream is silent
/// and the server's Digest carries a state-hash), then the client's proxy value
/// is CORRUPTED — only the digest → request → ResyncSpawn round can restore it.
/// The over-transport lift of the two_world R6-5 hash-mismatch unit test.
#[tokio::test(flavor = "multi_thread")]
async fn resync_heals_injected_desync_over_pump() {
    let room = start_signaling();
    let (mut app, server_id) = boot_server(&room, 0).await;
    engine_core::spawn_owned(
        app.world_mut(),
        server_id,
        Position { x: 5.0, y: 5.0 },
        Velocity { x: 0.0, y: 0.0 },
    );
    let mut client = Client::connect(&room).await;

    // Phase 1: converge to the (single) stationary proxy.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        app.update();
        client.pump();
        if client.server_proxies(server_id).len() == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "client never converged"
        );
        tokio::time::sleep(POLL).await;
    }

    // Phase 2: SETTLE TO QUIET (load-bearing). The value must be confirmed (client
    // acked) + unchanged so (a) the delta stream is silent — it can't mask the
    // injected divergence — and (b) the server's Digest carries a state-hash (a
    // pure value divergence is invisible without one). Settle FIRST (let the ack
    // land + the server confirm), THEN measure quiescence over a fresh window —
    // capturing the baseline POST-settle, not right after converge when the value
    // may still be unconfirmed (auditor N1).
    let settle_end = Instant::now() + Duration::from_secs(1);
    while Instant::now() < settle_end {
        app.update();
        client.pump();
        tokio::time::sleep(POLL).await;
    }
    let recv_baseline = client.received_state_msgs;
    let observe_end = Instant::now() + Duration::from_secs(1);
    while Instant::now() < observe_end {
        app.update();
        client.pump();
        tokio::time::sleep(POLL).await;
    }
    assert!(
        client.received_state_msgs - recv_baseline <= 2,
        "the server must be confirmed-quiet before the injection (got {} state msgs)",
        client.received_state_msgs - recv_baseline
    );

    // Phase 3: INJECT a silent divergence into the client's Remote proxy — the
    // delta stream cannot heal it (server quiet; a client never sends state for a
    // Remote proxy), so RESYNC is the ONLY possible heal path (non-vacuous).
    let proxy = client.server_proxy_entity(server_id);
    client.world.get_mut::<Position>(proxy).unwrap().x = 999.0;
    assert!(
        (client.server_proxies(server_id)[0].x - 999.0).abs() < 0.01,
        "divergence injected"
    );

    // Phase 4: HEAL — pump until the digest → request → ResyncSpawn round restores
    // the authoritative value.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        app.update();
        client.pump();
        if (client.server_proxies(server_id)[0].x - 5.0).abs() < 0.1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "resync never healed the injected desync (proxy x = {})",
            client.server_proxies(server_id)[0].x
        );
        tokio::time::sleep(POLL).await;
    }

    // Phase 5: STABILITY — the heal holds (idempotent; no re-divergence).
    let stable_end = Instant::now() + Duration::from_secs(1);
    while Instant::now() < stable_end {
        app.update();
        client.pump();
        assert!(
            (client.server_proxies(server_id)[0].x - 5.0).abs() < 0.1,
            "the healed value must hold"
        );
        tokio::time::sleep(POLL).await;
    }
}

// ═══ ADR-0028 (Stage 1) — claim + reassignment driven by the real net_pump ═══

/// WIRE-claim — the pump drives a CLAIM end-to-end. A client claims a
/// server-owned entity; whichever peer is the coordinator arbitrates via the
/// wired `drain_commits`, and (sole claimant) the entity converges to the CLIENT
/// on BOTH sides — proving claim/commit flows through the real pump.
#[tokio::test(flavor = "multi_thread")]
async fn pump_drives_claim_end_to_end() {
    let room = start_signaling();
    let (mut app, server_id) = boot_server(&room, 1).await;
    let mut client = Client::connect(&room).await;

    // Converge: the client holds the server's one entity as a proxy.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        app.update();
        client.pump();
        if client.server_proxies(server_id).len() == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "client never got the server proxy"
        );
        tokio::time::sleep(POLL).await;
    }

    // The client claims the server's entity (routes to the coordinator).
    let proxy = client.server_proxy_entity(server_id);
    let client_id = client.local;
    client.claim(proxy);

    // Pump both sides until the claim COMMITS: the sole claimant (the client) wins,
    // so the entity is client-owned on the client AND on the server.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        app.update();
        client.pump();
        let client_owns = client.owner_of(proxy) == Some(client_id);
        let world = app.world_mut();
        let server_owners: Vec<PeerId> = world.query::<&Owner>().iter(world).map(|o| o.0).collect();
        let server_agrees =
            server_owners.len() == 1 && server_owners.iter().all(|o| *o == client_id);
        if client_owns && server_agrees {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "claim never committed through the pump"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// WIRE-reassign — a departed OWNER's entity is reassigned by the pump. A client
/// owns a Mode-2-shaped entity (the server holds a proxy); the client leaves; the
/// server's Disconnected arm calls `reassign_orphans`, so the orphan is re-tagged
/// to the surviving owner (the server) — never frozen at the dead client.
#[tokio::test(flavor = "multi_thread")]
async fn pump_reassigns_departed_owners_entity() {
    let room = start_signaling();
    let (mut app, server_id) = boot_server(&room, 0).await; // server owns nothing
    let mut client = Client::connect(&room).await;
    let client_id = client.local;
    client.spawn_owned_entity(Position { x: 5.0, y: 0.0 }, Velocity { x: 0.0, y: 0.0 });

    // Converge: the server holds the client's entity as a proxy (owned by client).
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        app.update();
        client.pump();
        let world = app.world_mut();
        let has_client_proxy = world
            .query::<&Owner>()
            .iter(world)
            .any(|o| o.0 == client_id);
        if has_client_proxy {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server never got the client's proxy"
        );
        tokio::time::sleep(POLL).await;
    }

    // The client leaves the session.
    client.close();
    drop(client);

    // Pump the server until it observes the disconnect and reassigns the orphan:
    // the (formerly client-owned) entity is now server-owned, none left orphaned.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        app.update();
        let world = app.world_mut();
        let owners: Vec<PeerId> = world.query::<&Owner>().iter(world).map(|o| o.0).collect();
        let none_orphaned = !owners.contains(&client_id);
        let reassigned = !owners.is_empty() && owners.iter().all(|o| *o == server_id);
        if none_orphaned && reassigned {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server never reassigned the departed owner's entity (owners={owners:?})"
        );
        tokio::time::sleep(POLL).await;
    }
}
