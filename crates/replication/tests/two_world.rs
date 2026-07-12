//! Tier B — deterministic per-peer replication tests (AOI-aware, ADR-0021).
//! Bytes are hand-ferried between worlds; reordering, loss, visibility, and
//! ownership are expressed by delivery order — no transport, fully deterministic.
//!
//! `collect_all` is the per-peer sender (ADR-0021): each tracked peer gets its
//! own AOI-gated outbox, delta baseline, and seq stream. Tests drive ONE
//! `collect_all` per tick and index the per-peer `Outbox` (via `collect_for` for
//! a single receiver, or `collect_all` + `outbox_for` for several). The RECEIVER
//! is unchanged from the ADR-0020 delta slice.

use bevy_ecs::prelude::*;
use engine_core::{
    Controlled, ControlledBy, INTERP_DELAY_TICKS, InputHistory, Intent, InterpBuffer, Owner,
    Position, RenderPos, RenderTick, Tick, Velocity, apply_input, copy_owned_render, insert_sim,
    interpolate, predict, record_input, simulate, spawn_owned,
};
use protocol::{
    EventMsg, NetEntityId, NetEvent, PeerId, StateEntry, WIRE_VERSION, decode_event, decode_state,
    encode_event,
};
use replication::{Outbox, Replication};

const DT: f32 = 0.5;
/// Quantization tolerance: 1/2048.
const TOL: f32 = 0.5 / 1024.0;

struct TestPeer {
    id: PeerId,
    world: World,
    schedule: Schedule,
    render: Schedule,
    repl: Replication,
}

impl TestPeer {
    fn new(id: u64) -> Self {
        let mut world = World::new();
        insert_sim(&mut world, PeerId(id), DT);
        // Sim: apply_input (server, ADR-0022 — no-op without ControlledBy) then
        // simulate. One input per tick BEFORE integration.
        let mut schedule = Schedule::default();
        schedule.add_systems((apply_input, simulate).chain());
        // The render step (ADR-0022): interpolated remotes lerp their buffer,
        // then predicted (controlled) entities replay their input history, then
        // owned entities copy Position — all write only RenderPos; ordered so
        // predict wins over a stale InterpBuffer and Local authority wins last.
        let mut render = Schedule::default();
        render.add_systems((interpolate, predict, copy_owned_render).chain());
        let repl = Replication::new(&mut world);
        TestPeer {
            id: PeerId(id),
            world,
            schedule,
            render,
            repl,
        }
    }

    fn spawn(&mut self, x: f32, y: f32, vx: f32, vy: f32) -> Entity {
        spawn_owned(
            &mut self.world,
            self.id,
            Position { x, y },
            Velocity { x: vx, y: vy },
        )
    }

    fn sim_tick(&mut self) {
        self.schedule.run(&mut self.world);
    }

    /// Set the authoritative sim tick stamped into this peer's outgoing
    /// snapshots (the interpolation time axis).
    fn set_tick(&mut self, t: u64) {
        self.world.insert_resource(Tick(t));
    }

    /// Set the interpolation render clock (sim-tick units; virtual — no wall
    /// clock), then run the render step.
    fn set_render_tick(&mut self, t: f64) {
        self.world.insert_resource(RenderTick(t));
    }

    /// Run one render step (copy-owned + interpolate) — updates `RenderPos`.
    fn run_render(&mut self) {
        self.render.run(&mut self.world);
    }

    fn render_pos(&self, e: Entity) -> RenderPos {
        *self.world.get::<RenderPos>(e).unwrap()
    }

    /// CLIENT: designate this proxy as the locally-controlled avatar (predicted
    /// role) — add `Controlled` + `InputHistory`, drop the interpolation buffer.
    fn set_controlled(&mut self, e: Entity) {
        self.world
            .entity_mut(e)
            .insert((Controlled { next_seq: 1 }, InputHistory::default()))
            .remove::<InterpBuffer>();
    }

    /// SERVER: mark this entity as driven by `peer` (the authority applies that
    /// peer's inputs to it).
    fn set_controlled_by(&mut self, e: Entity, peer: PeerId) {
        self.world.entity_mut(e).insert(ControlledBy(peer));
    }

    /// CLIENT: record one input (desired velocity) on a controlled entity.
    fn feed_input(&mut self, e: Entity, vx: f32, vy: f32) {
        record_input(&mut self.world, e, Intent { vx, vy });
    }

    /// Read the velocity of an entity (for the "prediction touches no
    /// authoritative Velocity" assertion).
    fn vel(&self, e: Entity) -> Velocity {
        *self.world.get::<Velocity>(e).unwrap()
    }

    /// Track a destination peer so `collect_all` produces its outbox.
    fn track(&mut self, peer: PeerId) {
        self.repl.track_peer(peer);
    }

    /// Set a peer's area of interest (center + radius).
    fn set_aoi(&mut self, peer: PeerId, center: (f32, f32), radius: f32) {
        self.repl.set_aoi(peer, center, radius);
    }

    /// The per-peer collect for the whole tracked set — drive ONCE per tick.
    fn collect_all(&mut self) -> Vec<(PeerId, Outbox)> {
        self.repl.collect_all(&mut self.world)
    }

    /// Convenience: collect for ONE destination, auto-tracking it. Drives
    /// `collect_all` once and returns that peer's outbox (empty if it has none).
    /// For multi-peer ticks use `collect_all` + `outbox_for` (one snapshot).
    fn collect_for(&mut self, dest: PeerId) -> Outbox {
        self.repl.track_peer(dest);
        self.repl
            .collect_all(&mut self.world)
            .into_iter()
            .find(|(p, _)| *p == dest)
            .map(|(_, o)| o)
            .unwrap_or(Outbox {
                state: None,
                events: Vec::new(),
            })
    }

    fn deliver_state(&mut self, from: PeerId, bytes: &[u8]) {
        self.repl.apply_state(&mut self.world, from, bytes);
    }

    fn deliver_events(&mut self, from: PeerId, events: &[Box<[u8]>]) {
        for ev in events {
            self.repl.apply_events(&mut self.world, from, ev);
        }
    }

    /// Deliver a whole outbox (events first — spawn-before-state is the common
    /// case; tests that need the adverse order do it by hand).
    fn deliver_all(&mut self, from: PeerId, outbox: &Outbox) {
        self.deliver_events(from, &outbox.events);
        if let Some(state) = &outbox.state {
            self.deliver_state(from, state);
        }
    }

    fn pos(&self, e: Entity) -> Position {
        *self.world.get::<Position>(e).unwrap()
    }

    fn owner(&self, e: Entity) -> PeerId {
        self.world.get::<Owner>(e).unwrap().0
    }

    fn entity_count(&mut self) -> usize {
        self.world.query::<&Position>().iter(&self.world).count()
    }

    /// The single entity owned by `owner` (panics if not exactly one).
    fn entity_owned_by(&mut self, owner: PeerId) -> Entity {
        let found: Vec<Entity> = self
            .world
            .query::<(Entity, &Owner)>()
            .iter(&self.world)
            .filter(|(_, o)| o.0 == owner)
            .map(|(e, _)| e)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "expected exactly one entity owned by {owner:?}"
        );
        found[0]
    }
}

/// Flush `receiver`'s acks back to `sender` (ADR-0020): the receiver acks the
/// sender's stream; the sender records the confirmed baseline.
fn flush_acks(receiver: &mut TestPeer, sender: &mut TestPeer) {
    let acks = receiver.repl.drain_acks();
    for (target, ack) in acks {
        assert_eq!(target, sender.id, "an ack targets the acked sender");
        sender
            .repl
            .apply_events(&mut sender.world, receiver.id, &ack);
    }
}

/// The outbox `collect_all` produced for peer `p`, if any.
fn outbox_for(outs: &[(PeerId, Outbox)], p: PeerId) -> Option<&Outbox> {
    outs.iter().find(|(peer, _)| *peer == p).map(|(_, o)| o)
}

/// Drain `client`'s inputs and deliver them to the avatar's authority `server`
/// (ADR-0022 Stage B) — the input-flow analogue of `flush_acks`.
fn flush_inputs(client: &mut TestPeer, server: &mut TestPeer) {
    let inputs = client.repl.drain_inputs(&mut client.world);
    for (target, bytes) in inputs {
        assert_eq!(target, server.id, "an input targets the avatar's authority");
        server
            .repl
            .apply_events(&mut server.world, client.id, &bytes);
    }
}

/// Stand up a Mode-3-style controlled avatar: `s` (server) owns E at (x,y) and
/// marks it `ControlledBy(c)`; `c` (client) builds a proxy for E and designates
/// it its controlled/predicted avatar. Returns (E on server, proxy on client).
fn controlled_avatar(s: &mut TestPeer, c: &mut TestPeer, x: f32, y: f32) -> (Entity, Entity) {
    let e = s.spawn(x, y, 0.0, 0.0);
    s.set_controlled_by(e, c.id);
    c.deliver_all(s.id, &s.collect_for(c.id)); // C builds the proxy
    let proxy = c.entity_owned_by(s.id);
    c.set_controlled(proxy);
    (e, proxy)
}

fn state_entries(outbox: &Outbox) -> Vec<StateEntry> {
    outbox
        .state
        .as_ref()
        .map(|b| decode_state(b).unwrap().entries)
        .unwrap_or_default()
}

fn spawn_ids(events: &[Box<[u8]>]) -> Vec<NetEntityId> {
    events
        .iter()
        .filter_map(|b| match decode_event(b).unwrap().event {
            NetEvent::Spawn { id, .. } => Some(id),
            _ => None,
        })
        .collect()
}

fn despawn_count(outbox: &Outbox) -> usize {
    outbox
        .events
        .iter()
        .filter(|ev| matches!(decode_event(ev).unwrap().event, NetEvent::Despawn { .. }))
        .count()
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() <= TOL
}

