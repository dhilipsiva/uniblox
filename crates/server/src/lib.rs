//! `server` — the Mode-3 headless authoritative runtime (ADR-0014).
//!
//! Runs the IDENTICAL simulation as every other mode — `engine_core::simulate`
//! and `replication::Replication`, unmodified. Mode 3 is expressed purely as
//! data: this process spawns (and therefore owns) ALL gameplay entities, so
//! `authority_of` returns `Local` for everything here and `Remote` for
//! everything on clients. **There is no mode branch anywhere** — that is the
//! authority-swap thesis, proven by the M1–M4 battery.
//!
//! App shape (standalone `bevy_app` + `bevy_time`; the `MinimalPlugins`
//! equivalent — the umbrella `bevy` crate is NOT a dependency):
//! - `FixedUpdate` at 64 Hz (`Time<Fixed>`): `sync_sim_dt` → `count_tick` →
//!   `simulate`, chained. `TimePlugin` is mandatory — without it FixedUpdate
//!   silently never runs.
//! - `Update`: the exclusive `net_pump` — receive continuously (low latency),
//!   but COLLECT + SEND only on a virtual-clock accumulator at [`NET_INTERVAL`]
//!   (~20 Hz, the assumed-not-measured network tick; the Instrumentation item
//!   measures it). `Time<Virtual>` is max_delta-clamped (250 ms) — stalls drop
//!   sends rather than bursting them. Network send timing is deliberately NOT
//!   the fixed tick: fixed-timestep is not wall-clock.
//! - Exit: 0.19 buffered events are Messages — `AppExit` is written to
//!   `Messages<AppExit>` (never `EventWriter`).

use std::time::Duration;

use bevy_app::{App, AppExit, FixedUpdate, ScheduleRunnerPlugin, TaskPoolPlugin, Update};
use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use bevy_time::{Fixed, Time, TimePlugin, Virtual};
use engine_core::{
    ControlledBy, LocalPeer, PendingInputs, Position, ProcessedInput, SimDt, Velocity,
    advance_tick, apply_input, insert_sim, resolve_interactions, simulate, spawn_owned,
};
use protocol::PeerId;
use replication::Replication;
use transport::{PeerState, Transport};

/// The fixed simulation tick rate (also `Time<Fixed>`'s default; set
/// explicitly for intent).
pub const TICK_HZ: f64 = 64.0;

/// The network send interval (~20 Hz). An ASSUMED starting point per the
/// open-questions register — measured, not locked, in the Instrumentation item.
pub const NET_INTERVAL: Duration = Duration::from_millis(50);

/// The anti-entropy resync DIGEST interval (ADR-0024) — a SLOW background cadence
/// (10× the net tick), decoupled from it. Bounds steady-state digest chatter; the
/// heal latency of a silent divergence is ≈ one interval + ~1 RTT, which is fine
/// for a repair the delta stream provably cannot make. Requests/responses fire
/// promptly (every frame), rate-limited upstream by this cadence.
pub const RESYNC_INTERVAL: Duration = Duration::from_millis(500);

/// FixedUpdate tick counter — the 64 Hz evidence for tests and, later, the
/// instrumentation table.
#[derive(Resource, Default)]
pub struct TickCount(pub u64);

/// AOI focus (ADR-0023 c): the exit radius is this multiple of the focus (enter)
/// radius — a hysteresis band so an entity crossing a client's focus edge does
/// not flicker Spawn/Despawn.
const FOCUS_HYSTERESIS: f32 = 1.25;
/// Per-connection avatar lane spacing, as a multiple of the focus radius. Must
/// exceed `2 * FOCUS_HYSTERESIS` so per-client foci are disjoint.
const FOCUS_LANE_FACTOR: f32 = 4.0;

