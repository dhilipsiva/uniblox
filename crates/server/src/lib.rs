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
    Position, ProcessedInput, SimDt, Velocity, advance_tick, apply_input, insert_sim, simulate,
    spawn_owned,
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

/// FixedUpdate tick counter — the 64 Hz evidence for tests and, later, the
/// instrumentation table.
#[derive(Resource, Default)]
pub struct TickCount(pub u64);

/// The networking bundle. NOT Send/Sync-bound: stored as a NonSend resource
/// and taken/reinserted around the exclusive pump (there is no
/// `non_send_resource_scope` helper in 0.19).
pub struct Net {
    transport: Transport,
    repl: Replication,
    acc: Duration,
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
                    }
                    PeerState::Disconnected => {
                        log::info!("[server] peer disconnected: {proto:?}");
                        net.repl.untrack_peer(proto);
                        // Reset the processed-input high-water (ADR-0022) so a
                        // reconnect with a fresh input-seq namespace isn't frozen
                        // by a stale marker (auditor F4). (The avatar's
                        // PendingInputs entry is cleared when it despawns.)
                        if let Some(mut processed) = world.get_resource_mut::<ProcessedInput>() {
                            processed.0.remove(&proto);
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
    if !acks.is_empty() {
        let connected: Vec<_> = net.transport.connected_peers().collect();
        for (target, ack) in acks {
            if let Some(peer) = connected
                .iter()
                .find(|p| PeerId::from_uuid_bytes(*p.0.as_bytes()) == target)
            {
                let _ = net.transport.send_event(*peer, ack);
            }
        }
    }

    // Send on the network tick only.
    net.acc += dt;
    if net.acc >= NET_INTERVAL {
        net.acc -= NET_INTERVAL;
        // Don't let a long stall burst multiple sends.
        if net.acc >= NET_INTERVAL {
            net.acc = Duration::ZERO;
        }
        // Per-peer collect (ADR-0021 interest management): each peer gets its
        // own AOI-gated outbox. The server leaves AOI unset (every client sees
        // all demo entities); a per-client gameplay focus is future client work.
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

    world.insert_non_send(net);
}

/// Assemble the headless authoritative server App around an already-connected
/// transport (its signaling id already known → `local`). Spawns `entity_count`
/// server-owned demo entities — the ONLY mode-defining input: the server owns
/// everything, so every client applies everything.
pub fn build_server_app(transport: Transport, local: PeerId, entity_count: usize) -> App {
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
    });
    // `advance_tick` bumps engine_core::Tick (the snapshot time axis, ADR-0022);
    // `count_tick` keeps the server-local TickCount (the 64 Hz evidence test).
    // `apply_input` (ADR-0022 Stage B) drains ONE client input per controlled
    // entity BEFORE simulate, so simulate integrates exactly one dt step per
    // input (the alignment reconciliation depends on).
    app.add_systems(
        FixedUpdate,
        (sync_sim_dt, count_tick, advance_tick, apply_input, simulate).chain(),
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