/// The `NetEntityId` a peer mints for a local entity (mirrors the sender's
/// minting rule) — lets a test match a specific entity's state entries.
fn net_id(spawner: PeerId, e: Entity) -> NetEntityId {
    NetEntityId {
        spawner,
        index: e.index_u32(),
        generation: e.generation().to_bits(),
    }
}

/// The seq of a state message (panics if the outbox carried none).
fn state_seq(o: &Outbox) -> u64 {
    decode_state(o.state.as_ref().expect("outbox has state"))
        .expect("decode")
        .seq
}

/// Inject a hand-built `Ack{seq}` from `from` into `target`'s sender state —
/// white-box control for verifying the confirmation logic (e.g. a stale ack of
/// a seq the peer could not actually have applied for a given component).
fn inject_ack(target: &mut TestPeer, from: PeerId, seq: u64) {
    let bytes = encode_event(&EventMsg {
        version: WIRE_VERSION,
        sig: None,
        event: NetEvent::Ack { seq },
    })
    .expect("encode ack");
    target.repl.apply_events(&mut target.world, from, &bytes);
}

// ═══════════════════════ Core: LWW / echo / lifecycle ═══════════════════════

/// T9 ★ two peers, each authoritative over its own entity; full exchange each
/// tick; both converge to the authority's truth within quantization tolerance.
#[test]
fn two_peers_replicate_own_entities() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e_a = a.spawn(0.0, 0.0, 2.0, 0.0);
    let e_b = b.spawn(10.0, 10.0, 0.0, -2.0);

    for _ in 0..5 {
        a.sim_tick();
        b.sim_tick();
        let out_a = a.collect_for(b.id);
        let out_b = b.collect_for(a.id);
        b.deliver_all(a.id, &out_a);
        a.deliver_all(b.id, &out_b);
    }

    assert_eq!(a.entity_count(), 2);
    assert_eq!(b.entity_count(), 2);

    let proxy_a_on_b = b.entity_owned_by(a.id);
    let truth_a = a.pos(e_a);
    let got = b.pos(proxy_a_on_b);
    assert!(approx(got.x, truth_a.x) && approx(got.y, truth_a.y));

    let proxy_b_on_a = a.entity_owned_by(b.id);
    let truth_b = b.pos(e_b);
    let got = a.pos(proxy_b_on_a);
    assert!(approx(got.x, truth_b.x) && approx(got.y, truth_b.y));
}

/// T10 — the receiver NEVER re-simulates a remote entity.
#[test]
fn receiver_never_resimulates_remote() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(1.0, 1.0, 2.0, 2.0);
    let out = a.collect_for(b.id);
    b.deliver_all(a.id, &out);

    let proxy = b.entity_owned_by(a.id);
    let before = b.pos(proxy);
    for _ in 0..10 {
        b.sim_tick();
    }
    let after = b.pos(proxy);
    assert_eq!(before.x.to_bits(), after.x.to_bits());
    assert_eq!(before.y.to_bits(), after.y.to_bits());
}

/// T11 ★ THE echo-back test: applying remote state fires change detection, but
/// the receiver's next collect must emit NOTHING (authority gate first).
#[test]
fn applied_state_never_echoed() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(1.0, 1.0, 2.0, 0.0);
    let out = a.collect_for(b.id);
    b.deliver_all(a.id, &out);

    let out_b = b.collect_for(a.id);
    assert!(out_b.state.is_none(), "B must not echo A's applied state");
    assert!(out_b.events.is_empty(), "B must not announce A's proxy");
}

/// T12 ★ the mask tracks exactly the changed component set (with the prior
/// value confirmed between changes, so the delta stays quiet otherwise).
#[test]
fn mask_tracks_single_component_change() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(1.0, 1.0, 0.0, 0.0);
    b.deliver_all(a.id, &a.collect_for(b.id));
    flush_acks(&mut b, &mut a);
    assert!(
        a.collect_for(b.id).state.is_none(),
        "confirmed + unchanged -> no state message"
    );

    // Mutate ONLY Position.
    a.world.get_mut::<Position>(e).unwrap().x = 5.0;
    let out = a.collect_for(b.id);
    let entries = state_entries(&out);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].mask(), 0b01);
    assert!(entries[0].pos.is_some() && entries[0].vel.is_none());
    b.deliver_all(a.id, &out);
    flush_acks(&mut b, &mut a); // confirm pos before the next change

    // Mutate ONLY Velocity.
    a.world.get_mut::<Velocity>(e).unwrap().y = 3.0;
    let entries = state_entries(&a.collect_for(b.id));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].mask(), 0b10);
    assert!(entries[0].pos.is_none() && entries[0].vel.is_some());
}

/// T13 ★ out-of-order delivery: newest-seq wins, an older msg arriving later
/// mutates nothing.
#[test]
fn out_of_order_state_newest_wins() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(0.0, 0.0, 2.0, 0.0);

    let out1 = a.collect_for(b.id); // seq 1: spawn + initial state
    a.sim_tick();
    let out2 = a.collect_for(b.id); // seq 2
    a.sim_tick();
    let out3 = a.collect_for(b.id); // seq 3

    b.deliver_events(a.id, &out1.events);
    b.deliver_state(a.id, out1.state.as_ref().unwrap());
    b.deliver_state(a.id, out3.state.as_ref().unwrap());

    let proxy = b.entity_owned_by(a.id);
    let at_seq3 = b.pos(proxy);

    // The straggler (seq 2) must be a complete no-op.
    b.deliver_state(a.id, out2.state.as_ref().unwrap());
    let after = b.pos(proxy);
    assert_eq!(at_seq3.x.to_bits(), after.x.to_bits());
    assert_eq!(at_seq3.y.to_bits(), after.y.to_bits());
}

/// T14 — duplicated state delivery is idempotent.
#[test]
fn duplicate_state_idempotent() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(0.0, 0.0, 2.0, 0.0);
    a.sim_tick();
    let out = a.collect_for(b.id);
    b.deliver_all(a.id, &out);

    let proxy = b.entity_owned_by(a.id);
    let before = b.pos(proxy);
    let count_before = b.entity_count();
    b.deliver_state(a.id, out.state.as_ref().unwrap()); // duplicate
    assert_eq!(before.x.to_bits(), b.pos(proxy).x.to_bits());
    assert_eq!(count_before, b.entity_count());
}

/// T15 — state arriving before its Spawn is dropped (no panic, no entity),
/// then heals once the spawn lands.
#[test]
fn state_before_spawn_drops_then_heals() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(1.0, 1.0, 2.0, 0.0);
    let out1 = a.collect_for(b.id);

    b.deliver_state(a.id, out1.state.as_ref().unwrap());
    assert_eq!(b.entity_count(), 0, "state-before-spawn creates no entity");

    b.deliver_events(a.id, &out1.events);
    a.sim_tick();
    let out2 = a.collect_for(b.id);
    b.deliver_state(a.id, out2.state.as_ref().unwrap());

    let proxy = b.entity_owned_by(a.id);
    let e_a = a.entity_owned_by(a.id);
    let truth = a.pos(e_a);
    assert!(approx(b.pos(proxy).x, truth.x));
}

/// T16 ★ stale-generation state is NEVER misapplied (full-NetEntityId keying).
#[test]
fn stale_generation_never_misapplied() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e1 = a.spawn(1.0, 1.0, 0.0, 0.0);
    let out1 = a.collect_for(b.id);
    let old_id = spawn_ids(&out1.events)[0];
    assert_eq!(old_id.index, e1.index_u32());
    b.deliver_all(a.id, &out1);

    a.world.get_mut::<Position>(e1).unwrap().x = 2.0;
    let stale = a.collect_for(b.id).state.unwrap();

    a.world.despawn(e1);
    let out_despawn = a.collect_for(b.id);
    b.deliver_events(a.id, &out_despawn.events);
    assert_eq!(b.entity_count(), 0);

    let new_id = protocol::NetEntityId {
        spawner: old_id.spawner,
        index: old_id.index,
        generation: old_id.generation + 1,
    };
    let spawn_new = protocol::encode_event(&protocol::EventMsg {
        version: protocol::WIRE_VERSION,
        sig: None,
        event: NetEvent::Spawn {
            id: new_id,
            pos: protocol::quantize_vec2(5.0, 5.0),
            vel: protocol::quantize_vec2(0.0, 0.0),
        },
    })
    .unwrap();
    b.deliver_events(a.id, &[spawn_new.into_boxed_slice()]);
    assert_eq!(b.entity_count(), 1, "new-generation proxy must exist");

    let mut stale_msg = decode_state(&stale).unwrap();
    stale_msg.seq = 1_000_000;
    let stale_bytes = protocol::encode_state(&stale_msg).unwrap();

    let proxy = b.entity_owned_by(a.id);
    let before = b.pos(proxy);
    b.deliver_state(a.id, &stale_bytes);
    let after = b.pos(proxy);
    assert_eq!(
        before.x.to_bits(),
        after.x.to_bits(),
        "stale gen misapplied"
    );
    assert_eq!(before.x, 5.0, "proxy still holds the new entity's state");
}

/// T17 — despawn followed by a late state message: no resurrection.
#[test]
fn despawn_then_late_state_no_resurrection() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e1 = a.spawn(1.0, 1.0, 2.0, 0.0);
    let out1 = a.collect_for(b.id);
    b.deliver_all(a.id, &out1);

    a.sim_tick();
    let late = a.collect_for(b.id).state.unwrap();

    a.world.despawn(e1);
    let out_despawn = a.collect_for(b.id);
    b.deliver_events(a.id, &out_despawn.events);
    assert_eq!(b.entity_count(), 0);

    b.deliver_state(a.id, &late);
    assert_eq!(b.entity_count(), 0, "late state must not resurrect");
}

/// T23 — the same Spawn bytes twice create exactly one proxy.
#[test]
fn duplicate_spawn_is_noop() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(1.0, 1.0, 0.0, 0.0);
    let out = a.collect_for(b.id);
    b.deliver_events(a.id, &out.events);
    b.deliver_events(a.id, &out.events);
    assert_eq!(b.entity_count(), 1);
}