/// The networking bundle. NOT Send/Sync-bound: stored as a NonSend resource
/// and taken/reinserted around the exclusive pump (there is no
/// `non_send_resource_scope` helper in 0.19).
pub struct Net {
    transport: Transport,
    repl: Replication,
    acc: Duration,
    /// Virtual-clock accumulator for the SLOW resync digest cadence (ADR-0024),
    /// separate from `acc` (the net tick).
    resync_acc: Duration,
    /// Focused mode (ADR-0023 c): when `Some(r)`, each connecting client gets a
    /// server-owned avatar it controls and an AOI focused on it (enter radius
    /// `r`). `None` ⇒ the unbounded Mode-3 default (every client sees all).
    focus_radius: Option<f32>,
    /// Monotonic per-connection lane for placing avatars at disjoint positions.
    avatar_lane: u64,
}

/// Route a batch of DIRECTED events `(target protocol id, encoded bytes)` on the
/// reliable channel: map each protocol id back to its transport peer and
/// `send_event`. The shared send path for acks + resync (ADR-0024).
fn send_directed(transport: &mut Transport, msgs: Vec<(PeerId, Box<[u8]>)>) {
    if msgs.is_empty() {
        return;
    }
    let connected: Vec<_> = transport.connected_peers().collect();
    for (target, bytes) in msgs {
        if let Some(peer) = connected
            .iter()
            .find(|p| PeerId::from_uuid_bytes(*p.0.as_bytes()) == target)
        {
            let _ = transport.send_event(*peer, bytes);
        }
    }
}

/// Feed engine-core's `SimDt` contract from the fixed clock. Inside
/// `FixedUpdate`, `Res<Time>` yields `Time<Fixed>`'s delta.
fn sync_sim_dt(time: Res<Time>, mut dt: ResMut<SimDt>) {
    dt.0 = time.delta_secs();
}

fn count_tick(mut count: ResMut<TickCount>) {
    count.0 += 1;
}