/// T24 — a sender may only assert state for entities it currently owns; state
/// from a non-owner is dropped, and nobody can overwrite a receiver's own
/// authoritative entities.
#[test]
fn receiver_rejects_unowned_sender_state() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let c_id = PeerId(3);

    a.spawn(1.0, 1.0, 0.0, 0.0);
    let out_a = a.collect_for(b.id);
    let a_entity_id = spawn_ids(&out_a.events)[0];
    b.deliver_all(a.id, &out_a);
    let proxy = b.entity_owned_by(a.id);

    let forged = protocol::encode_state(&protocol::StateMsg {
        version: protocol::WIRE_VERSION,
        seq: 999,
        tick: 0,
        last_input: 0,
        entries: vec![StateEntry {
            id: a_entity_id,
            pos: Some(protocol::quantize_vec2(99.0, 99.0)),
            vel: None,
        }],
    })
    .unwrap();
    let before = b.pos(proxy);
    b.deliver_state(c_id, &forged);
    assert_eq!(before.x.to_bits(), b.pos(proxy).x.to_bits());

    let e_b = b.spawn(5.0, 5.0, 0.0, 0.0);
    let out_b = b.collect_for(a.id);
    let b_entity_id = spawn_ids(&out_b.events)[0];
    let forged = protocol::encode_state(&protocol::StateMsg {
        version: protocol::WIRE_VERSION,
        seq: 1000,
        tick: 0,
        last_input: 0,
        entries: vec![StateEntry {
            id: b_entity_id,
            pos: Some(protocol::quantize_vec2(-50.0, -50.0)),
            vel: None,
        }],
    })
    .unwrap();
    b.deliver_state(a.id, &forged);
    let own = b.pos(e_b);
    assert_eq!(
        own.x.to_bits(),
        5.0f32.to_bits(),
        "own authority must never be overwritten"
    );
}

/// T25 — first collect over a world with pre-existing owned entities emits
/// their Spawns plus a full-mask snapshot, seq == 1 (for a tracked peer).
#[test]
fn first_collect_announces_and_snapshots_everything() {
    let mut a = TestPeer::new(1);
    let b = PeerId(2);
    a.spawn(1.0, 1.0, 0.0, 0.0);
    a.spawn(2.0, 2.0, 1.0, 1.0);

    let out = a.collect_for(b);
    assert_eq!(spawn_ids(&out.events).len(), 2, "both entities announced");
    let msg = decode_state(out.state.as_ref().unwrap()).unwrap();
    assert_eq!(msg.seq, 1, "first state message carries seq 1");
    assert_eq!(msg.entries.len(), 2);
    for entry in &msg.entries {
        assert_eq!(entry.mask(), 0b11, "first snapshot is full-mask");
    }
}

// ═══════════════════════ Handoff (per-peer) ═══════════════════════

/// T18 ★ clean A→B ownership handoff: authority flips atomically on A, the
/// NetEntityId stays stable, B switches from apply to compute, and at no point
/// do both peers collect the entity.
#[test]
fn handoff_clean_authority_transfer() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e_a = a.spawn(0.0, 0.0, 2.0, 0.0);
    let out = a.collect_for(b.id);
    let original_id = spawn_ids(&out.events)[0];
    b.deliver_all(a.id, &out);
    let proxy = b.entity_owned_by(a.id);

    // B cannot give away what it does not own.
    assert!(
        b.repl
            .transfer_ownership(&mut b.world, proxy, b.id)
            .is_err()
    );

    // A transfers to B: local Owner flips immediately; A stops computing it.
    a.repl
        .transfer_ownership(&mut a.world, e_a, b.id)
        .expect("A owns e_a");
    assert_eq!(a.owner(e_a), b.id, "A's local Owner flips immediately");
    let frozen = a.pos(e_a);
    a.sim_tick();
    assert_eq!(
        frozen.x.to_bits(),
        a.pos(e_a).x.to_bits(),
        "the old owner stops computing a transferred entity"
    );

    let out_a = a.collect_for(b.id);
    assert!(
        state_entries(&out_a).is_empty(),
        "A collects no state for a transferred entity"
    );
    let has_transfer = out_a.events.iter().any(|ev| {
        matches!(
            decode_event(ev).unwrap().event,
            NetEvent::OwnershipTransfer { id, new_owner } if id == original_id && new_owner == PeerId(2)
        )
    });
    assert!(has_transfer, "transfer event on the reliable channel");

    // B receives the transfer: proxy Owner flips, B now computes it.
    b.deliver_events(a.id, &out_a.events);
    assert_eq!(b.owner(proxy), b.id);
    let before = b.pos(proxy);
    b.sim_tick();
    assert!(
        b.pos(proxy).x > before.x,
        "B now simulates the adopted entity"
    );

    // B's collect emits state addressed to the ORIGINAL id (spawner-stable).
    let out_b = b.collect_for(a.id);
    let entries = state_entries(&out_b);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, original_id, "identity survives the transfer");
}

/// T19 ★ in-flight state from the OLD owner arriving at a third peer AFTER the
/// transfer event is dropped by the ownership gate.
#[test]
fn stale_old_owner_state_dropped_after_transfer() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut c = TestPeer::new(3);
    let e_a = a.spawn(0.0, 0.0, 2.0, 0.0);
    a.track(b.id);
    a.track(c.id);
    let outs = a.collect_all();
    b.deliver_all(a.id, outbox_for(&outs, b.id).unwrap());
    c.deliver_all(a.id, outbox_for(&outs, c.id).unwrap());
    let proxy_on_c = c.entity_owned_by(a.id);

    a.sim_tick();
    let in_flight = a.collect_for(c.id).state.unwrap(); // from A, still owned
    a.repl
        .transfer_ownership(&mut a.world, e_a, b.id)
        .expect("A owns e_a");
    let out_transfer = a.collect_for(c.id);
    c.deliver_events(a.id, &out_transfer.events);
    assert_eq!(c.owner(proxy_on_c), b.id);

    let before = c.pos(proxy_on_c);
    c.deliver_state(a.id, &in_flight);
    assert_eq!(
        before.x.to_bits(),
        c.pos(proxy_on_c).x.to_bits(),
        "old-owner state must be dropped"
    );
}

/// T20 — state from the NEW owner arriving BEFORE the transfer event is
/// dropped; once the event lands, the next snapshot applies.
#[test]
fn early_new_owner_state_dropped_until_transfer() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut c = TestPeer::new(3);
    let e_a = a.spawn(0.0, 0.0, 2.0, 0.0);
    a.track(b.id);
    a.track(c.id);
    let outs = a.collect_all();
    b.deliver_all(a.id, outbox_for(&outs, b.id).unwrap());
    c.deliver_all(a.id, outbox_for(&outs, c.id).unwrap());
    let proxy_on_c = c.entity_owned_by(a.id);

    a.repl
        .transfer_ownership(&mut a.world, e_a, b.id)
        .expect("A owns e_a");
    let out_transfer = a.collect_for(b.id);
    b.deliver_all(a.id, &out_transfer);
    let proxy_on_b = b.entity_owned_by(b.id);

    b.world.get_mut::<Position>(proxy_on_b).unwrap().x = 7.0;
    let early = b.collect_for(c.id).state.unwrap();
    let before = c.pos(proxy_on_c);
    c.deliver_state(b.id, &early);
    assert_eq!(
        before.x.to_bits(),
        c.pos(proxy_on_c).x.to_bits(),
        "early new-owner state must drop"
    );

    c.deliver_events(a.id, &out_transfer.events);
    b.world.get_mut::<Position>(proxy_on_b).unwrap().x = 8.0;
    let next = b.collect_for(c.id).state.unwrap();
    c.deliver_state(b.id, &next);
    assert!(
        approx(c.pos(proxy_on_c).x, 8.0),
        "post-transfer state applies"
    );
}

// ═══════════════════════ Delta baseline (ADR-0020, per-peer) ═══════════════════════

/// T21 — the acked baseline heals a lost final packet: the unconfirmed value is
/// re-sent every tick until acked.
#[test]
fn acked_baseline_heals_dropped_final_packet() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(1.0, 1.0, 0.0, 0.0);
    let out = a.collect_for(b.id);
    b.deliver_all(a.id, &out);
    flush_acks(&mut b, &mut a);

    a.world.get_mut::<Position>(e).unwrap().x = 9.0;
    let _lost = a.collect_for(b.id); // seq 2: pos(9,1) DROPPED

    let out = a.collect_for(b.id); // seq 3: pos still (9,1)
    assert!(
        out.state.is_some(),
        "an unconfirmed value is re-sent until acked"
    );
    b.deliver_all(a.id, &out);
    let proxy = b.entity_owned_by(a.id);
    assert!(
        approx(b.pos(proxy).x, 9.0),
        "the re-send heals the lost packet"
    );

    flush_acks(&mut b, &mut a);
    assert!(
        a.collect_for(b.id).state.is_none(),
        "a confirmed value is not re-sent"
    );
}

/// T29 ★ confirm → quiet: a stationary confirmed entity stops being re-sent.
#[test]
fn confirmed_value_goes_quiet() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(4.0, 4.0, 0.0, 0.0);
    let out = a.collect_for(b.id);
    assert!(out.state.is_some(), "first send carries the value");
    b.deliver_all(a.id, &out);
    flush_acks(&mut b, &mut a);

    for _ in 0..5 {
        assert!(
            a.collect_for(b.id).state.is_none(),
            "a confirmed, unchanged value is never re-sent"
        );
    }
}

/// T30 ★ a value change re-arms confirmation: an ack of the OLD value does not
/// confirm the NEW one.
#[test]
fn value_change_re_arms_confirmation() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    b.deliver_all(a.id, &a.collect_for(b.id));
    flush_acks(&mut b, &mut a);

    a.world.get_mut::<Position>(e).unwrap().x = 7.0;
    let _lost = a.collect_for(b.id); // seq 2 (7,0) dropped

    let out = a.collect_for(b.id); // seq 3: pos(7,0) re-sent
    assert!(
        state_entries(&out).iter().any(|s| s.pos.is_some()),
        "the new value is not confirmed by an old ack"
    );
    b.deliver_all(a.id, &out);
    flush_acks(&mut b, &mut a);
    assert!(
        a.collect_for(b.id).state.is_none(),
        "confirmed after the ack catches up"
    );
}

/// T33 ★ cumulative-ack soundness under loss: a value across seqs 1/2/3 with the
/// middle dropped is confirmed by an ack of a surviving seq; a LATER value is
/// NOT confirmed by that older ack.
#[test]
fn cumulative_ack_sound_under_loss() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(1.0, 0.0, 0.0, 0.0);

    let m1 = a.collect_for(b.id); // seq 1
    b.deliver_all(a.id, &m1);
    let _m2 = a.collect_for(b.id); // seq 2 DROPPED
    let m3 = a.collect_for(b.id); // seq 3
    b.deliver_all(a.id, &m3);
    flush_acks(&mut b, &mut a); // B acks 3 >= run_start 1 -> confirmed
    assert!(
        a.collect_for(b.id).state.is_none(),
        "value confirmed: B received it in a surviving message"
    );
    let proxy = b.entity_owned_by(a.id);
    assert!(approx(b.pos(proxy).x, 1.0));

    a.world.get_mut::<Position>(e).unwrap().x = 8.0;
    assert!(
        state_entries(&a.collect_for(b.id))
            .iter()
            .any(|s| s.pos.is_some()),
        "the new value's run is unconfirmed until B acks a message carrying it"
    );
}

/// T34 — despawning an owned entity prunes its (per-peer) delta baseline.
#[test]
fn despawn_prunes_baseline() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(1.0, 1.0, 0.0, 0.0);
    b.deliver_all(a.id, &a.collect_for(b.id));
    flush_acks(&mut b, &mut a);

    a.world.despawn(e);
    let out = a.collect_for(b.id); // Despawn event; no state for the gone entity
    assert!(
        state_entries(&out).is_empty(),
        "no state entry for a despawned entity"
    );
    assert_eq!(despawn_count(&out), 1, "the despawn is announced");

    a.spawn(2.0, 2.0, 0.0, 0.0);
    let entries = state_entries(&a.collect_for(b.id));
    assert_eq!(entries.len(), 1, "only the new entity is sent");
    assert_eq!(entries[0].mask(), 0b11, "fresh baseline is a full send");
}

/// T35 ★ ACCEPTANCE — bandwidth drops vs the keyframe scheme. A confirmed
/// stationary scene sends ZERO steady-state bytes; the first send is the
/// keyframe-equivalent cost the old scheme re-paid every 30 ticks.
#[test]
fn bandwidth_drops_for_stationary_scene() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    for i in 0..8 {
        a.spawn(i as f32, i as f32, 0.0, 0.0);
    }

    let mut first_send_bytes = 0usize;
    let mut steady_state_bytes = 0usize;
    for tick in 0..40 {
        let out = a.collect_for(b.id);
        if let Some(state) = &out.state {
            if tick == 0 {
                first_send_bytes = state.len();
            } else if tick >= 4 {
                steady_state_bytes += state.len();
            }
            b.deliver_all(a.id, &out);
        }
        flush_acks(&mut b, &mut a);
    }
    assert!(first_send_bytes > 0, "first send is a full snapshot");
    assert_eq!(
        steady_state_bytes, 0,
        "a confirmed stationary scene sends ZERO steady-state bytes"
    );
}

/// T37 ★ THE F1 regression — state racing its Spawn must NOT be acked.
#[test]
fn state_before_spawn_defers_ack() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(5.0, 5.0, 0.0, 0.0);

    let m1 = a.collect_for(b.id); // seq 1: Spawn + state
    assert!(!m1.events.is_empty(), "first collect mints a Spawn");
    assert!(m1.state.is_some(), "first collect sends state");

    b.deliver_state(a.id, m1.state.as_ref().unwrap()); // state only — dropped
    assert!(
        b.repl.drain_acks().is_empty(),
        "F1: a dropped-entry message must NOT be acked"
    );

    let m2 = a.collect_for(b.id); // seq 2: re-sent (unconfirmed)
    assert!(
        state_entries(&m2).iter().any(|s| s.pos.is_some()),
        "unconfirmed -> still re-sent"
    );

    b.deliver_events(a.id, &m1.events); // build the proxy
    b.deliver_state(a.id, m2.state.as_ref().unwrap());
    let proxy = b.entity_owned_by(a.id);
    assert!(approx(b.pos(proxy).x, 5.0) && approx(b.pos(proxy).y, 5.0));

    flush_acks(&mut b, &mut a);
    assert!(
        a.collect_for(b.id).state.is_none(),
        "confirmed only after a message that actually applied"
    );
}

// ═══════════════════════ Group A — AOI gate basics ═══════════════════════

/// A1 — an entity inside a peer's AOI is spawned + stated to it.
#[test]
fn aoi_gate_includes_in_radius() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 10.0);
    a.spawn(3.0, 4.0, 0.0, 0.0); // dist 5 < 10
    let out = a.collect_for(b.id);
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 1, "in-AOI entity is replicated");
    let proxy = b.entity_owned_by(a.id);
    assert!(approx(b.pos(proxy).x, 3.0) && approx(b.pos(proxy).y, 4.0));
}

/// A2 — an entity outside a peer's AOI gets NO Spawn and NO state.
#[test]
fn aoi_gate_excludes_out_of_radius() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 10.0);
    a.spawn(100.0, 100.0, 0.0, 0.0);
    let out = a.collect_for(b.id);
    assert!(out.state.is_none(), "no state for an out-of-AOI entity");
    assert!(spawn_ids(&out.events).is_empty(), "no Spawn either");
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 0);
}

/// A3 — a tracked peer with NO AOI set sees ALL owned entities (unbounded).
#[test]
fn no_aoi_peer_sees_all_owned() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id); // no set_aoi
    a.spawn(0.0, 0.0, 0.0, 0.0);
    a.spawn(9999.0, 9999.0, 0.0, 0.0);
    let out = a.collect_for(b.id);
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 2, "unbounded default sees all owned");
}

// ═══════════════════════ Group B — enter / exit / re-enter ═══════════════════════

/// B1 — crossing INTO the AOI emits Spawn (before state); the proxy builds.
#[test]
fn aoi_enter_emits_spawn_then_state() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 5.0);
    let e = a.spawn(100.0, 0.0, 0.0, 0.0); // OUT
    assert!(a.collect_for(b.id).state.is_none(), "out of AOI: nothing");

    a.world.get_mut::<Position>(e).unwrap().x = 2.0; // IN
    let out = a.collect_for(b.id);
    assert_eq!(spawn_ids(&out.events).len(), 1, "enter emits a Spawn");
    assert!(out.state.is_some(), "enter also sends the first state");
    b.deliver_all(a.id, &out);
    let proxy = b.entity_owned_by(a.id);
    assert!(approx(b.pos(proxy).x, 2.0));
}

/// B2 — crossing OUT of the AOI emits a Despawn; the proxy is removed.
#[test]
fn aoi_exit_emits_despawn() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 5.0);
    let e = a.spawn(1.0, 0.0, 0.0, 0.0); // IN
    b.deliver_all(a.id, &a.collect_for(b.id));
    assert_eq!(b.entity_count(), 1);

    a.world.get_mut::<Position>(e).unwrap().x = 100.0; // OUT
    let out = a.collect_for(b.id);
    assert_eq!(despawn_count(&out), 1, "exit emits exactly one Despawn");
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 0, "proxy removed on AOI-exit");
}

/// B3 — in→out→in re-enters with a full re-spawn + full-mask state.
#[test]
fn aoi_reenter_respawns_fresh_baseline() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 5.0);
    let e = a.spawn(1.0, 1.0, 0.0, 0.0); // IN
    b.deliver_all(a.id, &a.collect_for(b.id));
    flush_acks(&mut b, &mut a);

    a.world.get_mut::<Position>(e).unwrap().x = 100.0; // OUT
    b.deliver_all(a.id, &a.collect_for(b.id));
    assert_eq!(b.entity_count(), 0);

    a.world.get_mut::<Position>(e).unwrap().x = 1.0; // back IN
    let out = a.collect_for(b.id);
    assert_eq!(spawn_ids(&out.events).len(), 1, "re-enter re-spawns");
    let entries = state_entries(&out);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].mask(), 0b11, "fresh baseline is full-mask");
    b.deliver_all(a.id, &out);
    let proxy = b.entity_owned_by(a.id);
    assert!(approx(b.pos(proxy).x, 1.0) && approx(b.pos(proxy).y, 1.0));
}

/// B4 ★ WHITE-BOX — the exit/re-enter re-baseline: after an entity exits and
/// re-enters, its run_start jumps to the re-enter seq, so a STALE ack (of a seq
/// before the re-enter) cannot confirm it. Guards the `send_state[P][E]` drop on
/// exit (without it, a climbing acked_seq would falsely confirm on re-enter).
#[test]
fn reenter_stale_ack_does_not_confirm() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 50.0);
    let e = a.spawn(1.0, 0.0, 0.0, 0.0); // exits / re-enters
    let f = a.spawn(2.0, 0.0, 0.0, 0.0); // stays IN, advances the stream
    let e_id = net_id(a.id, e);

    b.deliver_all(a.id, &a.collect_for(b.id)); // seq 1: E + F
    flush_acks(&mut b, &mut a);

    // E exits; F keeps moving (in-AOI) so b's stream + acked_seq climb.
    a.world.get_mut::<Position>(e).unwrap().x = 1000.0; // OUT
    for i in 0..3 {
        a.world.get_mut::<Position>(f).unwrap().x = 3.0 + i as f32; // < 50
        let out = a.collect_for(b.id);
        b.deliver_all(a.id, &out);
        flush_acks(&mut b, &mut a);
    }

    // E re-enters at a NEW run_start (the current seq).
    a.world.get_mut::<Position>(e).unwrap().x = 1.0; // back IN
    let out = a.collect_for(b.id);
    assert!(
        state_entries(&out).iter().any(|s| s.id == e_id),
        "E re-sent on re-enter"
    );
    let reenter_seq = state_seq(&out);
    b.deliver_all(a.id, &out);

    // A stale ack (< the re-enter run_start) must NOT confirm the re-entered E.
    inject_ack(&mut a, b.id, reenter_seq - 1);
    assert!(
        state_entries(&a.collect_for(b.id))
            .iter()
            .any(|s| s.id == e_id),
        "a stale ack (< re-enter run_start) must not confirm E"
    );
    // A correct ack >= the re-enter run_start confirms it.
    inject_ack(&mut a, b.id, reenter_seq);
    assert!(
        !state_entries(&a.collect_for(b.id))
            .iter()
            .any(|s| s.id == e_id),
        "E quiet once b acks >= the re-enter run_start"
    );
}