/// The exclusive network pump (Update). Receives every frame; collects and
/// broadcasts only when the wall-clock accumulator crosses [`NET_INTERVAL`].
pub fn net_pump(world: &mut World) {
    let dt = world.resource::<Time<Virtual>>().delta();
    let Some(mut net) = world.remove_non_send::<Net>() else {
        return;
    };

    // Peer bookkeeping + late-join replay, every frame.
    match net.transport.poll_peers() {
        Ok(peers) => {
            for (peer, state) in peers {
                let proto = PeerId::from_uuid_bytes(*peer.0.as_bytes());
                match state {
                    PeerState::Connected => {
                        log::info!("[server] peer connected: {proto:?}");
                        // Track the peer (ADR-0020/0021). Its entities are
                        // announced per-peer by the next collect_all via
                        // AOI-ENTER — no blanket replay (existence is now gated).
                        net.repl.on_peer_connected(proto);
                        // Focused mode (ADR-0023 c): give the client a
                        // server-OWNED avatar it CONTROLS (Owner=server keeps Mode 3
                        // authoritative; ControlledBy=client is the input/focus
                        // link). A distinct lane per connection keeps foci disjoint.
                        // Its AOI is focused on it each net tick, below.
                        if let Some(radius) = net.focus_radius {
                            let local = world.resource::<LocalPeer>().0;
                            let lane = net.avatar_lane;
                            net.avatar_lane += 1;
                            let avatar = spawn_owned(
                                world,
                                local,
                                Position {
                                    x: radius * FOCUS_LANE_FACTOR * lane as f32,
                                    y: 0.0,
                                },
                                Velocity { x: 0.0, y: 0.0 },
                            );
                            world.entity_mut(avatar).insert(ControlledBy(proto));
                        }
                    }
                    PeerState::Disconnected => {
                        log::info!("[server] peer disconnected: {proto:?}");
                        net.repl.untrack_peer(proto);
                        // Reset the processed-input high-water (ADR-0022) so a
                        // reconnect with a fresh input-seq namespace isn't frozen
                        // by a stale marker (auditor F4).
                        if let Some(mut processed) = world.get_resource_mut::<ProcessedInput>() {
                            processed.0.remove(&proto);
                        }
                        // Despawn the client's avatar (ADR-0023 c) via the
                        // ControlledBy scan, and PRUNE its PendingInputs — that map
                        // is not pruned anywhere else, so without this every
                        // reconnect would leak a queue. The next collect_all's
                        // `dead` set retires the map entry (no Despawn is wired —
                        // no other peer had the avatar in focus).
                        let avatar = world
                            .query::<(Entity, &ControlledBy)>()
                            .iter(world)
                            .find(|(_, cb)| cb.0 == proto)
                            .map(|(e, _)| e);
                        if let Some(avatar) = avatar {
                            if let Some(mut pending) = world.get_resource_mut::<PendingInputs>() {
                                pending.0.remove(&avatar);
                            }
                            world.despawn(avatar);
                        }
                    }
                }
            }
        }
        Err(err) => {
            log::error!("[server] transport closed ({err}) — exiting");
            if let Some(mut messages) = world.get_resource_mut::<Messages<AppExit>>() {
                messages.write(AppExit::error());
            }
            world.insert_non_send(net);
            return;
        }
    }

    // Receive continuously (events before state) — freshness costs nothing.
    for (from, bytes) in net.transport.recv_events() {
        let from = PeerId::from_uuid_bytes(*from.0.as_bytes());
        net.repl.apply_events(world, from, &bytes);
    }
    for (from, bytes) in net.transport.recv_state() {
        let from = PeerId::from_uuid_bytes(*from.0.as_bytes());
        net.repl.apply_state(world, from, &bytes);
    }

    // Emit acks for state we received (ADR-0020) so senders advance their delta
    // baselines. Directed back to each sender (map protocol id → transport peer).
    let acks = net.repl.drain_acks();
    send_directed(&mut net.transport, acks);

    // Anti-entropy resync (ADR-0024), PROMPT (every frame — one-shot, already
    // rate-limited upstream by the slow digest cadence): a divergence detected
    // from a received digest requests the fix next frame; a received request is
    // answered with a ResyncSpawn next frame. Empty (no-op) when nothing diverges.
    let reqs = net.repl.drain_resync_requests();
    send_directed(&mut net.transport, reqs);
    let resp = net.repl.drain_resync_responses(world);
    send_directed(&mut net.transport, resp);

    // Send on the network tick only.
    net.acc += dt;
    if net.acc >= NET_INTERVAL {
        net.acc -= NET_INTERVAL;
        // Don't let a long stall burst multiple sends.
        if net.acc >= NET_INTERVAL {
            net.acc = Duration::ZERO;
        }
        // Focus each client's AOI on the entity it controls (ADR-0023 c). Gather
        // the (peer, avatar-position) pairs first (world read), then set each AOI,
        // then collect — set_aoi_* MUST precede collect_all (it reads self.aoi).
        // Unfocused mode leaves AOI unset ⇒ every client sees all (the Mode-3
        // fail-open default). A moving avatar's focus follows it each net tick.
        if let Some(radius) = net.focus_radius {
            let focuses: Vec<(PeerId, (f32, f32))> = world
                .query::<(&ControlledBy, &Position)>()
                .iter(world)
                .map(|(cb, pos)| (cb.0, (pos.x, pos.y)))
                .collect();
            for (peer, center) in focuses {
                net.repl
                    .set_aoi_hysteresis(peer, center, radius, radius * FOCUS_HYSTERESIS);
            }
        }
        // Per-peer collect (ADR-0021 interest management): each peer gets its own
        // AOI-gated outbox (focused above, or unbounded in the demo default).
        // Map each protocol peer back to its transport peer for the send.
        let connected: Vec<_> = net.transport.connected_peers().collect();
        for (target, out) in net.repl.collect_all(world) {
            let Some(peer) = connected
                .iter()
                .find(|p| PeerId::from_uuid_bytes(*p.0.as_bytes()) == target)
            else {
                continue;
            };
            if let Some(state) = out.state {
                let _ = net.transport.send_state(*peer, state);
            }
            for ev in out.events {
                let _ = net.transport.send_event(*peer, ev);
            }
        }
    }

    // Anti-entropy resync DIGEST on the SLOW cadence (ADR-0024), decoupled from
    // the net tick. Each connected peer gets a per-peer summary of the entities
    // it owns; the receiver detects a diverged proxy and pulls a fix (the prompt
    // request/response above). `Time<Virtual>` clamps `dt` at 250 ms < 500 ms, so
    // a stall fires at most one digest — the anti-burst clamp is belt-and-braces.
    net.resync_acc += dt;
    if net.resync_acc >= RESYNC_INTERVAL {
        net.resync_acc -= RESYNC_INTERVAL;
        if net.resync_acc >= RESYNC_INTERVAL {
            net.resync_acc = Duration::ZERO;
        }
        let digests = net.repl.collect_resync(world);
        send_directed(&mut net.transport, digests);
    }

    world.insert_non_send(net);
}