/// B5 — after an AOI-exit, further ticks with the entity still out emit NO
/// repeat Despawn (guards the `known[P]` removal on exit).
#[test]
fn aoi_exit_no_repeat_despawn() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 5.0);
    let e = a.spawn(1.0, 0.0, 0.0, 0.0);
    b.deliver_all(a.id, &a.collect_for(b.id));

    a.world.get_mut::<Position>(e).unwrap().x = 100.0; // OUT
    assert_eq!(
        despawn_count(&a.collect_for(b.id)),
        1,
        "one despawn on exit"
    );
    // Still out on subsequent ticks: no further despawn.
    for _ in 0..3 {
        assert_eq!(
            despawn_count(&a.collect_for(b.id)),
            0,
            "no repeat despawn while it stays out"
        );
    }
}

// ═══════════════════════ Group C — per-peer independence ═══════════════════════

/// C1 — one collect_all: X's AOI covers E, Y's excludes it → X sees E, Y nothing.
#[test]
fn per_peer_visibility_independent() {
    let mut a = TestPeer::new(1);
    let mut x = TestPeer::new(2);
    let mut y = TestPeer::new(3);
    a.track(x.id);
    a.track(y.id);
    a.set_aoi(x.id, (0.0, 0.0), 10.0); // E in range
    a.set_aoi(y.id, (500.0, 500.0), 10.0); // E out of range
    a.spawn(1.0, 1.0, 0.0, 0.0);

    let outs = a.collect_all();
    if let Some(ox) = outbox_for(&outs, x.id) {
        x.deliver_all(a.id, ox);
    }
    if let Some(oy) = outbox_for(&outs, y.id) {
        y.deliver_all(a.id, oy);
    }
    assert_eq!(x.entity_count(), 1, "X sees the in-AOI entity");
    assert_eq!(y.entity_count(), 0, "Y structurally never receives it");
}

/// C2 (replaces T32) — per-peer seq streams are independent: both peers see E,
/// each stream starts at seq 1; acking X quiets X while Y still carries E.
#[test]
fn per_peer_seq_streams_independent() {
    let mut a = TestPeer::new(1);
    let mut x = TestPeer::new(2);
    let mut y = TestPeer::new(3);
    a.track(x.id);
    a.track(y.id);
    a.spawn(5.0, 5.0, 0.0, 0.0); // both see it (unbounded)

    let outs = a.collect_all();
    assert_eq!(
        state_seq(outbox_for(&outs, x.id).unwrap()),
        1,
        "X stream seq 1"
    );
    assert_eq!(
        state_seq(outbox_for(&outs, y.id).unwrap()),
        1,
        "Y stream seq 1"
    );
    x.deliver_all(a.id, outbox_for(&outs, x.id).unwrap());
    y.deliver_all(a.id, outbox_for(&outs, y.id).unwrap());
    flush_acks(&mut x, &mut a); // only X acks

    let outs = a.collect_all();
    assert!(
        outbox_for(&outs, x.id).is_none_or(|o| o.state.is_none()),
        "X confirmed -> quiet"
    );
    assert!(
        outbox_for(&outs, y.id).is_some_and(|o| o.state.is_some()),
        "Y unconfirmed -> still sent"
    );
    flush_acks(&mut y, &mut a);
    let outs = a.collect_all();
    assert!(outs.is_empty(), "both confirmed -> everyone quiet");
}

/// C3 (replaces T31) — a new joiner gets the entity on ITS stream only; the
/// already-confirmed peer stays quiet.
#[test]
fn new_join_sends_only_to_joiner() {
    let mut a = TestPeer::new(1);
    let mut x = TestPeer::new(2);
    let mut y = TestPeer::new(3);
    a.spawn(2.0, 3.0, 0.0, 0.0);
    b_confirm(&mut a, &mut x); // X confirms E, then A is quiet to X

    // Y joins (tracked). One collect_all: E on Y's stream only.
    a.repl.on_peer_connected(y.id);
    let outs = a.collect_all();
    assert!(
        outbox_for(&outs, x.id).is_none(),
        "the confirmed peer gets nothing"
    );
    let oy = outbox_for(&outs, y.id).expect("the joiner gets E");
    assert_eq!(spawn_ids(&oy.events).len(), 1, "joiner gets a Spawn");
    assert!(oy.state.is_some(), "joiner gets the state");
    y.deliver_all(a.id, oy);
    assert_eq!(y.entity_count(), 1);
}

/// Helper for C3: A sends its (single) entity to X and X confirms it.
fn b_confirm(a: &mut TestPeer, x: &mut TestPeer) {
    let out = a.collect_for(x.id);
    x.deliver_all(a.id, &out);
    flush_acks(x, a);
    assert!(a.collect_for(x.id).state.is_none(), "quiet once X confirms");
}

// ═══════════════════════ Group D — read-cheat completeness ═══════════════════════

/// D1 — an entity outside a peer's AOI for its whole life (moving each tick)
/// NEVER has its existence (a Spawn) leaked to that peer.
#[test]
fn out_of_aoi_withholds_existence() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 5.0);
    let e = a.spawn(1000.0, 0.0, 2.0, 0.0); // far, moving

    for i in 0..10 {
        a.world.get_mut::<Position>(e).unwrap().x = 1000.0 + i as f32;
        let out = a.collect_for(b.id);
        assert!(
            spawn_ids(&out.events).is_empty(),
            "existence of an out-of-AOI entity must never leak"
        );
        b.deliver_all(a.id, &out);
    }
    assert_eq!(
        b.entity_count(),
        0,
        "the peer never learns the entity exists"
    );
}

/// D2 — a straggler unreliable state for an entity that just exited is safe: no
/// resurrection, no panic, no re-send storm.
#[test]
fn straggler_state_after_exit_is_safe() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 5.0);
    let e = a.spawn(1.0, 0.0, 0.0, 0.0); // IN
    let in_view = a.collect_for(b.id);
    b.deliver_all(a.id, &in_view);
    let straggler = in_view.state.unwrap();

    a.world.get_mut::<Position>(e).unwrap().x = 100.0; // OUT
    let exit = a.collect_for(b.id);
    b.deliver_all(a.id, &exit); // Despawn delivered; proxy gone
    assert_eq!(b.entity_count(), 0);

    // The old in-view state arrives late (its seq is stale) — inert.
    b.deliver_state(a.id, &straggler);
    assert_eq!(b.entity_count(), 0, "no resurrection from a straggler");
    // Still out of AOI: no re-send.
    assert!(
        a.collect_for(b.id).state.is_none(),
        "an out-of-AOI entity is not re-sent"
    );
}

// ═══════════════════════ Group E — transfer under AOI ═══════════════════════

/// E1 — a peer that knows the entity gets an OwnershipTransfer (proxy kept),
/// NOT a Despawn; the sender stops collecting the transferred entity.
#[test]
fn transfer_known_peer_gets_transfer_not_despawn() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let q = PeerId(3);
    let e = a.spawn(1.0, 1.0, 0.0, 0.0);
    b.deliver_all(a.id, &a.collect_for(b.id));
    let proxy = b.entity_owned_by(a.id);

    a.repl
        .transfer_ownership(&mut a.world, e, q)
        .expect("A owns e");
    let out = a.collect_for(b.id);
    assert_eq!(despawn_count(&out), 0, "a transfer is not a despawn");
    let has_transfer = out.events.iter().any(|ev| {
        matches!(
            decode_event(ev).unwrap().event,
            NetEvent::OwnershipTransfer { new_owner, .. } if new_owner == q
        )
    });
    assert!(has_transfer, "the observer gets the transfer");
    assert!(state_entries(&out).is_empty(), "A no longer collects it");

    b.deliver_all(a.id, &out);
    assert_eq!(b.owner(proxy), q, "the proxy is kept under the new owner");
}

/// E2 — an observer that does NOT have the entity in its AOI learns nothing
/// about a transfer (the read-cheat holds under handoff).
#[test]
fn transfer_out_of_aoi_observer_gets_nothing() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let q = PeerId(3);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 5.0);
    let e = a.spawn(1000.0, 0.0, 0.0, 0.0); // out of B's AOI
    assert!(a.collect_for(b.id).state.is_none());

    a.repl
        .transfer_ownership(&mut a.world, e, q)
        .expect("A owns e");
    let out = a.collect_for(b.id);
    assert!(
        out.state.is_none() && out.events.is_empty(),
        "B told nothing"
    );
    assert_eq!(b.entity_count(), 0);
}

/// E3 (rework T28) — the NEW OWNER is notified regardless of AOI: transferring
/// an uncollected / out-of-view entity to Q ships Spawn THEN Transfer to Q, so
/// Q can build+own the proxy.
#[test]
fn transfer_notifies_new_owner_regardless_of_aoi() {
    let mut a = TestPeer::new(1);
    let mut q = TestPeer::new(2);
    let e = a.spawn(3.0, 4.0, 2.0, 0.0);
    // Never collected; and give Q an AOI far from the entity — it must still be
    // notified because it is becoming the owner.
    a.set_aoi(q.id, (9999.0, 9999.0), 1.0);
    a.repl
        .transfer_ownership(&mut a.world, e, q.id)
        .expect("A owns e");

    let out = a.collect_for(q.id);
    let events: Vec<NetEvent> = out
        .events
        .iter()
        .map(|b| decode_event(b).unwrap().event)
        .collect();
    let spawn_idx = events
        .iter()
        .position(|ev| matches!(ev, NetEvent::Spawn { .. }))
        .expect("a Spawn for the new owner");
    let transfer_idx = events
        .iter()
        .position(|ev| matches!(ev, NetEvent::OwnershipTransfer { .. }))
        .expect("the OwnershipTransfer");
    assert!(spawn_idx < transfer_idx, "Spawn precedes Transfer");

    q.deliver_all(a.id, &out);
    assert_eq!(q.entity_count(), 1);
    let proxy = q.entity_owned_by(q.id);
    assert!(approx(q.pos(proxy).x, 3.0) && approx(q.pos(proxy).y, 4.0));
    q.sim_tick();
    assert!(q.pos(proxy).x > 3.0, "Q now computes the adopted entity");
}

// ═══════════════════════ Group F — dead vs transfer / dead under AOI ═══════════════════════

/// F1 (rework T27) — transfer-then-despawn in the SAME tick yields a Despawn,
/// NOT a Transfer (dead wins over transfer): the receiver ends with no entity.
#[test]
fn dead_over_transfer_same_tick() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let q = PeerId(3);
    let e = a.spawn(1.0, 1.0, 0.0, 0.0);
    b.deliver_all(a.id, &a.collect_for(b.id));
    assert_eq!(b.entity_count(), 1);

    // Same tick: give it away, then destroy it before collect.
    a.repl
        .transfer_ownership(&mut a.world, e, q)
        .expect("A owns e");
    a.world.despawn(e);

    let out = a.collect_for(b.id);
    let mut saw_transfer = false;
    let mut saw_despawn = false;
    for ev in &out.events {
        match decode_event(ev).unwrap().event {
            NetEvent::OwnershipTransfer { .. } => saw_transfer = true,
            NetEvent::Despawn { .. } => saw_despawn = true,
            _ => {}
        }
    }
    assert!(
        !saw_transfer,
        "a transfer for a dead entity must be dropped"
    );
    assert!(saw_despawn, "the despawn is announced instead");
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 0, "receiver keeps no owned ghost");
}

/// F2 — a real despawn under AOI: a peer that knows the entity gets exactly one
/// Despawn; its baseline is pruned.
#[test]
fn dead_under_aoi_known_peer_despawns() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 10.0);
    let e = a.spawn(1.0, 1.0, 0.0, 0.0);
    b.deliver_all(a.id, &a.collect_for(b.id));
    assert_eq!(b.entity_count(), 1);

    a.world.despawn(e);
    let out = a.collect_for(b.id);
    assert_eq!(despawn_count(&out), 1, "exactly one Despawn");
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 0);
}

/// F3 — a despawn of an entity the peer had already lost from AOI: no Despawn
/// (the peer already despawned it on exit / never knew it).
#[test]
fn dead_out_of_aoi_no_despawn() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 5.0);
    let e = a.spawn(1.0, 0.0, 0.0, 0.0); // IN
    b.deliver_all(a.id, &a.collect_for(b.id));
    a.world.get_mut::<Position>(e).unwrap().x = 100.0; // OUT
    b.deliver_all(a.id, &a.collect_for(b.id)); // exit despawn
    assert_eq!(b.entity_count(), 0);

    a.world.despawn(e); // now really dead, but B already forgot it
    let out = a.collect_for(b.id);
    assert_eq!(
        despawn_count(&out),
        0,
        "no despawn for an already-forgotten entity"
    );
}

// ═══════════════════════ Group H — leak / timing ═══════════════════════

/// H1 — untrack then reconnect (same id, fresh world) MUST re-emit the Spawn:
/// `untrack_peer` clears `known`/`send_state`, so the joiner isn't seen as
/// already-known (which would suppress the Spawn forever).
#[test]
fn untrack_then_reconnect_respawns() {
    let mut a = TestPeer::new(1);
    let b_id = PeerId(2);
    a.spawn(1.0, 1.0, 0.0, 0.0);

    // First session: B receives + confirms the entity.
    let mut b = TestPeer::new(2);
    b.deliver_all(a.id, &a.collect_for(b_id));
    flush_acks(&mut b, &mut a);
    assert_eq!(b.entity_count(), 1);

    // B disconnects.
    a.repl.untrack_peer(b_id);

    // B reconnects with the SAME id and a FRESH world.
    let mut b2 = TestPeer::new(2);
    a.repl.on_peer_connected(b_id);
    let out = a.collect_for(b_id);
    assert_eq!(
        spawn_ids(&out.events).len(),
        1,
        "reconnect must re-emit the Spawn (known/send_state cleared)"
    );
    b2.deliver_all(a.id, &out);
    assert_eq!(
        b2.entity_count(),
        1,
        "the reconnected peer rebuilds the proxy"
    );
}

/// H2 — the id-map is pruned only AFTER every peer is notified: a single
/// collect_all delivers the Despawn to BOTH peers that knew a dying entity.
#[test]
fn dead_notifies_all_peers_before_map_removal() {
    let mut a = TestPeer::new(1);
    let mut x = TestPeer::new(2);
    let mut y = TestPeer::new(3);
    a.track(x.id);
    a.track(y.id);
    let e = a.spawn(1.0, 1.0, 0.0, 0.0);
    let outs = a.collect_all();
    x.deliver_all(a.id, outbox_for(&outs, x.id).unwrap());
    y.deliver_all(a.id, outbox_for(&outs, y.id).unwrap());
    assert_eq!(x.entity_count(), 1);
    assert_eq!(y.entity_count(), 1);

    a.world.despawn(e);
    let outs = a.collect_all(); // ONE collect for both peers
    assert_eq!(
        despawn_count(outbox_for(&outs, x.id).unwrap()),
        1,
        "X gets the despawn"
    );
    assert_eq!(
        despawn_count(outbox_for(&outs, y.id).unwrap()),
        1,
        "Y gets the despawn (map survived until after the peer loop)"
    );
    x.deliver_all(a.id, outbox_for(&outs, x.id).unwrap());
    y.deliver_all(a.id, outbox_for(&outs, y.id).unwrap());
    assert_eq!(x.entity_count(), 0);
    assert_eq!(y.entity_count(), 0);
}

/// F4 (auditor F2) — the corpse guard on the NEW-OWNER branch. Transfer to a
/// TRACKED new owner that has NEVER seen the entity, then despawn it the same
/// tick. `pending_transfers.remove(dead)` must fire so the new owner is not sent
/// a Spawn+Transfer for a corpse (an unhealable owned ghost). This exercises the
/// dangerous `p == q, !known` branch F1 never reached.
#[test]
fn dead_over_transfer_to_new_owner_no_corpse_spawn() {
    let mut a = TestPeer::new(1);
    let q = PeerId(3);
    a.track(q); // the tracked new owner — but has never seen e
    let e = a.spawn(1.0, 1.0, 0.0, 0.0);

    a.repl
        .transfer_ownership(&mut a.world, e, q)
        .expect("A owns e");
    a.world.despawn(e); // same tick

    let outs = a.collect_all();
    if let Some(oq) = outbox_for(&outs, q) {
        assert!(
            spawn_ids(&oq.events).is_empty(),
            "no corpse Spawn to the new owner"
        );
        assert!(
            !oq.events.iter().any(|ev| matches!(
                decode_event(ev).unwrap().event,
                NetEvent::OwnershipTransfer { .. }
            )),
            "no transfer of a corpse to the new owner"
        );
    }
}

/// E4 (auditor F1/F3) — handing an ADOPTED entity (spawner ≠ us) to a peer that
/// never witnessed it is a documented cross-sender gap: we cannot introduce a
/// foreign-namespace entity (the receiver rejects the Spawn), so we emit NO
/// Spawn (not a silently-dropped one) and the new owner builds no proxy until
/// Phase-3 resync. Distinguishes the F1 fix: pre-fix, A emitted a droppable
/// `Spawn{spawner=O}` here.
#[test]
fn adopted_entity_handoff_to_new_peer_is_documented_gap() {
    let mut o = TestPeer::new(1);
    let mut a = TestPeer::new(2);
    let mut q = TestPeer::new(3);
    let e = o.spawn(1.0, 1.0, 0.0, 0.0);

    // O replicates e to A, then hands it to A (A adopts; id.spawner stays O).
    let out = o.collect_for(a.id);
    a.deliver_all(o.id, &out);
    let proxy = a.entity_owned_by(o.id);
    o.repl
        .transfer_ownership(&mut o.world, e, a.id)
        .expect("O owns e");
    let handoff = o.collect_for(a.id);
    a.deliver_events(o.id, &handoff.events);
    assert_eq!(a.owner(proxy), a.id, "A adopted e");

    // A hands the ADOPTED e to q, who never saw it.
    a.track(q.id);
    a.repl
        .transfer_ownership(&mut a.world, proxy, q.id)
        .expect("A owns e now");
    let outs = a.collect_all();
    if let Some(oq) = outbox_for(&outs, q.id) {
        assert!(
            spawn_ids(&oq.events).is_empty(),
            "no foreign-namespace Spawn emitted for an adopted handoff"
        );
        q.deliver_all(a.id, oq);
    }
    assert_eq!(
        q.entity_count(),
        0,
        "the non-witnessing new owner builds no proxy — the documented gap"
    );
}