/// Assemble the headless authoritative server App around an already-connected
/// transport (its signaling id already known → `local`). Spawns `entity_count`
/// server-owned demo entities — the ONLY mode-defining input: the server owns
/// everything, so every client applies everything. Every client sees ALL demo
/// entities (unbounded AOI — the Mode-3 fail-open default).
pub fn build_server_app(transport: Transport, local: PeerId, entity_count: usize) -> App {
    build_server_app_inner(transport, local, entity_count, None)
}

/// Like [`build_server_app`], but FOCUSED (ADR-0023 c): each connecting client
/// gets a server-owned avatar it controls, and its AOI is focused on that avatar
/// with enter radius `focus_radius` — so a client sees only entities within its
/// focus (out-of-focus entities are withheld in state AND existence).
pub fn build_server_app_focused(
    transport: Transport,
    local: PeerId,
    entity_count: usize,
    focus_radius: f32,
) -> App {
    build_server_app_inner(transport, local, entity_count, Some(focus_radius))
}

fn build_server_app_inner(
    transport: Transport,
    local: PeerId,
    entity_count: usize,
    focus_radius: Option<f32>,
) -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        TimePlugin,
        ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / TICK_HZ)),
    ));
    app.insert_resource(Time::<Fixed>::from_hz(TICK_HZ));
    app.init_resource::<TickCount>();

    let world = app.world_mut();
    insert_sim(world, local, (1.0 / TICK_HZ) as f32);
    let repl = Replication::new(world);
    // Every demo entity advances in +x — the M3/M4 test predicates observe
    // proxies in replay order (arbitrary), so ALL entities must move on x.
    // Keep nonzero vel.x if you change these (auditor finding F2).
    for i in 0..entity_count {
        spawn_owned(
            world,
            local,
            Position {
                x: 0.0,
                y: 2.0 * i as f32,
            },
            Velocity {
                x: 2.0,
                y: 0.5 * i as f32,
            },
        );
    }

    app.insert_non_send(Net {
        transport,
        repl,
        acc: Duration::ZERO,
        resync_acc: Duration::ZERO,
        focus_radius,
        avatar_lane: 0,
    });
    // `advance_tick` bumps engine_core::Tick (the snapshot time axis, ADR-0022);
    // `count_tick` keeps the server-local TickCount (the 64 Hz evidence test).
    // `apply_input` (ADR-0022 Stage B) drains ONE client input per controlled
    // entity BEFORE simulate, so simulate integrates exactly one dt step per
    // input (the alignment reconciliation depends on).
    app.add_systems(
        FixedUpdate,
        (
            sync_sim_dt,
            count_tick,
            advance_tick,
            apply_input,
            simulate,
            // Coarse cross-owner interactions (ADR-0027) run AFTER simulate, on the
            // final positions. A no-op until entities carry Interactable+Contacts;
            // in Mode 3 the server owns all, so it decides every interaction.
            resolve_interactions,
        )
            .chain(),
    );
    app.add_systems(Update, net_pump);
    app
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert_eq!(2 + 2, 4);
    }
}