/// E5 (auditor) — a chained handoff O→A→q where q WITNESSED e via O completes:
/// A cannot Spawn the foreign-namespace (adopted) entity, but the bare
/// OwnershipTransfer alone flips q's EXISTING proxy to q — even though A never
/// AOI-entered e for q (the `p == q, !known` branch). Guards the fix that emits
/// the Transfer unconditionally.
#[test]
fn adopted_handoff_to_witnessing_peer_completes() {
    let mut o = TestPeer::new(1);
    let mut a = TestPeer::new(2);
    let mut q = TestPeer::new(3);
    let e = o.spawn(1.0, 1.0, 0.0, 0.0);

    // O replicates e to BOTH A and q (both hold a proxy owned by O).
    o.track(a.id);
    o.track(q.id);
    let outs = o.collect_all();
    a.deliver_all(o.id, outbox_for(&outs, a.id).unwrap());
    q.deliver_all(o.id, outbox_for(&outs, q.id).unwrap());
    let proxy_a = a.entity_owned_by(o.id);
    let proxy_q = q.entity_owned_by(o.id);

    // O hands e→A. O tells BOTH witnesses. A adopts; q's proxy now reads owner A.
    o.repl
        .transfer_ownership(&mut o.world, e, a.id)
        .expect("O owns e");
    let outs = o.collect_all();
    a.deliver_events(o.id, &outbox_for(&outs, a.id).unwrap().events);
    q.deliver_events(o.id, &outbox_for(&outs, q.id).unwrap().events);
    assert_eq!(a.owner(proxy_a), a.id, "A adopted e");
    assert_eq!(q.owner(proxy_q), a.id, "q's proxy now reads owner A");

    // A hands the adopted e→q. A never AOI-entered e for q ⇒ the p==q, !known
    // branch. The bare Transfer must flip q's existing proxy to q.
    a.track(q.id);
    a.repl
        .transfer_ownership(&mut a.world, proxy_a, q.id)
        .expect("A owns e now");
    let outs = a.collect_all();
    if let Some(oq) = outbox_for(&outs, q.id) {
        assert!(
            spawn_ids(&oq.events).is_empty(),
            "no foreign-namespace Spawn (A can't mint in O's namespace)"
        );
        q.deliver_events(a.id, &oq.events);
    }
    assert_eq!(
        q.owner(proxy_q),
        q.id,
        "the bare Transfer completes the handoff to the witnessing new owner"
    );
}

// ═══════════ Stage A — interpolate-others (ADR-0022) ═══════════

/// Buffer two snapshots on B's proxy of A's entity `e`: (tick `t0` @ x=`x0`)
/// then (tick `t1` @ x=`x1`), y=0. Returns B's proxy entity. Uses the real
/// sender flow (A stamps its `Tick`, the delta ships the moved position).
fn two_snapshots(
    a: &mut TestPeer,
    b: &mut TestPeer,
    e: Entity,
    t0: u64,
    x0: f32,
    t1: u64,
    x1: f32,
) -> Entity {
    a.track(b.id);
    a.world.get_mut::<Position>(e).unwrap().x = x0;
    a.set_tick(t0);
    b.deliver_all(a.id, &a.collect_for(b.id)); // Spawn + snapshot @ t0
    a.world.get_mut::<Position>(e).unwrap().x = x1;
    a.set_tick(t1);
    b.deliver_all(a.id, &a.collect_for(b.id)); // snapshot @ t1
    b.entity_owned_by(a.id)
}

/// SA1 — the render position lerps between the two snapshots bracketing
/// `RenderTick − DELAY`.
#[test]
fn interp_lerps_between_two_snapshots() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    let proxy = two_snapshots(&mut a, &mut b, e, 100, 0.0, 120, 20.0);
    // Sample at tick 110 (midpoint of 100..120) ⇒ lerp 0.5 ⇒ x = 10.
    b.set_render_tick(110.0 + INTERP_DELAY_TICKS);
    b.run_render();
    assert!(approx(b.render_pos(proxy).x, 10.0), "midpoint lerp");
}

/// SA1b — a 3+-snapshot buffer: the interior bracket search picks the correct
/// segment (guards the loop that a 2-point buffer never exercises).
#[test]
fn interp_lerps_across_three_snapshots() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    a.track(b.id);
    for (t, x) in [(100u64, 0.0f32), (110, 10.0), (120, 20.0)] {
        a.world.get_mut::<Position>(e).unwrap().x = x;
        a.set_tick(t);
        b.deliver_all(a.id, &a.collect_for(b.id));
    }
    let proxy = b.entity_owned_by(a.id);
    // First interior segment (100..110): target 105 ⇒ x = 5.
    b.set_render_tick(105.0 + INTERP_DELAY_TICKS);
    b.run_render();
    assert!(approx(b.render_pos(proxy).x, 5.0), "first-segment lerp");
    // Second interior segment (110..120): target 115 ⇒ x = 15.
    b.set_render_tick(115.0 + INTERP_DELAY_TICKS);
    b.run_render();
    assert!(approx(b.render_pos(proxy).x, 15.0), "second-segment lerp");
}

/// SA2 — with `RenderTick` at the newest snapshot's tick, the entity renders
/// DELAY behind the newest, not at it.
#[test]
fn interp_renders_at_fixed_delay() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    let proxy = two_snapshots(&mut a, &mut b, e, 100, 0.0, 120, 20.0);
    b.set_render_tick(120.0); // target = 120 − 6.4 = 113.6 ⇒ x = 13.6
    b.run_render();
    let x = b.render_pos(proxy).x;
    assert!(approx(x, 13.6), "renders at the delay (got {x})");
    assert!(x < 20.0 - TOL, "must lag behind the newest snapshot");
}

/// SA3 — sampling PAST the newest snapshot clamps to the newest — never
/// extrapolates (a receiver must not re-simulate others).
#[test]
fn interp_underrun_clamps_no_extrapolation() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    let proxy = two_snapshots(&mut a, &mut b, e, 100, 0.0, 120, 20.0);
    b.set_render_tick(1000.0); // target far past the newest tick
    b.run_render();
    assert!(
        approx(b.render_pos(proxy).x, 20.0),
        "clamp to the newest, never extrapolate"
    );
}

/// SA4 — sampling BEFORE the oldest snapshot clamps to the oldest.
#[test]
fn interp_overrun_clamps_to_oldest() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    let proxy = two_snapshots(&mut a, &mut b, e, 100, 0.0, 120, 20.0);
    b.set_render_tick(0.0); // target = −6.4 < oldest tick 100
    b.run_render();
    assert!(approx(b.render_pos(proxy).x, 0.0), "clamp to the oldest");
}

/// SA5 ★ interpolation writes ONLY `RenderPos`; the authoritative `Position`
/// stays the last snapped value (bit-identical) — the invariant that receivers
/// never re-simulate others' entities.
#[test]
fn interp_does_not_touch_authoritative_position() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    let proxy = two_snapshots(&mut a, &mut b, e, 100, 0.0, 120, 20.0);
    let auth_before = b.pos(proxy); // last snapped = (20, 0)
    b.set_render_tick(120.0); // renders behind (13.6)
    b.run_render();
    let auth_after = b.pos(proxy);
    assert_eq!(
        auth_before.x.to_bits(),
        auth_after.x.to_bits(),
        "interpolation must not touch the authoritative Position"
    );
    assert_eq!(auth_before.y.to_bits(), auth_after.y.to_bits());
    assert!(
        (b.render_pos(proxy).x - auth_after.x).abs() > TOL,
        "RenderPos lags the authoritative Position"
    );
}

/// SA6 — the sender stamps its authoritative `Tick` into the snapshot (the
/// interpolation time axis).
#[test]
fn collect_all_stamps_tick() {
    let mut a = TestPeer::new(1);
    let b = PeerId(2);
    a.spawn(1.0, 1.0, 0.0, 0.0);
    a.set_tick(777);
    let out = a.collect_for(b);
    let msg = decode_state(out.state.as_ref().unwrap()).unwrap();
    assert_eq!(msg.tick, 777, "the sender stamps its authoritative tick");
}

/// SA7 — an OWNED entity renders at its authoritative (locally-simulated)
/// position: `RenderPos` tracks `Position`.
#[test]
fn owned_render_tracks_position() {
    let mut a = TestPeer::new(1);
    let e = a.spawn(3.0, 4.0, 2.0, 0.0);
    a.sim_tick(); // owned: Local arm integrates Position
    a.run_render(); // copy_owned_render: RenderPos = Position
    let auth = a.pos(e);
    let r = a.render_pos(e);
    assert!(
        approx(r.x, auth.x) && approx(r.y, auth.y),
        "owned render tracks the authoritative Position"
    );
    assert!(auth.x > 3.0, "owned entity moved under the local sim");
}

// ═══════════ Stage B — predict-own + input + reconciliation (ADR-0022) ═══════════

/// Build + deliver a manual snapshot to `c` from `s` for entity id (of server
/// entity `e`): seq, last_input, and (x,y). For the LWW / marker tests.
fn deliver_snapshot(
    c: &mut TestPeer,
    s: PeerId,
    e_id: NetEntityId,
    seq: u64,
    last_input: u64,
    x: f32,
    y: f32,
) {
    let bytes = protocol::encode_state(&protocol::StateMsg {
        version: protocol::WIRE_VERSION,
        seq,
        tick: 0,
        last_input,
        entries: vec![StateEntry {
            id: e_id,
            pos: Some(protocol::quantize_vec2(x, y)),
            vel: None,
        }],
    })
    .unwrap();
    c.deliver_state(s, &bytes);
}

/// SB1 — one input moves the render position IMMEDIATELY (no round-trip lag),
/// while the authoritative Position is untouched.
#[test]
fn predicted_avatar_has_no_input_lag() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (_e, proxy) = controlled_avatar(&mut s, &mut c, 0.0, 0.0);
    c.feed_input(proxy, 2.0, 0.0);
    c.run_render();
    assert!(
        c.render_pos(proxy).x > TOL,
        "input moves the render position immediately"
    );
    assert!(
        approx(c.pos(proxy).x, 0.0),
        "authoritative Position not yet advanced"
    );
}

/// SB2 — the predicted avatar LEADS the authority by the un-acked input window.
#[test]
fn predicted_avatar_leads_authority_by_input_window() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (e, proxy) = controlled_avatar(&mut s, &mut c, 0.0, 0.0);
    for _ in 0..4 {
        c.feed_input(proxy, 1.0, 0.0);
    }
    c.run_render();
    flush_inputs(&mut c, &mut s);
    for _ in 0..2 {
        s.sim_tick(); // server processes 2 of the 4
    }
    assert!(approx(s.pos(e).x, 1.0), "server processed 2 inputs");
    c.deliver_all(s.id, &s.collect_for(c.id)); // last_input = 2, pos 1.0
    c.run_render();
    let auth = c.pos(proxy).x; // 1.0
    let render = c.render_pos(proxy).x; // 1.0 + 2 un-acked * 0.5 = 2.0
    assert!(approx(auth, 1.0), "authority through 2 inputs");
    assert!(approx(render, 2.0), "render leads by the 2 un-acked inputs");
    assert!(render > auth + TOL, "prediction leads the authority");
}

/// SB3 — prediction writes ONLY RenderPos; authoritative Position AND Velocity
/// are untouched (the predicted avatar is Remote — the sender never emits it).
#[test]
fn prediction_writes_only_render_pos() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (_e, proxy) = controlled_avatar(&mut s, &mut c, 5.0, 7.0);
    let pos_before = c.pos(proxy);
    let vel_before = c.vel(proxy);
    for _ in 0..3 {
        c.feed_input(proxy, 2.0, -1.0);
    }
    c.run_render();
    assert_eq!(
        c.pos(proxy).x.to_bits(),
        pos_before.x.to_bits(),
        "prediction must not touch Position"
    );
    assert_eq!(c.pos(proxy).y.to_bits(), pos_before.y.to_bits());
    assert_eq!(
        c.vel(proxy).x.to_bits(),
        vel_before.x.to_bits(),
        "prediction must not touch Velocity"
    );
    assert_eq!(c.vel(proxy).y.to_bits(), vel_before.y.to_bits());
    assert!(
        (c.render_pos(proxy).x - pos_before.x).abs() > TOL,
        "RenderPos advanced"
    );
}

/// SB4 ★ reconciliation snaps to the authority + replays un-acked inputs and
/// CONVERGES: the re-anchor never moves a correct prediction (no oscillation),
/// and once all inputs are processed RenderPos == Position.
#[test]
fn reconcile_snaps_and_replays_converges() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (e, proxy) = controlled_avatar(&mut s, &mut c, 0.0, 0.0);

    for _ in 0..5 {
        c.feed_input(proxy, 1.0, 0.0);
    }
    c.run_render();
    assert!(
        approx(c.render_pos(proxy).x, 2.5),
        "predicted 5 steps ahead"
    );
    assert!(approx(c.pos(proxy).x, 0.0), "authority not advanced yet");
    flush_inputs(&mut c, &mut s);

    for _ in 0..3 {
        s.sim_tick(); // process 3 of 5
    }
    c.deliver_all(s.id, &s.collect_for(c.id)); // last_input 3, pos 1.5
    assert!(
        approx(c.pos(proxy).x, 1.5),
        "reconcile snapped Position to the authority"
    );
    c.run_render();
    assert!(
        approx(c.render_pos(proxy).x, 2.5),
        "re-anchored — prediction unchanged (no oscillation)"
    );

    for _ in 0..2 {
        s.sim_tick(); // process the rest
    }
    c.deliver_all(s.id, &s.collect_for(c.id)); // last_input 5
    c.run_render();
    assert!(
        approx(c.render_pos(proxy).x, 2.5) && approx(c.pos(proxy).x, 2.5),
        "converged: RenderPos == Position"
    );
    assert!(approx(s.pos(e).x, 2.5), "authority processed all 5");
}

/// SB5 — a CORRECT prediction reconciles with NO pop: the snapshot apply doesn't
/// move RenderPos (client math == server math).
#[test]
fn reconcile_no_oscillation_on_correct_prediction() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (_e, proxy) = controlled_avatar(&mut s, &mut c, 0.0, 0.0);
    for _ in 0..10 {
        c.feed_input(proxy, 1.0, 0.0);
        c.run_render();
        let before = c.render_pos(proxy).x;
        flush_inputs(&mut c, &mut s);
        s.sim_tick();
        c.deliver_all(s.id, &s.collect_for(c.id)); // reconcile
        c.run_render();
        let after = c.render_pos(proxy).x;
        assert!(
            approx(before, after),
            "correct prediction reconciles with no pop: {before} vs {after}"
        );
    }
}

/// SB6 — the server applies each input seq EXACTLY once (a duplicate is skipped
/// without consuming a tick); `last_input` is monotonic; the client converges.
#[test]
fn reconcile_bounded_under_input_reorder() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (e, proxy) = controlled_avatar(&mut s, &mut c, 0.0, 0.0);
    for _ in 0..3 {
        c.feed_input(proxy, 1.0, 0.0);
    }
    let inputs = c.repl.drain_inputs(&mut c.world); // seq 1, 2, 3
    assert_eq!(inputs.len(), 3);
    // Deliver I1, I1(dup), I2, I3 — the dup must be ignored (seq <= last).
    for &i in &[0usize, 0, 1, 2] {
        s.repl.apply_events(&mut s.world, c.id, &inputs[i].1);
    }
    // 3 sim ticks process exactly the 3 DISTINCT inputs (the dup costs no tick):
    // pos = 3 * 0.5 = 1.5. A double-apply would have left I3 unprocessed here.
    for _ in 0..3 {
        s.sim_tick();
    }
    assert!(
        approx(s.pos(e).x, 1.5),
        "each distinct input applied exactly once"
    );
    c.deliver_all(s.id, &s.collect_for(c.id)); // last_input 3
    c.run_render();
    assert!(
        approx(c.pos(proxy).x, 1.5) && approx(c.render_pos(proxy).x, 1.5),
        "converged after the duplicate"
    );
}

/// SB7 — a stale (older-seq) snapshot with a smaller last_input is LWW-dropped
/// and does NOT un-prune the input history or overwrite the Position.
#[test]
fn stale_snapshot_marker_monotonic() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (e, proxy) = controlled_avatar(&mut s, &mut c, 0.0, 0.0);
    let id = net_id(s.id, e);
    for _ in 0..3 {
        c.feed_input(proxy, 1.0, 0.0);
    }
    // Fresh snapshot: seq 10, last_input 2, pos 1.0 ⇒ prunes I1,I2, history=[I3].
    deliver_snapshot(&mut c, s.id, id, 10, 2, 1.0, 0.0);
    // Stale snapshot: seq 5 (< 10), last_input 0, pos 99 ⇒ LWW-dropped.
    deliver_snapshot(&mut c, s.id, id, 5, 0, 99.0, 0.0);
    c.run_render();
    assert!(
        approx(c.pos(proxy).x, 1.0),
        "the stale snapshot's Position was LWW-dropped"
    );
    assert!(
        approx(c.render_pos(proxy).x, 1.5),
        "history still [I3] (1.0 + 1 input) — the stale marker did not un-prune"
    );
}

/// SB8 — the client NEVER emits authored state/events for its predicted avatar
/// (it is Remote/server-owned ⇒ the authority gate excludes it); only inputs
/// flow, via the input channel.
#[test]
fn client_never_sends_state_for_predicted_avatar() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (_e, proxy) = controlled_avatar(&mut s, &mut c, 0.0, 0.0);
    c.feed_input(proxy, 1.0, 0.0);
    c.run_render();
    c.track(s.id);
    let outs = c.collect_all();
    assert!(
        outs.is_empty(),
        "the client authors no state/events for its predicted avatar"
    );
    assert!(
        !c.repl.drain_inputs(&mut c.world).is_empty(),
        "inputs flow via the input channel instead"
    );
}

/// SB9 ★ THE headline reconciliation: correcting a WRONG prediction. When the
/// authority's Position DIVERGES from the client's prediction, the snapshot
/// re-anchors RenderPos to server truth and replays the un-acked inputs from
/// there — the correction the whole stack exists for (auditor F6).
#[test]
fn reconcile_corrects_a_wrong_prediction() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (e, proxy) = controlled_avatar(&mut s, &mut c, 0.0, 0.0);
    let id = net_id(s.id, e);
    for _ in 0..3 {
        c.feed_input(proxy, 1.0, 0.0);
    }
    c.run_render();
    assert!(approx(c.render_pos(proxy).x, 1.5), "client predicted ahead");
    // The authority DIVERGES: it processed input 1 but the avatar is at 5.0
    // (not the predicted 0.5) — e.g. a wall / server push. seq 10 > last_seq 1.
    deliver_snapshot(&mut c, s.id, id, 10, 1, 5.0, 0.0);
    c.run_render();
    assert!(
        approx(c.pos(proxy).x, 5.0),
        "Position snapped to the divergent authority"
    );
    assert!(
        approx(c.render_pos(proxy).x, 6.0),
        "RenderPos corrected to server (5.0) + the 2 un-acked inputs (1.0)"
    );
}

/// SB10 — on UNDERRUN (the client's inputs stop arriving) the server zeros
/// velocity and stops, rather than drifting on a held velocity — matching the
/// client's replay (no input ⇒ no displacement), so no forward pop (auditor F3).
#[test]
fn underrun_stops_server_matching_client() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (e, proxy) = controlled_avatar(&mut s, &mut c, 0.0, 0.0);
    for _ in 0..2 {
        c.feed_input(proxy, 1.0, 0.0);
    }
    c.run_render();
    flush_inputs(&mut c, &mut s);
    // Process the 2 inputs, then 3 MORE ticks with an empty queue (underrun).
    for _ in 0..5 {
        s.sim_tick();
    }
    assert!(
        approx(s.pos(e).x, 1.0),
        "underrun stops the server (no held-velocity drift past the inputs)"
    );
    c.deliver_all(s.id, &s.collect_for(c.id)); // last_input 2, pos 1.0
    c.run_render();
    assert!(
        approx(c.pos(proxy).x, 1.0) && approx(c.render_pos(proxy).x, 1.0),
        "client converged with no forward pop"
    );
}
