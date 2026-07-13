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
    Contacts, Controlled, ControlledBy, INTERP_DELAY_TICKS, InputHistory, Intent, Interactable,
    InterpBuffer, Owner, Position, RenderPos, RenderTick, Tick, Velocity, apply_input,
    copy_owned_render, insert_sim, interpolate, predict, record_input, reset_render_role,
    resolve_interactions, simulate, spawn_owned,
};
use protocol::{
    EventMsg, NetEntityId, NetEvent, OwnerSeq, PeerId, StateEntry, WIRE_VERSION, decode_event,
    decode_state, encode_event, quantize_vec2,
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
        // The render step (ADR-0022): reset_render_role first maintains the
        // per-entity role on a handoff/control change (flush/seed from the
        // authoritative Position), THEN interpolated remotes lerp their buffer,
        // predicted (controlled) entities replay their input history, and owned
        // entities copy Position — all write only RenderPos; ordered so predict
        // wins over a stale InterpBuffer and Local authority wins last.
        let mut render = Schedule::default();
        render.add_systems((reset_render_role, interpolate, predict, copy_owned_render).chain());
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

    /// Set a peer's AOI with a hysteresis band: enter at `dist ≤ r_inner`, exit
    /// at `dist > r_outer` (ADR-0023 b — no churn in the band).
    fn set_aoi_hysteresis(&mut self, peer: PeerId, center: (f32, f32), r_inner: f32, r_outer: f32) {
        self.repl.set_aoi_hysteresis(peer, center, r_inner, r_outer);
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

/// Drive one full anti-entropy resync round (ADR-0024) from `owner` (the current
/// authority) to `receiver` (a diverged peer): owner digest → receiver request →
/// owner `ResyncSpawn`. Delivers only the messages addressed to the other party.
fn flush_resync(owner: &mut TestPeer, receiver: &mut TestPeer) {
    for (target, bytes) in owner.repl.collect_resync(&mut owner.world) {
        if target == receiver.id {
            receiver
                .repl
                .apply_events(&mut receiver.world, owner.id, &bytes);
        }
    }
    for (target, bytes) in receiver.repl.drain_resync_requests() {
        if target == owner.id {
            owner
                .repl
                .apply_events(&mut owner.world, receiver.id, &bytes);
        }
    }
    for (target, bytes) in owner.repl.drain_resync_responses(&mut owner.world) {
        if target == receiver.id {
            receiver
                .repl
                .apply_events(&mut receiver.world, owner.id, &bytes);
        }
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
            NetEvent::OwnershipTransfer { id, new_owner, .. } if id == original_id && new_owner == PeerId(2)
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

/// B6 — HYSTERESIS: an entity oscillating inside the band (`r_inner < dist ≤
/// r_outer`) after entering does NOT churn Spawn/Despawn — the anti-flicker
/// payoff (ADR-0023 b). Single-radius today would Despawn/Spawn every crossing.
#[test]
fn hysteresis_in_band_no_spawn_no_despawn() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi_hysteresis(b.id, (0.0, 0.0), 5.0, 10.0);
    let e = a.spawn(3.0, 0.0, 0.0, 0.0); // dist 3 ≤ r_inner → enters
    let out = a.collect_for(b.id);
    assert_eq!(spawn_ids(&out.events).len(), 1, "enters at ≤ r_inner");
    b.deliver_all(a.id, &out);

    // Oscillate strictly inside the band (5 < dist ≤ 10) for several ticks.
    for x in [7.0f32, 6.0, 9.0, 7.0, 10.0, 6.0] {
        a.world.get_mut::<Position>(e).unwrap().x = x;
        let out = a.collect_for(b.id);
        assert!(
            spawn_ids(&out.events).is_empty(),
            "no re-Spawn oscillating in the band (dist {x})"
        );
        assert_eq!(
            despawn_count(&out),
            0,
            "no Despawn oscillating in the band (dist {x})"
        );
        b.deliver_all(a.id, &out);
    }
    assert_eq!(
        b.entity_count(),
        1,
        "the proxy is retained through the band"
    );
}

/// B7 — HYSTERESIS: a band dip does NOT drop the delta baseline (no full-mask
/// re-send, no re-Spawn). Contrast B3, where a true AOI-exit re-baselines.
#[test]
fn hysteresis_baseline_survives_band_dip() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi_hysteresis(b.id, (0.0, 0.0), 5.0, 10.0);
    let e = a.spawn(3.0, 0.0, 0.0, 0.0); // enters (≤ r_inner)
    b.deliver_all(a.id, &a.collect_for(b.id));
    flush_acks(&mut b, &mut a); // confirmed → quiet

    // Dip into the band: the value changed so state re-sends the delta, but there
    // is NO re-Spawn and NO Despawn (still known, never exited) and the baseline
    // is NOT dropped.
    a.world.get_mut::<Position>(e).unwrap().x = 7.0; // band
    let out = a.collect_for(b.id);
    assert!(
        spawn_ids(&out.events).is_empty(),
        "a band dip must NOT re-Spawn (the entity never exited)"
    );
    assert_eq!(
        despawn_count(&out),
        0,
        "a band dip must NOT Despawn — guards EXIT wired to the OUTER radius"
    );
    b.deliver_all(a.id, &out);
    flush_acks(&mut b, &mut a);

    // Stationary in the band + confirmed ⇒ quiet. A dropped baseline would have
    // forced a full-mask re-send here (as a true exit+re-enter does — B3).
    assert!(
        a.collect_for(b.id).state.is_none(),
        "confirmed band entity is quiet — the baseline survived the dip"
    );
}

/// B8 — HYSTERESIS full loop + the band read-cheat: enter ONLY at `≤ r_inner`,
/// exit ONLY at `> r_outer`, and an entity that appears in the band but was
/// NEVER inside `r_inner` is FULLY withheld (no Spawn, no state — the D1
/// read-cheat, in the band).
#[test]
fn hysteresis_enters_at_inner_exits_at_outer() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi_hysteresis(b.id, (0.0, 0.0), 5.0, 10.0);
    let e = a.spawn(20.0, 0.0, 0.0, 0.0); // > r_outer
    let out = a.collect_for(b.id);
    assert!(
        out.state.is_none() && spawn_ids(&out.events).is_empty(),
        "far entity: fully withheld"
    );

    // In the band but never entered r_inner ⇒ fully withheld (read-cheat).
    a.world.get_mut::<Position>(e).unwrap().x = 7.0; // band, unknown
    let out = a.collect_for(b.id);
    assert!(
        out.state.is_none() && spawn_ids(&out.events).is_empty(),
        "a never-entered band entity leaks NOTHING (existence gating in the band)"
    );

    // Cross ≤ r_inner ⇒ enter.
    a.world.get_mut::<Position>(e).unwrap().x = 3.0; // ≤ r_inner
    let out = a.collect_for(b.id);
    assert_eq!(spawn_ids(&out.events).len(), 1, "enters at ≤ r_inner");
    b.deliver_all(a.id, &out);

    // Back into the band ⇒ stays known (no despawn); state CONTINUES for the
    // changed value (guards STATE wired to the OUTER radius — an inner-wired
    // state pass would starve the still-spawned band proxy).
    a.world.get_mut::<Position>(e).unwrap().x = 7.0; // band, now known
    let out = a.collect_for(b.id);
    assert_eq!(
        despawn_count(&out),
        0,
        "known band entity stays (no exit at ≤ r_outer)"
    );
    assert!(
        out.state.is_some(),
        "a known band entity keeps receiving state (STATE uses r_outer, not r_inner)"
    );
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 1);
    let proxy = b.entity_owned_by(a.id);
    assert!(
        approx(b.pos(proxy).x, 7.0),
        "the band proxy tracks the authoritative position (not frozen)"
    );

    // Past r_outer ⇒ exit.
    a.world.get_mut::<Position>(e).unwrap().x = 12.0; // > r_outer
    let out = a.collect_for(b.id);
    assert_eq!(despawn_count(&out), 1, "exits only past r_outer");
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 0);
}

/// B9 — backward-compat: `set_aoi` (single radius) is the degenerate band
/// (`r_inner == r_outer == radius`) — enter at `≤ r`, exit at `> r`, exactly the
/// pre-hysteresis single-boundary behavior (what keeps Groups A–H green).
#[test]
fn degenerate_band_equals_single_radius() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.track(b.id);
    a.set_aoi(b.id, (0.0, 0.0), 5.0); // single radius ⇒ inner == outer == 5
    let e = a.spawn(3.0, 0.0, 0.0, 0.0); // ≤ 5 → in
    let out = a.collect_for(b.id);
    assert_eq!(spawn_ids(&out.events).len(), 1, "enters at ≤ r");
    b.deliver_all(a.id, &out);

    // dist 7 > 5: with no band it exits IMMEDIATELY (unlike hysteresis, where
    // 5 < 7 ≤ 10 would linger).
    a.world.get_mut::<Position>(e).unwrap().x = 7.0;
    let out = a.collect_for(b.id);
    assert_eq!(
        despawn_count(&out),
        1,
        "exits at > r (no band to linger in)"
    );
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 0);
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

// ═══════════ Stage C — handoff interplay (ADR-0022) ═══════════

/// SC1 — the flagged failure mode: on ADOPTION (an interpolated remote flips to
/// Local) the avatar renders at the AUTHORITATIVE Position, NOT the ~DELAY-behind
/// interpolated render — no jump to the past — and the interp buffer is dropped.
/// (The render lands at Position via BOTH `reset_render_role`'s seed AND
/// `copy_owned_render` — the "Local wins last" co-guarantor; SC5 exercises the
/// role reset's unmasked half — the input-history clear.)
#[test]
fn adopt_seeds_render_from_authoritative_not_interp() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    let proxy = two_snapshots(&mut a, &mut b, e, 100, 0.0, 120, 20.0);
    b.set_render_tick(120.0); // renders behind: RenderPos ≈ 13.6
    b.run_render();
    let interp_render = b.render_pos(proxy).x;
    let auth = b.pos(proxy).x; // 20.0 (last snapped)
    assert!(
        interp_render < auth - TOL,
        "interp render lags the authoritative Position"
    );

    // B ADOPTS e (A transfers e → B).
    a.repl
        .transfer_ownership(&mut a.world, e, b.id)
        .expect("A owns e");
    let out = a.collect_for(b.id);
    b.deliver_events(a.id, &out.events);
    assert_eq!(b.owner(proxy), b.id, "B adopted e");
    b.run_render(); // reset_render_role: → Owned, seed from Position
    assert!(
        approx(b.render_pos(proxy).x, auth),
        "adopt seeds RenderPos from the authoritative Position, not the stale interp ({interp_render})"
    );
    assert!(
        b.world.get::<InterpBuffer>(proxy).is_none(),
        "the interp buffer is dropped on adoption"
    );
}

/// SC2 — relinquishing an owned, NON-controlled entity to a remote peer turns it
/// Interpolated: an `InterpBuffer` is attached (it was absent while owned).
#[test]
fn relinquish_noncontrolled_interpolates() {
    let mut a = TestPeer::new(1);
    let e = a.spawn(3.0, 3.0, 0.0, 0.0);
    a.run_render(); // reset → Owned
    assert!(
        a.world.get::<InterpBuffer>(e).is_none(),
        "owned entity has no interp buffer"
    );
    a.repl
        .transfer_ownership(&mut a.world, e, PeerId(9))
        .expect("A owns e");
    a.run_render(); // reset → Interpolated (now Remote, not Controlled)
    assert_eq!(a.owner(e), PeerId(9));
    assert!(
        a.world.get::<InterpBuffer>(e).is_some(),
        "a relinquished remote entity gets an interp buffer"
    );
}

/// SC3 — relinquishing an owned entity while KEEPING control (Mode-2→3 style:
/// hand authority to the server, keep driving it) turns it Predicted: an
/// `InputHistory` is ensured and no interp buffer.
#[test]
fn relinquish_keeping_control_becomes_predicted() {
    let mut a = TestPeer::new(1);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    a.world.entity_mut(e).insert(Controlled { next_seq: 1 });
    a.run_render(); // reset → Owned (controlled but Local)
    a.repl
        .transfer_ownership(&mut a.world, e, PeerId(9))
        .expect("A owns e");
    a.run_render(); // reset → Predicted (Remote + Controlled)
    assert_eq!(a.owner(e), PeerId(9));
    assert!(
        a.world.get::<InputHistory>(e).is_some(),
        "a predicted avatar has an input history"
    );
    assert!(
        a.world.get::<InterpBuffer>(e).is_none(),
        "a predicted avatar does not interpolate"
    );
}

/// SC4 — an observer flushes its interp buffer on ANY authority change (A→B):
/// the buffered snapshots came from A; lerping across the A→B source
/// discontinuity would glide through a wrong intermediate.
#[test]
fn interp_buffer_flushed_on_owner_change() {
    let mut a = TestPeer::new(1);
    let mut obs = TestPeer::new(3);
    let b = PeerId(2);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    let proxy = two_snapshots(&mut a, &mut obs, e, 100, 0.0, 120, 20.0);
    assert!(
        !obs.world.get::<InterpBuffer>(proxy).unwrap().0.is_empty(),
        "the buffer holds A's snapshots"
    );
    // A transfers e → B; obs (a third peer) receives the transfer.
    a.repl
        .transfer_ownership(&mut a.world, e, b)
        .expect("A owns e");
    let out = a.collect_for(obs.id);
    obs.deliver_events(a.id, &out.events);
    assert_eq!(obs.owner(proxy), b, "obs proxy owner flipped A→B");
    assert!(
        obs.world
            .get::<InterpBuffer>(proxy)
            .is_none_or(|buf| buf.0.is_empty()),
        "the interp buffer is flushed on the A→B source change"
    );
}

/// SC5 — adopting a PREDICTED avatar (Remote+Controlled, mid-prediction with
/// un-acked inputs) to Local CLEARS its InputHistory: we are authoritative now,
/// so the old authority's un-acked inputs must NOT replay against the new anchor.
/// Asserts directly on InputHistory (a path `copy_owned_render` does not mask),
/// and that the transition fires exactly once (idempotent second render).
#[test]
fn adopt_predicted_avatar_clears_input_history() {
    let mut s = TestPeer::new(1);
    let mut c = TestPeer::new(2);
    let (e, proxy) = controlled_avatar(&mut s, &mut c, 0.0, 0.0);
    for _ in 0..3 {
        c.feed_input(proxy, 1.0, 0.0);
    }
    c.run_render();
    assert!(
        !c.world.get::<InputHistory>(proxy).unwrap().0.is_empty(),
        "the predicted avatar has un-acked inputs"
    );

    // S hands the avatar's authority to C — C adopts it.
    s.repl
        .transfer_ownership(&mut s.world, e, c.id)
        .expect("S owns e");
    let out = s.collect_for(c.id);
    c.deliver_events(s.id, &out.events);
    assert_eq!(c.owner(proxy), c.id, "C adopted the avatar");
    c.run_render(); // reset_render_role: → Owned clears InputHistory
    assert!(
        c.world
            .get::<InputHistory>(proxy)
            .is_none_or(|h| h.0.is_empty()),
        "adopt clears the input history — stale inputs must not replay under new authority"
    );
    // Idempotent: a second render with no ownership change is a no-op.
    c.run_render();
    assert!(
        c.world
            .get::<InputHistory>(proxy)
            .is_none_or(|h| h.0.is_empty()),
        "the role transition fired once — the second render doesn't re-run it"
    );
}

// ═══════════ Group Q — shared per-tick snapshot (quantization hoist, ADR-0021 (a)) ═══════════

/// Q1 — the quantized value is PEER-INVARIANT: two unbounded peers seeing the
/// SAME owned entity in one `collect_all` receive an IDENTICAL `StateEntry`
/// (id + quantized pos/vel). This is the correctness precondition for hoisting
/// quantization into the once-per-tick snapshot (compute `QVec2` once per owned
/// entity, not per (peer,entity)) — it holds before AND after the refactor (a
/// characterization guard), and the byte-exact battery (T35, A/B/C, the SA/SB
/// interp suites) proves the wire output itself is unchanged.
#[test]
fn hoist_quantized_value_is_peer_invariant() {
    let mut a = TestPeer::new(1);
    let x = TestPeer::new(2);
    let y = TestPeer::new(3);
    a.track(x.id);
    a.track(y.id);
    // Non-integer coords so quantization is actually exercised (an integer would
    // quantize to itself trivially and hide a per-peer recompute divergence).
    a.spawn(3.25, -7.5, 1.5, -0.25);

    let outs = a.collect_all();
    let ex = state_entries(outbox_for(&outs, x.id).expect("X outbox"));
    let ey = state_entries(outbox_for(&outs, y.id).expect("Y outbox"));
    assert_eq!(ex.len(), 1, "X gets the one entity");
    assert_eq!(ey.len(), 1, "Y gets the one entity");
    assert_eq!(
        ex[0].mask(),
        0b11,
        "full-mask first send — invariance must cover BOTH quantized components"
    );
    assert_eq!(
        ex[0], ey[0],
        "the quantized StateEntry must be identical across peers (peer-invariant \
         quantization — the hoist precondition)"
    );
    // Pin the ACTUAL quantized values to the spawn coords — an independent guard
    // (a symmetric hoist bug like swapping qpos/qvel would keep the two peers
    // equal above but map the wrong field here).
    assert_eq!(
        ex[0].pos,
        Some(quantize_vec2(3.25, -7.5)),
        "position quantized from the spawn coords, not the velocity"
    );
    assert_eq!(
        ex[0].vel,
        Some(quantize_vec2(1.5, -0.25)),
        "velocity quantized from the spawn coords, not the position"
    );
}

// ═══════════════════════ Group R — handoff depth (ADR-0024) ═══════════════════════

/// R-HB1 — hand-back A→B→A restores the ORIGINAL owner. A's original entity
/// (spawner=A) becomes a B-owned proxy A must apply remote state to, then on the
/// hand-back A re-adopts its own entity (resolved via the persistent id-map,
/// lib.rs:848-878) and RE-BASELINES full-mask (the send_state drop on transfer-
/// away, lib.rs:1108-1114).
#[test]
fn handback_a_b_a_restores_original_owner() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e_a = a.spawn(0.0, 0.0, 2.0, 0.0);

    let out = a.collect_for(b.id);
    let original_id = spawn_ids(&out.events)[0];
    b.deliver_all(a.id, &out);
    let proxy = b.entity_owned_by(a.id);
    flush_acks(&mut b, &mut a);

    // A→B: A stops owning + collecting e_a; B adopts and simulates it.
    a.repl
        .transfer_ownership(&mut a.world, e_a, b.id)
        .expect("A owns e_a");
    assert_eq!(a.owner(e_a), b.id, "A's Owner flips immediately");
    let t = a.collect_for(b.id);
    assert!(
        state_entries(&t).is_empty(),
        "A collects no state for a transferred entity"
    );
    b.deliver_events(a.id, &t.events);
    assert_eq!(b.owner(proxy), b.id, "B adopts");
    b.sim_tick(); // B moves the adopted entity

    // B replicates back to A: A applies B's state to its OWN original entity
    // (by_id → e_a, owner==from==B). The load-bearing hand-back subtlety.
    let back = b.collect_for(a.id);
    a.deliver_all(b.id, &back);
    assert_eq!(a.owner(e_a), b.id, "A still sees e_a as B-owned");
    assert!(
        approx(a.pos(e_a).x, b.pos(proxy).x),
        "A applies the new owner's state to its own original entity"
    );

    // Hand-back B→A: A re-adopts e_a.
    b.repl
        .transfer_ownership(&mut b.world, proxy, a.id)
        .expect("B owns the proxy");
    let t2 = b.collect_for(a.id);
    a.deliver_events(b.id, &t2.events);
    assert_eq!(a.owner(e_a), a.id, "A re-adopts its original entity");

    // A resumes simulating and RE-BASELINES full-mask (proves the send_state drop).
    let frozen = a.pos(e_a);
    a.sim_tick();
    assert!(
        a.pos(e_a).x > frozen.x,
        "A re-simulates the re-adopted entity"
    );
    let re = a.collect_for(b.id);
    let entries = state_entries(&re);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].id, original_id,
        "identity survives the round trip"
    );
    assert_eq!(entries[0].mask(), 0b11, "re-adopt re-baselines full-mask");

    // B applies the round-trip without duplicating the proxy (idempotent re-Spawn).
    b.deliver_all(a.id, &re);
    assert_eq!(b.owner(proxy), a.id, "B's proxy tracks the hand-back to A");
    assert_eq!(b.entity_count(), 1, "no duplicate proxy");
    assert!(approx(b.pos(proxy).x, a.pos(e_a).x));
}

/// R-HB2 — a witnessing observer D tracks the full A→B→A ownership round trip.
/// TRAP: the intermediate owner B must replicate to D BETWEEN adopting and
/// handing back, else e ∉ B.known[D] and B's hand-back Transfer never reaches D
/// (that would be an R6-class freeze, not this happy path).
#[test]
fn handback_observer_tracks_ownership_roundtrip() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut d = TestPeer::new(3);
    let e_a = a.spawn(0.0, 0.0, 2.0, 0.0);
    a.track(b.id);
    a.track(d.id);
    let outs = a.collect_all();
    b.deliver_all(a.id, outbox_for(&outs, b.id).unwrap());
    d.deliver_all(a.id, outbox_for(&outs, d.id).unwrap());
    let proxy_b = b.entity_owned_by(a.id);
    let proxy_d = d.entity_owned_by(a.id);

    // A→B: tell BOTH B and D.
    a.repl
        .transfer_ownership(&mut a.world, e_a, b.id)
        .expect("A owns e_a");
    let outs = a.collect_all();
    b.deliver_events(a.id, &outbox_for(&outs, b.id).unwrap().events);
    d.deliver_events(a.id, &outbox_for(&outs, d.id).unwrap().events);
    assert_eq!(d.owner(proxy_d), b.id, "observer sees A→B");

    // TRAP: B replicates to D so e enters B.known[D] before the hand-back.
    b.track(a.id);
    b.track(d.id);
    b.sim_tick();
    d.deliver_all(b.id, &b.collect_for(d.id));

    // Hand-back B→A: tell both A and D.
    b.repl
        .transfer_ownership(&mut b.world, proxy_b, a.id)
        .expect("B owns the proxy");
    let outs = b.collect_all();
    a.deliver_events(b.id, &outbox_for(&outs, a.id).unwrap().events);
    d.deliver_events(b.id, &outbox_for(&outs, d.id).unwrap().events);
    assert_eq!(
        d.owner(proxy_d),
        a.id,
        "observer sees the full round trip B→A"
    );
    assert_eq!(d.entity_count(), 1, "no proxy churn");
}

/// R-RT1 — repeated transfers A→B→A→B: the NetEntityId is stable across every
/// hop and a witnessing observer D's proxy owner tracks each one. (Each hand-off
/// needs the current owner to replicate to D first — the known[D] trap.)
#[test]
fn repeated_transfers_a_b_a_b_identity_stable() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut d = TestPeer::new(3);
    let e = a.spawn(0.0, 0.0, 2.0, 0.0);
    let original_id = net_id(a.id, e);
    a.track(b.id);
    a.track(d.id);
    let outs = a.collect_all();
    b.deliver_all(a.id, outbox_for(&outs, b.id).unwrap());
    d.deliver_all(a.id, outbox_for(&outs, d.id).unwrap());
    let proxy_b = b.entity_owned_by(a.id);
    let proxy_d = d.entity_owned_by(a.id);
    b.track(a.id);
    b.track(d.id);

    // A→B: tell B and D.
    a.repl
        .transfer_ownership(&mut a.world, e, b.id)
        .expect("A→B");
    let outs = a.collect_all();
    b.deliver_events(a.id, &outbox_for(&outs, b.id).unwrap().events);
    d.deliver_events(a.id, &outbox_for(&outs, d.id).unwrap().events);
    assert_eq!(d.owner(proxy_d), b.id, "D tracks A→B");
    b.sim_tick();
    d.deliver_all(b.id, &b.collect_for(d.id)); // B witnesses D before handing back

    // B→A: tell A and D.
    b.repl
        .transfer_ownership(&mut b.world, proxy_b, a.id)
        .expect("B→A");
    let outs = b.collect_all();
    a.deliver_events(b.id, &outbox_for(&outs, a.id).unwrap().events);
    d.deliver_events(b.id, &outbox_for(&outs, d.id).unwrap().events);
    assert_eq!(d.owner(proxy_d), a.id, "D tracks B→A");
    a.sim_tick();
    d.deliver_all(a.id, &a.collect_for(d.id)); // A re-witnesses D (known[D] dropped on A→B)

    // A→B again: tell B and D.
    a.repl
        .transfer_ownership(&mut a.world, e, b.id)
        .expect("A→B #2");
    let outs = a.collect_all();
    b.deliver_events(a.id, &outbox_for(&outs, b.id).unwrap().events);
    d.deliver_events(a.id, &outbox_for(&outs, d.id).unwrap().events);
    assert_eq!(d.owner(proxy_d), b.id, "D tracks the second A→B");

    // The wire carries the STABLE original id at the final hop (D's owner-tracking
    // above already depends on this resolving; asserted explicitly on the wire).
    assert!(
        outbox_for(&outs, d.id)
            .unwrap()
            .events
            .iter()
            .any(|ev| matches!(
                decode_event(ev).unwrap().event,
                NetEvent::OwnershipTransfer { id, .. } if id == original_id
            )),
        "the transfer wire carries the stable original NetEntityId"
    );
    assert_eq!(d.entity_count(), 1, "no proxy churn on the observer");
    assert_eq!(b.entity_count(), 1);
}

/// R-RT2 — a full cycle A→B→C→A through THREE distinct owners (all pre-
/// witnessing). C→A re-adopts A's ORIGINAL entity after two intervening owners
/// (exercises the id-map `last_owner` bookkeeping, lib.rs:864-866).
#[test]
fn cycle_a_b_c_a_all_witnessing() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut c = TestPeer::new(3);
    let e = a.spawn(0.0, 0.0, 2.0, 0.0);
    let original_id = net_id(a.id, e);
    // A replicates to BOTH B and C so each witnesses e (can adopt it).
    a.track(b.id);
    a.track(c.id);
    let outs = a.collect_all();
    b.deliver_all(a.id, outbox_for(&outs, b.id).unwrap());
    c.deliver_all(a.id, outbox_for(&outs, c.id).unwrap());
    let proxy_b = b.entity_owned_by(a.id);
    let proxy_c = c.entity_owned_by(a.id);

    // A→B: tell B (new owner) and C (future owner, must keep witnessing).
    a.repl
        .transfer_ownership(&mut a.world, e, b.id)
        .expect("A→B");
    let outs = a.collect_all();
    b.deliver_events(a.id, &outbox_for(&outs, b.id).unwrap().events);
    c.deliver_events(a.id, &outbox_for(&outs, c.id).unwrap().events);
    assert_eq!(c.owner(proxy_c), b.id, "C witnesses A→B");
    b.track(a.id);
    b.track(c.id);
    b.sim_tick();
    a.deliver_all(b.id, &b.collect_for(a.id)); // B → A (A must witness for C→A later)
    c.deliver_all(b.id, &b.collect_for(c.id)); // B → C

    // B→C: tell C (new owner) and A (future owner).
    b.repl
        .transfer_ownership(&mut b.world, proxy_b, c.id)
        .expect("B→C");
    let outs = b.collect_all();
    a.deliver_events(b.id, &outbox_for(&outs, a.id).unwrap().events);
    c.deliver_events(b.id, &outbox_for(&outs, c.id).unwrap().events);
    assert_eq!(c.owner(proxy_c), c.id, "C adopts B→C");
    assert_eq!(a.owner(e), c.id, "A's proxy tracks B→C");
    c.track(a.id);
    c.sim_tick();
    a.deliver_all(c.id, &c.collect_for(a.id)); // C → A witnesses

    // C→A: A re-adopts its ORIGINAL entity after two intervening owners.
    c.repl
        .transfer_ownership(&mut c.world, proxy_c, a.id)
        .expect("C→A");
    let ct = c.collect_for(a.id);
    // The C→A transfer wire carries A's ORIGINAL id (spawner=A), proving identity
    // survived two intervening owners (C could not mint in A's namespace).
    assert!(
        ct.events.iter().any(|ev| matches!(
            decode_event(ev).unwrap().event,
            NetEvent::OwnershipTransfer { id, .. } if id == original_id
        )),
        "the wire carries the stable original NetEntityId across the cycle"
    );
    a.deliver_events(c.id, &ct.events);
    assert_eq!(
        a.owner(e),
        a.id,
        "A re-adopts its original entity after the cycle"
    );
    assert_eq!(b.entity_count(), 1);
    assert_eq!(c.entity_count(), 1);
}

/// R-RT3 (WHITE-BOX) — the transfer-path analogue of B4: a round trip A→B→A
/// RE-BASELINES the entity (the send_state drop, lib.rs:1108-1114), so a STALE
/// ack (< the re-adopt run_start) must NOT confirm the re-adopted value. Without
/// the drop, a pre-round-trip ack would falsely confirm → A goes silent →
/// permanent divergence. Guards a currently-untested invariant.
#[test]
fn roundtrip_rebaseline_stale_ack_does_not_confirm() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(1.0, 0.0, 0.0, 0.0); // stationary
    let e_id = net_id(a.id, e);
    let out = a.collect_for(b.id);
    b.deliver_all(a.id, &out);
    let proxy = b.entity_owned_by(a.id);
    flush_acks(&mut b, &mut a); // acked_seq[b] = 1

    // A→B→A round trip.
    a.repl
        .transfer_ownership(&mut a.world, e, b.id)
        .expect("A→B");
    b.deliver_events(a.id, &a.collect_for(b.id).events);
    b.repl
        .transfer_ownership(&mut b.world, proxy, a.id)
        .expect("B→A");
    a.deliver_events(b.id, &b.collect_for(a.id).events);
    assert_eq!(a.owner(e), a.id, "A re-adopts");

    // A re-collects e at a FRESH run_start (send_state[b][e] was dropped). If the
    // drop regressed, `re` would be quiet (falsely confirmed) → state_seq panics.
    let re = a.collect_for(b.id);
    let reenter_seq = state_seq(&re);
    assert!(
        state_entries(&re).iter().any(|s| s.id == e_id),
        "e is re-sent on re-adopt"
    );

    // A stale ack (< the re-adopt run_start) must NOT confirm it.
    inject_ack(&mut a, b.id, reenter_seq - 1);
    assert!(
        state_entries(&a.collect_for(b.id))
            .iter()
            .any(|s| s.id == e_id),
        "a stale ack (< re-adopt run_start) must not confirm the re-baselined entity"
    );
    // A correct ack ≥ the re-adopt run_start confirms it.
    inject_ack(&mut a, b.id, reenter_seq);
    assert!(
        !state_entries(&a.collect_for(b.id))
            .iter()
            .any(|s| s.id == e_id),
        "e quiet once b acks ≥ the re-adopt run_start"
    );
}

/// R-LOSS1 — a state packet DROPPED around a handoff heals via the fresh owner's
/// delta, and a late stale packet from the OLD owner is inert (LWW + the
/// `owner!=from` gate — no wrong-owner apply, no resurrection).
#[test]
fn state_drop_around_handoff_heals_and_no_wrong_owner() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut d = TestPeer::new(3);
    let e = a.spawn(0.0, 0.0, 2.0, 0.0);
    a.track(b.id);
    a.track(d.id);
    let outs = a.collect_all();
    b.deliver_all(a.id, outbox_for(&outs, b.id).unwrap());
    d.deliver_all(a.id, outbox_for(&outs, d.id).unwrap());
    let proxy_b = b.entity_owned_by(a.id);
    let proxy_d = d.entity_owned_by(a.id);

    // A moves e and its new state to D is DROPPED (seq 2, never delivered).
    a.sim_tick();
    let lost = a.collect_for(d.id);
    assert!(lost.state.is_some());

    // A→B: tell B and D.
    a.repl
        .transfer_ownership(&mut a.world, e, b.id)
        .expect("A→B");
    let outs = a.collect_all();
    b.deliver_events(a.id, &outbox_for(&outs, b.id).unwrap().events);
    d.deliver_events(a.id, &outbox_for(&outs, d.id).unwrap().events);
    assert_eq!(d.owner(proxy_d), b.id, "D sees A→B");

    // B (fresh owner) replicates to D — its full-mask delta heals the drop. Sim
    // B TWICE so it reaches x=2.0, DISTINCT from the stale old-owner value (1.0),
    // so the owner-gate drop below is actually discriminating (auditor).
    b.track(d.id);
    b.sim_tick();
    b.sim_tick();
    d.deliver_all(b.id, &b.collect_for(d.id));
    assert!(
        approx(d.pos(proxy_d).x, b.pos(proxy_b).x),
        "the fresh owner's delta heals the dropped packet"
    );

    // Late-deliver the dropped OLD-owner packet (stale value 1.0 ≠ B's 2.0): inert
    // by the owner gate — its seq 2 > D.last_seq[a]=1 so LWW alone would ADMIT it,
    // so the assertion below would FAIL if the `owner!=from` gate were removed.
    let healed = d.pos(proxy_d);
    d.deliver_state(a.id, lost.state.as_ref().unwrap());
    assert_eq!(
        healed.x.to_bits(),
        d.pos(proxy_d).x.to_bits(),
        "a stale old-owner packet must not apply over the new owner"
    );
}

/// R-LOSS2 — reordered state around a handoff: the NEW owner's newer state
/// applies, then the OLD owner's earlier (higher-seq) in-flight state is inert by
/// the `owner!=from` gate even though LWW would admit it.
#[test]
fn reordered_state_around_transfer_dropped() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut d = TestPeer::new(3);
    let e = a.spawn(0.0, 0.0, 2.0, 0.0);
    a.track(b.id);
    a.track(d.id);
    let outs = a.collect_all();
    b.deliver_all(a.id, outbox_for(&outs, b.id).unwrap());
    d.deliver_all(a.id, outbox_for(&outs, d.id).unwrap());
    let proxy_b = b.entity_owned_by(a.id);
    let proxy_d = d.entity_owned_by(a.id);

    // Capture A's in-flight state (seq 2) BEFORE the transfer.
    a.sim_tick();
    let in_flight = a.collect_for(d.id).state.unwrap();

    // A→B; D adopts the new owner.
    a.repl
        .transfer_ownership(&mut a.world, e, b.id)
        .expect("A→B");
    let outs = a.collect_all();
    b.deliver_events(a.id, &outbox_for(&outs, b.id).unwrap().events);
    d.deliver_events(a.id, &outbox_for(&outs, d.id).unwrap().events);

    // B's newer state applies at D. Sim B TWICE so b_val=2.0 is DISTINCT from the
    // captured in-flight value (1.0) — else the drop below would be unobservable.
    b.track(d.id);
    b.sim_tick();
    b.sim_tick();
    d.deliver_all(b.id, &b.collect_for(d.id));
    let b_val = b.pos(proxy_b).x;
    assert!(approx(d.pos(proxy_d).x, b_val), "new-owner state applies");

    // Now the reordered OLD-owner in-flight (value 1.0, seq 2 > D.last_seq[a]=1 so
    // LWW ADMITS it) arrives — inert by the owner gate. Would FAIL (d.pos → 1.0)
    // if the `owner!=from` gate were removed.
    d.deliver_state(a.id, &in_flight);
    assert!(
        approx(d.pos(proxy_d).x, b_val),
        "reordered old-owner in-flight is dropped by the owner gate"
    );
}

/// Build the documented R6 chained-transfer scenario: an A→B→C chain with the two
/// ownership events for observer D CAPTURED but not yet delivered, so each test
/// chooses the delivery order. Under the ADR-0025 A OwnerSeq gate the withheld
/// `t1_d` carries rank `{1,A}` (A→B) and `t2_d` carries `{2,B}` (B→C); C is the
/// real current owner (rank `{2,B}`). Returns
/// `(a, b, c, d, proxy_c, proxy_d, t1_d, t2_d)`.
#[allow(clippy::type_complexity)]
fn build_r6_chain() -> (
    TestPeer,
    TestPeer,
    TestPeer,
    TestPeer,
    Entity,
    Entity,
    Vec<Box<[u8]>>,
    Vec<Box<[u8]>>,
) {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut c = TestPeer::new(3);
    let mut d = TestPeer::new(4);
    let e = a.spawn(0.0, 0.0, 2.0, 0.0);
    a.track(b.id);
    a.track(c.id);
    a.track(d.id);
    let outs = a.collect_all();
    b.deliver_all(a.id, outbox_for(&outs, b.id).unwrap());
    c.deliver_all(a.id, outbox_for(&outs, c.id).unwrap());
    d.deliver_all(a.id, outbox_for(&outs, d.id).unwrap());
    let proxy_c = c.entity_owned_by(a.id);
    let proxy_d = d.entity_owned_by(a.id);

    // A→B: deliver to B and C in order; capture T1 for D and withhold it.
    a.repl
        .transfer_ownership(&mut a.world, e, b.id)
        .expect("A→B");
    let outs = a.collect_all();
    b.deliver_events(a.id, &outbox_for(&outs, b.id).unwrap().events);
    c.deliver_events(a.id, &outbox_for(&outs, c.id).unwrap().events);
    let t1_d = outbox_for(&outs, d.id).unwrap().events.clone();

    // B AOI-enters e for C and D (so a B→C transfer reaches both); apply to C only.
    b.track(a.id);
    b.track(c.id);
    b.track(d.id);
    b.sim_tick();
    let bcast = b.collect_all();
    c.deliver_all(b.id, outbox_for(&bcast, c.id).unwrap());

    // B→C: deliver to C in order; capture T2 for D and withhold it.
    let proxy_b = b.entity_owned_by(b.id);
    b.repl
        .transfer_ownership(&mut b.world, proxy_b, c.id)
        .expect("B→C");
    let outs = b.collect_all();
    c.deliver_events(b.id, &outbox_for(&outs, c.id).unwrap().events);
    let t2_d = outbox_for(&outs, d.id).unwrap().events.clone();

    // D's two ownership events are withheld — the caller delivers them (in an
    // order that exercises the reorder resolution or a lost-transfer heal).
    (a, b, c, d, proxy_c, proxy_d, t1_d, t2_d)
}

/// R6-1 — the chained-transfer cross-sender REORDER now RESOLVES BY RANK
/// (ADR-0025 A), closing the documented ADR-0013 R6 gap at the SOURCE — no
/// freeze, no resync needed. D receives T2 `{2,B}` (owner→C) FIRST: it outranks
/// the birth rank so it applies. T1 `{1,A}` (owner→B) arrives second and is
/// dropped as stale (lower rank). D lands on the REAL owner C and C's state
/// applies — the exact scenario that previously FROZE D at the wrong owner B.
#[test]
fn r6_chained_reorder_resolves_by_seq() {
    let (a, b, mut c, mut d, proxy_c, proxy_d, t1_d, t2_d) = build_r6_chain();

    // Adverse cross-sender order at D: T2 (from B, rank {2,B}) FIRST, then T1
    // (from A, rank {1,A}). The higher rank wins regardless of arrival order.
    d.deliver_events(b.id, &t2_d);
    d.deliver_events(a.id, &t1_d);

    assert_eq!(
        d.owner(proxy_d),
        c.id,
        "R6 resolved by rank: D lands on the REAL owner C (not frozen at B)"
    );
    assert_eq!(c.owner(proxy_c), c.id, "C is the real current owner");

    // C's normal state APPLIES immediately — the freeze is gone entirely.
    c.sim_tick();
    let cstate = c.collect_for(d.id);
    assert!(
        cstate.state.is_some(),
        "C genuinely emits state for e (so the assertion below is non-vacuous)"
    );
    d.deliver_state(c.id, cstate.state.as_ref().unwrap());
    assert!(
        approx(d.pos(proxy_d).x, c.pos(proxy_c).x),
        "the real owner C's state applies to D — no freeze"
    );

    // D acks C directly (nothing was ever withheld).
    assert!(
        d.repl.drain_acks().iter().any(|(t, _)| *t == c.id),
        "D acks C's stream"
    );
}

/// R6-2 — anti-entropy RESYNC heals a genuinely-stale wrong-owner proxy — the
/// residual the reorder path can't fix because the event was LOST, not reordered
/// (ADR-0024 + the ADR-0025 A owner-change heal). D receives ONLY T1 (owner→B)
/// and misses T2 entirely, so it is stuck at owner B rank {1,A} while the real
/// owner is C at {2,B}. One digest→request→ResyncSpawn round from C corrects D
/// via the `seq >= rec.owner_seq` gate ({2,B} outranks {1,A}); idempotent after.
#[test]
fn resync_heals_lost_transfer_wrong_owner() {
    let (a, b, mut c, mut d, proxy_c, proxy_d, t1_d, _t2_d) = build_r6_chain();

    // D gets T1 only — T2 is LOST. D is stuck at owner B; C really owns it.
    d.deliver_events(a.id, &t1_d);
    assert_eq!(d.owner(proxy_d), b.id, "D stuck at owner B (missed T2)");
    assert_eq!(c.owner(proxy_c), c.id, "C is the real current owner");

    // Precondition: C's state is REJECTED by the wrong-owner proxy (the residual
    // that the STATE owner gate produces — non-vacuous, and what resync heals).
    let before = d.pos(proxy_d);
    c.sim_tick();
    let cstate = c.collect_for(d.id);
    d.deliver_state(c.id, cstate.state.as_ref().unwrap());
    assert_eq!(
        before.x.to_bits(),
        d.pos(proxy_d).x.to_bits(),
        "before heal: the real owner C's state is rejected by the wrong-owner proxy"
    );

    // One resync round heals D (owner-change gate {2,B} >= {1,A}).
    flush_resync(&mut c, &mut d);
    assert_eq!(
        d.owner(proxy_d),
        c.id,
        "resync corrects the proxy owner B→C"
    );

    // C's normal state now APPLIES.
    c.sim_tick();
    let cstate = c.collect_for(d.id);
    d.deliver_state(c.id, cstate.state.as_ref().unwrap());
    assert!(
        approx(d.pos(proxy_d).x, c.pos(proxy_c).x),
        "C's state now applies to D's corrected proxy"
    );

    // The withheld ack stream unblocks: D now acks C.
    assert!(
        d.repl.drain_acks().iter().any(|(t, _)| *t == c.id),
        "D now acks C's stream (the withheld ack unblocks)"
    );

    // Idempotent: once converged, a further digest produces NO request from D.
    for (target, bytes) in c.repl.collect_resync(&mut c.world) {
        if target == d.id {
            d.repl.apply_events(&mut d.world, c.id, &bytes);
        }
    }
    assert!(
        d.repl.drain_resync_requests().is_empty(),
        "converged — a further digest triggers no resync request"
    );
    assert_eq!(d.owner(proxy_d), c.id, "proxy stays owner C");
}

/// R6-3 — responder-owns-check: a peer asked to resync an entity it does NOT own
/// emits no `ResyncSpawn` (it cannot assert ownership it lacks — no theft).
#[test]
fn resync_responder_only_answers_owned() {
    let mut a = TestPeer::new(1); // owns e
    let mut b = TestPeer::new(2); // holds a proxy, does NOT own e
    let e = a.spawn(1.0, 2.0, 0.0, 0.0);
    b.deliver_all(a.id, &a.collect_for(b.id));
    let proxy = b.entity_owned_by(a.id);
    assert_eq!(
        b.owner(proxy),
        a.id,
        "B holds A's entity but does not own it"
    );

    let id = net_id(a.id, e);
    let req = EventMsg {
        version: WIRE_VERSION,
        sig: None,
        event: NetEvent::ResyncRequest { ids: vec![id] },
    };
    let bytes = encode_event(&req).expect("encode");
    b.repl.apply_events(&mut b.world, PeerId(9), &bytes);

    assert!(
        b.repl.drain_resync_responses(&mut b.world).is_empty(),
        "a non-owner must not answer a resync request (no ownership assertion)"
    );
}

/// R6-4 — own-authority guard: a `ResyncSpawn` for an entity WE own is dropped —
/// a stale/foreign owner-assertion can never steal our authority or clobber our
/// authoritative state, EVEN at a forged-high rank (the guard precedes the seq
/// gate; ADR-0025 A).
#[test]
fn resync_own_authority_guard_drops_foreign_assert() {
    let mut a = TestPeer::new(1);
    let e = a.spawn(3.0, 3.0, 0.0, 0.0);
    let _ = a.collect_all(); // mint e's id into the map so the guard can resolve it
    let id = net_id(a.id, e);

    let ev = EventMsg {
        version: WIRE_VERSION,
        sig: None,
        event: NetEvent::ResyncSpawn {
            id,
            pos: quantize_vec2(99.0, 99.0),
            vel: quantize_vec2(0.0, 0.0),
            // A deliberately huge rank — the own-authority guard must still win.
            seq: OwnerSeq {
                seq: 999,
                coordinator: PeerId(2),
            },
        },
    };
    let bytes = encode_event(&ev).expect("encode");
    a.repl.apply_events(&mut a.world, PeerId(2), &bytes);

    assert_eq!(
        a.owner(e),
        a.id,
        "own-authority guard: a ResyncSpawn cannot steal an entity we own"
    );
    assert!(
        approx(a.pos(e).x, 3.0),
        "and cannot overwrite our authoritative state"
    );
}

/// R6-5 — the digest HASH-mismatch path (ADR-0024): a SILENT divergence on a
/// confirmed + quiet value (correct owner, right existence — a bug / dropped
/// correction the delta stream can no longer heal because the owner is quiet) is
/// caught by the state-hash and healed. Exercises `component_quiet` / `fnv32` /
/// `proxy_state_hash` end-to-end.
#[test]
fn resync_heals_stale_silent_value() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(5.0, 5.0, 0.0, 0.0); // stationary
    b.deliver_all(a.id, &a.collect_for(b.id));
    flush_acks(&mut b, &mut a); // confirmed + quiet on both sides
    let proxy = b.entity_owned_by(a.id);
    assert!(approx(b.pos(proxy).x, 5.0));
    assert!(
        a.collect_for(b.id).state.is_none(),
        "A is quiet — the delta stream cannot heal a silent divergence"
    );

    // Simulate a silent divergence: B's proxy value drifts while A stays quiet.
    b.world.get_mut::<Position>(proxy).unwrap().x = 9.0;

    // One resync round: A's digest carries the confirmed value's hash, B finds it
    // mismatched (same owner, different hash), and the ResyncSpawn corrects it.
    flush_resync(&mut a, &mut b);
    assert!(
        approx(b.pos(proxy).x, 5.0),
        "the digest hash-mismatch heals the stale silent value"
    );
    assert_eq!(
        b.owner(proxy),
        a.id,
        "owner unchanged (this was a value divergence)"
    );

    // Converged: a further digest triggers no request.
    for (target, bytes) in a.repl.collect_resync(&mut a.world) {
        if target == b.id {
            b.repl.apply_events(&mut b.world, a.id, &bytes);
        }
    }
    assert!(
        b.repl.drain_resync_requests().is_empty(),
        "in sync — the matching hash triggers no further request"
    );
}

// ═══════════════ Group HM — host-migration reassignment (ADR-0025 B) ═══════════════

/// A 3-peer mesh where A(1) owns e and B(2), C(3) hold proxies; every peer tracks
/// the other two, so a survivor's `peers` set = the surviving membership that
/// `reassign_orphans` elects over. Returns (a, b, c, proxy_b, proxy_c).
fn mesh3_a_owns() -> (TestPeer, TestPeer, TestPeer, Entity, Entity) {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut c = TestPeer::new(3);
    a.spawn(0.0, 0.0, 2.0, 0.0);
    a.track(b.id);
    a.track(c.id);
    let outs = a.collect_all();
    b.deliver_all(a.id, outbox_for(&outs, b.id).unwrap());
    c.deliver_all(a.id, outbox_for(&outs, c.id).unwrap());
    let proxy_b = b.entity_owned_by(a.id);
    let proxy_c = c.entity_owned_by(a.id);
    b.track(a.id);
    b.track(c.id);
    c.track(a.id);
    c.track(b.id);
    (a, b, c, proxy_b, proxy_c)
}

/// HM1 — owner-drop reassigns the orphan to the LOWEST surviving peer, exactly
/// once: B(2) < C(3) adopts (Owner:=local, simulates); C re-tags its proxy to B.
/// The "reassigned exactly once" acceptance.
#[test]
fn owner_drop_reassigns_to_lowest_survivor() {
    let (a, mut b, mut c, proxy_b, proxy_c) = mesh3_a_owns();
    assert_eq!(b.owner(proxy_b), a.id, "before: B's proxy owned by A");

    b.repl.untrack_peer(a.id);
    let rb = b.repl.reassign_orphans(&mut b.world, a.id);
    c.repl.untrack_peer(a.id);
    let rc = c.repl.reassign_orphans(&mut c.world, a.id);

    assert_eq!(rb, vec![proxy_b], "B reassigned the one orphan");
    assert_eq!(rc, vec![proxy_c], "C reassigned the one orphan");
    assert_eq!(
        b.owner(proxy_b),
        b.id,
        "lowest survivor B adopts (Owner:=local)"
    );
    assert_eq!(
        c.owner(proxy_c),
        b.id,
        "C re-tags its proxy to the elected owner B"
    );

    let before = b.pos(proxy_b);
    b.sim_tick();
    assert!(
        b.pos(proxy_b).x > before.x,
        "B now simulates the reassigned entity (freeze lifted)"
    );
}

/// HM2 — idempotent: a second `reassign_orphans` for the same departed peer finds
/// nothing owned by it (already re-tagged) and is a no-op.
#[test]
fn reassign_idempotent_on_double_call() {
    let (a, mut b, _c, proxy_b, _) = mesh3_a_owns();
    b.repl.untrack_peer(a.id);
    assert_eq!(b.repl.reassign_orphans(&mut b.world, a.id), vec![proxy_b]);
    assert_eq!(b.owner(proxy_b), b.id);
    assert!(
        b.repl.reassign_orphans(&mut b.world, a.id).is_empty(),
        "re-reassign finds no departed-owned entity — no-op"
    );
    assert_eq!(b.owner(proxy_b), b.id, "owner unchanged");
    assert_eq!(b.entity_count(), 1, "no churn");
}

/// HM3 — after reassignment the elected owner's state applies to a survivor's
/// proxy (the freeze lifts — the wrong-owner gate now passes since owner==elected).
#[test]
fn reassign_elected_owner_unfreezes_proxy() {
    let (a, mut b, mut c, proxy_b, proxy_c) = mesh3_a_owns();
    b.repl.untrack_peer(a.id);
    b.repl.reassign_orphans(&mut b.world, a.id);
    c.repl.untrack_peer(a.id);
    c.repl.reassign_orphans(&mut c.world, a.id);

    // C re-tagged its proxy to the REMOTE elected owner B, so C must NOT simulate
    // it (the "never re-simulate others' entities" invariant — authority gate).
    let c_before = c.pos(proxy_c);
    c.sim_tick();
    assert_eq!(
        c.pos(proxy_c).x.to_bits(),
        c_before.x.to_bits(),
        "C does not simulate the entity it re-tagged to a remote owner"
    );

    b.sim_tick();
    let out = b.collect_for(c.id);
    c.deliver_all(b.id, &out);
    assert!(
        approx(c.pos(proxy_c).x, b.pos(proxy_b).x),
        "C's proxy tracks the elected owner B's state (freeze lifted)"
    );
}

/// HM4 (WHITE-BOX) — reassignment FLUSHES the proxy's InterpBuffer: the buffered
/// snapshots came from the departed owner and must not lerp across the source
/// discontinuity (mirrors the OwnershipTransfer/ResyncSpawn buffer flush).
#[test]
fn reassign_flushes_interp_buffer() {
    let (a, _b, mut c, _proxy_b, proxy_c) = mesh3_a_owns();
    assert!(
        c.world
            .get::<InterpBuffer>(proxy_c)
            .is_some_and(|buf| !buf.0.is_empty()),
        "precondition: C's proxy has buffered snapshots from the old owner"
    );
    c.repl.untrack_peer(a.id);
    c.repl.reassign_orphans(&mut c.world, a.id);
    assert!(
        c.world
            .get::<InterpBuffer>(proxy_c)
            .is_none_or(|buf| buf.0.is_empty()),
        "reassign flushes the buffer (old-owner snapshots must not lerp)"
    );
}

/// HM5 — chained double-drop: A drops → B adopts; then B drops → C (the last
/// survivor) adopts. Owned exactly once at each hop.
#[test]
fn reassign_chained_double_drop() {
    let (a, mut b, mut c, proxy_b, proxy_c) = mesh3_a_owns();
    b.repl.untrack_peer(a.id);
    b.repl.reassign_orphans(&mut b.world, a.id);
    c.repl.untrack_peer(a.id);
    c.repl.reassign_orphans(&mut c.world, a.id);
    assert_eq!(b.owner(proxy_b), b.id, "B adopts after A drops");

    // B drops → C is the only survivor → C adopts.
    c.repl.untrack_peer(b.id);
    let rc = c.repl.reassign_orphans(&mut c.world, b.id);
    assert_eq!(rc, vec![proxy_c], "C reassigns the now-B-owned orphan");
    assert_eq!(c.owner(proxy_c), c.id, "C — the last survivor — adopts");
}

/// HM6 — reassigning a departed peer that owned nothing here is a no-op.
#[test]
fn reassign_no_orphans_is_noop() {
    let mut a = TestPeer::new(1);
    let b = TestPeer::new(2);
    let own = a.spawn(0.0, 0.0, 0.0, 0.0);
    a.track(b.id);
    let r = a.repl.reassign_orphans(&mut a.world, PeerId(9));
    assert!(r.is_empty(), "no entity owned by the departed peer — no-op");
    assert_eq!(a.owner(own), a.id, "A's own entity untouched");
}

/// HM7 — every survivor independently elects the SAME owner (no coordination):
/// A(1) drops; B(2),C(3),D(4) all elect B(2). B local, the rest remote-B.
#[test]
fn reassign_all_survivors_agree_on_elected() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut c = TestPeer::new(3);
    let mut d = TestPeer::new(4);
    a.spawn(0.0, 0.0, 2.0, 0.0);
    a.track(b.id);
    a.track(c.id);
    a.track(d.id);
    let outs = a.collect_all();
    b.deliver_all(a.id, outbox_for(&outs, b.id).unwrap());
    c.deliver_all(a.id, outbox_for(&outs, c.id).unwrap());
    d.deliver_all(a.id, outbox_for(&outs, d.id).unwrap());
    let (pb, pc, pd) = (
        b.entity_owned_by(a.id),
        c.entity_owned_by(a.id),
        d.entity_owned_by(a.id),
    );
    b.track(a.id);
    b.track(c.id);
    b.track(d.id);
    c.track(a.id);
    c.track(b.id);
    c.track(d.id);
    d.track(a.id);
    d.track(b.id);
    d.track(c.id);

    b.repl.untrack_peer(a.id);
    b.repl.reassign_orphans(&mut b.world, a.id);
    c.repl.untrack_peer(a.id);
    c.repl.reassign_orphans(&mut c.world, a.id);
    d.repl.untrack_peer(a.id);
    d.repl.reassign_orphans(&mut d.world, a.id);

    assert_eq!(b.owner(pb), b.id, "B (min) adopts");
    assert_eq!(c.owner(pc), b.id, "C agrees: elected = B");
    assert_eq!(d.owner(pd), b.id, "D agrees: elected = B");
}

/// HM8 — reassignment CLOSES the ADR-0024 E4 orphan. O(3) spawns e and hands it
/// to A(4) (adopted, spawner stays O); B(1) witnessed it, D(2) NEVER did. A drops:
/// B (lowest survivor) adopts the FOREIGN-namespace orphan and so cannot mint a
/// Spawn for D — but B now holds a LOCAL proxy (exactly what E4 lacked), so one
/// resync round builds D's proxy. Reassignment + resync together heal the orphan.
#[test]
fn reassign_of_e4_orphan_then_resync_heals_nonwitness() {
    let mut o = TestPeer::new(3); // spawner
    let mut a = TestPeer::new(4); // adopter — will DROP
    let mut b = TestPeer::new(1); // witness + lowest survivor ⇒ elected owner
    let mut d = TestPeer::new(2); // NEVER witnesses e

    let e = o.spawn(1.0, 1.0, 0.0, 0.0);
    o.track(a.id);
    o.track(b.id);
    let outs = o.collect_all();
    a.deliver_all(o.id, outbox_for(&outs, a.id).unwrap());
    b.deliver_all(o.id, outbox_for(&outs, b.id).unwrap());
    let proxy_b = b.entity_owned_by(o.id);

    // O→A: A adopts (e.spawner stays O); B's proxy reads owner A.
    o.repl
        .transfer_ownership(&mut o.world, e, a.id)
        .expect("O owns e");
    let outs = o.collect_all();
    a.deliver_events(o.id, &outbox_for(&outs, a.id).unwrap().events);
    b.deliver_events(o.id, &outbox_for(&outs, b.id).unwrap().events);
    assert_eq!(b.owner(proxy_b), a.id, "B's proxy reads owner A");
    assert_eq!(d.entity_count(), 0, "D never witnessed e");

    // Membership for the election: each survivor must have B in its candidate set.
    b.track(o.id);
    b.track(a.id);
    b.track(d.id);
    d.track(o.id);
    d.track(a.id);
    d.track(b.id);

    // A drops. B (lowest survivor) adopts the foreign-namespace orphan; D (a
    // non-witness) has nothing to reassign.
    b.repl.untrack_peer(a.id);
    let rb = b.repl.reassign_orphans(&mut b.world, a.id);
    d.repl.untrack_peer(a.id);
    let rd = d.repl.reassign_orphans(&mut d.world, a.id);
    assert_eq!(rb, vec![proxy_b], "B adopts the orphan");
    assert_eq!(
        b.owner(proxy_b),
        b.id,
        "B is the elected owner (Local proxy — E4 lacked this)"
    );
    assert!(
        rd.is_empty(),
        "D never had the entity — nothing to reassign"
    );

    // B's collect AOI-enters e for D (no Spawn — foreign namespace); one resync
    // round then builds D's proxy — the E4 orphan is finally healed.
    let _ = b.collect_for(d.id);
    flush_resync(&mut b, &mut d);
    assert_eq!(
        d.entity_count(),
        1,
        "resync builds D's proxy (E4 orphan closed)"
    );
    let proxy_d = d.entity_owned_by(b.id);
    assert_eq!(
        d.owner(proxy_d),
        b.id,
        "D's proxy is owned by the elected owner B"
    );
}

// ═══════════ Group AK — ownership arbitration by OwnerSeq (ADR-0025 A kernel) ═══════════

/// AK-K1 — the OwnerSeq strictly INCREASES along a transfer chain and records the
/// giver as its coordinator: A→B mints `{1,A}`, B→C mints `{2,B}` (white-box).
#[test]
fn transfer_seq_increments_along_chain() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let c = TestPeer::new(3);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    let _ = a.collect_all(); // mint e's id into the map
    assert_eq!(
        a.repl.owner_seq(e),
        Some(OwnerSeq {
            seq: 0,
            coordinator: a.id
        }),
        "birth rank is (0, spawner)"
    );

    // A→B mints {1, A}.
    a.repl
        .transfer_ownership(&mut a.world, e, b.id)
        .expect("A→B");
    assert_eq!(
        a.repl.owner_seq(e),
        Some(OwnerSeq {
            seq: 1,
            coordinator: a.id
        }),
        "A→B mints (1, A)"
    );

    // Deliver A→B so B builds + owns the proxy at rank {1, A}.
    b.deliver_all(a.id, &a.collect_for(b.id));
    let proxy_b = b.entity_owned_by(b.id);
    assert_eq!(
        b.repl.owner_seq(proxy_b),
        Some(OwnerSeq {
            seq: 1,
            coordinator: a.id
        }),
        "B's proxy adopts rank (1, A)"
    );

    // B→C mints {2, B} — strictly higher, coordinator now B.
    b.repl
        .transfer_ownership(&mut b.world, proxy_b, c.id)
        .expect("B→C");
    assert_eq!(
        b.repl.owner_seq(proxy_b),
        Some(OwnerSeq {
            seq: 2,
            coordinator: b.id
        }),
        "B→C mints (2, B) — strictly higher rank, coordinator now B"
    );
}

/// AK-K2 — highest rank wins irrespective of ARRIVAL ORDER and irrespective of the
/// sending peer: a higher-rank transfer delivered FIRST applies, and a later
/// lower-rank transfer is dropped. `from` is not consulted — the rank is the
/// arbiter (the old `owner!=from` gate is gone).
#[test]
fn highest_seq_wins_on_reordered_transfer() {
    let mut a = TestPeer::new(1); // spawner
    let mut d = TestPeer::new(4); // observer
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    d.deliver_all(a.id, &a.collect_for(d.id));
    let proxy = d.entity_owned_by(a.id);
    let id = net_id(a.id, e);
    assert_eq!(
        d.repl.owner_seq(proxy),
        Some(OwnerSeq {
            seq: 0,
            coordinator: a.id
        })
    );

    let mk = |new_owner: PeerId, seq: OwnerSeq| {
        encode_event(&EventMsg {
            version: WIRE_VERSION,
            sig: None,
            event: NetEvent::OwnershipTransfer { id, new_owner, seq },
        })
        .expect("encode")
    };
    let hi = mk(
        PeerId(3),
        OwnerSeq {
            seq: 2,
            coordinator: PeerId(2),
        },
    );
    let lo = mk(
        PeerId(2),
        OwnerSeq {
            seq: 1,
            coordinator: PeerId(1),
        },
    );

    // Higher rank FIRST (delivered from an arbitrary peer — `from` is irrelevant).
    d.repl.apply_events(&mut d.world, PeerId(7), &hi);
    assert_eq!(d.owner(proxy), PeerId(3), "higher-rank transfer applies");
    assert_eq!(
        d.repl.owner_seq(proxy),
        Some(OwnerSeq {
            seq: 2,
            coordinator: PeerId(2)
        })
    );

    // Lower rank SECOND — dropped despite arriving later.
    d.repl.apply_events(&mut d.world, PeerId(8), &lo);
    assert_eq!(
        d.owner(proxy),
        PeerId(3),
        "the later lower-rank transfer is dropped"
    );
    assert_eq!(
        d.repl.owner_seq(proxy),
        Some(OwnerSeq {
            seq: 2,
            coordinator: PeerId(2)
        }),
        "rank unchanged by the stale transfer"
    );
}

/// AK-K3 — the transfer gate is STRICT (`seq > proxy`): a conflicting transfer at
/// the SAME rank the proxy already holds is dropped, so a same-rank assertion can
/// never flip the owner (this is why transfer/commit use `>`, not `>=`).
#[test]
fn equal_rank_transfer_dropped_strict_gate() {
    let mut a = TestPeer::new(1);
    let mut d = TestPeer::new(4);
    let e = a.spawn(0.0, 0.0, 0.0, 0.0);
    d.deliver_all(a.id, &a.collect_for(d.id));
    let proxy = d.entity_owned_by(a.id);
    let id = net_id(a.id, e);

    let mk = |new_owner: PeerId| {
        encode_event(&EventMsg {
            version: WIRE_VERSION,
            sig: None,
            event: NetEvent::OwnershipTransfer {
                id,
                new_owner,
                seq: OwnerSeq {
                    seq: 1,
                    coordinator: a.id,
                },
            },
        })
        .expect("encode")
    };

    // Advance D to rank {1, A}, owner PeerId(2).
    d.repl.apply_events(&mut d.world, a.id, &mk(PeerId(2)));
    assert_eq!(d.owner(proxy), PeerId(2));

    // A conflicting transfer at the SAME rank {1, A} to a DIFFERENT owner — dropped.
    // If the gate were `>=` this would wrongly flip the owner to PeerId(3).
    d.repl.apply_events(&mut d.world, a.id, &mk(PeerId(3)));
    assert_eq!(
        d.owner(proxy),
        PeerId(2),
        "equal-rank transfer dropped by the strict `>` gate"
    );
}

/// AK-K4 — the ADR-0025 A resync BACKDOOR is closed: after an entity is committed
/// to a new owner at a higher rank, a stale `ResyncSpawn` from a FORMER owner (a
/// strictly-lower rank) must NOT revert the owner or clobber state — the
/// owner-change heal is gated `seq >= rec.owner_seq`.
#[test]
fn resync_backdoor_stale_former_owner_dropped() {
    let mut src = TestPeer::new(1); // spawns e so D can build a proxy
    let mut d = TestPeer::new(4);
    let e = src.spawn(1.0, 1.0, 0.0, 0.0);
    d.deliver_all(src.id, &src.collect_for(d.id));
    let proxy = d.entity_owned_by(src.id);
    let id = net_id(src.id, e);

    // Commit D's proxy to PeerId(3) at rank {5, coordinator 2}.
    let commit = encode_event(&EventMsg {
        version: WIRE_VERSION,
        sig: None,
        event: NetEvent::OwnershipTransfer {
            id,
            new_owner: PeerId(3),
            seq: OwnerSeq {
                seq: 5,
                coordinator: PeerId(2),
            },
        },
    })
    .expect("encode");
    d.repl.apply_events(&mut d.world, PeerId(2), &commit);
    assert_eq!(
        d.owner(proxy),
        PeerId(3),
        "committed to owner 3 at rank (5,2)"
    );

    // The FORMER owner (PeerId(1)) sends a stale ResyncSpawn at a LOWER rank {3,1}
    // asserting itself — must be dropped by the `seq >= rec.owner_seq` gate.
    let stale = encode_event(&EventMsg {
        version: WIRE_VERSION,
        sig: None,
        event: NetEvent::ResyncSpawn {
            id,
            pos: quantize_vec2(99.0, 99.0),
            vel: quantize_vec2(0.0, 0.0),
            seq: OwnerSeq {
                seq: 3,
                coordinator: PeerId(1),
            },
        },
    })
    .expect("encode");
    d.repl.apply_events(&mut d.world, PeerId(1), &stale);

    assert_eq!(
        d.owner(proxy),
        PeerId(3),
        "stale former-owner resync does NOT revert the committed owner"
    );
    assert!(
        approx(d.pos(proxy).x, 1.0),
        "and does not clobber the committed proxy's state"
    );
}

/// AK-K5 — the same-owner value heal accepts REGARDLESS of rank: a `ResyncSpawn`
/// from the CURRENT owner (`from == owner`) snaps state even at a LOWER rank than
/// the proxy holds, and leaves owner + rank untouched (the stale-silent-value
/// path — distinct from the rank-gated owner-change path).
#[test]
fn resync_value_heal_ignores_lower_rank() {
    let mut src = TestPeer::new(1); // spawns e
    let mut d = TestPeer::new(4);
    let e = src.spawn(5.0, 5.0, 0.0, 0.0);
    d.deliver_all(src.id, &src.collect_for(d.id));
    let proxy = d.entity_owned_by(src.id);
    let id = net_id(src.id, e);

    // Move D's proxy to owner X=PeerId(9) at rank {3, 2}.
    let x = PeerId(9);
    let t = encode_event(&EventMsg {
        version: WIRE_VERSION,
        sig: None,
        event: NetEvent::OwnershipTransfer {
            id,
            new_owner: x,
            seq: OwnerSeq {
                seq: 3,
                coordinator: PeerId(2),
            },
        },
    })
    .expect("encode");
    d.repl.apply_events(&mut d.world, PeerId(2), &t);
    assert_eq!(d.owner(proxy), x);

    // Corrupt the value (a silent divergence).
    d.world.get_mut::<Position>(proxy).unwrap().x = 99.0;

    // A resync from the CURRENT owner X at a LOWER rank {1, 9} — the same-owner
    // value heal snaps regardless of rank; owner + rank are untouched.
    let heal = encode_event(&EventMsg {
        version: WIRE_VERSION,
        sig: None,
        event: NetEvent::ResyncSpawn {
            id,
            pos: quantize_vec2(5.0, 5.0),
            vel: quantize_vec2(0.0, 0.0),
            seq: OwnerSeq {
                seq: 1,
                coordinator: x,
            },
        },
    })
    .expect("encode");
    d.repl.apply_events(&mut d.world, x, &heal);

    assert!(
        approx(d.pos(proxy).x, 5.0),
        "same-owner value heal restores the value despite a lower rank"
    );
    assert_eq!(d.owner(proxy), x, "owner unchanged by a value heal");
    assert_eq!(
        d.repl.owner_seq(proxy),
        Some(OwnerSeq {
            seq: 3,
            coordinator: PeerId(2)
        }),
        "rank unchanged by a value heal"
    );
}

// ═════ Group AK-H — coordinator claim/commit/reject handshake (ADR-0025 A) ═════

/// A coordinator-arbitration mesh: O=PeerId(4) owns e; coord=PeerId(1) is the
/// LOWEST live peer (the coordinator); x=PeerId(2) and y=PeerId(3) are claimants.
/// Every peer tracks the other three (so all agree coordinator == 1), and
/// coord/x/y each hold a proxy for e. Returns
/// (o, coord, x, y, e, proxy_coord, proxy_x, proxy_y).
#[allow(clippy::type_complexity)]
fn coord_mesh() -> (
    TestPeer,
    TestPeer,
    TestPeer,
    TestPeer,
    Entity,
    Entity,
    Entity,
    Entity,
) {
    let mut o = TestPeer::new(4);
    let mut coord = TestPeer::new(1);
    let mut x = TestPeer::new(2);
    let mut y = TestPeer::new(3);
    let e = o.spawn(1.0, 2.0, 0.0, 0.0);
    o.track(coord.id);
    o.track(x.id);
    o.track(y.id);
    let outs = o.collect_all();
    coord.deliver_all(o.id, outbox_for(&outs, coord.id).unwrap());
    x.deliver_all(o.id, outbox_for(&outs, x.id).unwrap());
    y.deliver_all(o.id, outbox_for(&outs, y.id).unwrap());
    let proxy_coord = coord.entity_owned_by(o.id);
    let proxy_x = x.entity_owned_by(o.id);
    let proxy_y = y.entity_owned_by(o.id);
    // Membership: everyone tracks everyone (so all compute coordinator == 1).
    coord.track(o.id);
    coord.track(x.id);
    coord.track(y.id);
    x.track(o.id);
    x.track(coord.id);
    x.track(y.id);
    y.track(o.id);
    y.track(coord.id);
    y.track(x.id);
    (o, coord, x, y, e, proxy_coord, proxy_x, proxy_y)
}

/// Route a claim from `claimant` to the coordinator (asserting it targets `coord`).
fn route_claim(claimant: &mut TestPeer, proxy: Entity, coord: &mut TestPeer) {
    if let Some((target, bytes)) = claimant.repl.claim_ownership(&mut claimant.world, proxy) {
        assert_eq!(target, coord.id, "a claim routes to the coordinator");
        coord
            .repl
            .apply_events(&mut coord.world, claimant.id, &bytes);
    }
}

/// Deliver each directed message from the coordinator to whichever peer it targets.
fn route_all(msgs: &[(PeerId, Box<[u8]>)], from: PeerId, peers: &mut [&mut TestPeer]) {
    for (target, bytes) in msgs {
        if let Some(p) = peers.iter_mut().find(|p| p.id == *target) {
            p.repl.apply_events(&mut p.world, from, bytes);
        }
    }
}

/// AK-H1 — two SIMULTANEOUS claims resolve to exactly ONE committed owner by
/// coordinator sequence number, the loser is explicitly rejected, the prior owner
/// demotes (no double authority). The 145/148 acceptance.
#[test]
fn two_claims_resolve_to_one_committed_owner() {
    let (mut o, mut coord, mut x, mut y, e, proxy_coord, proxy_x, proxy_y) = coord_mesh();

    // X and Y both claim e; each routes to the coordinator (peer 1).
    route_claim(&mut x, proxy_x, &mut coord);
    route_claim(&mut y, proxy_y, &mut coord);

    // The coordinator arbitrates: winner = the lowest-id claimant = X(2).
    let msgs = coord.repl.drain_commits(&mut coord.world);

    // Inspect the directed output BEFORE routing it.
    let mut commit_owners = Vec::new();
    let mut rejects_to = Vec::new();
    for (target, bytes) in &msgs {
        match decode_event(bytes).unwrap().event {
            NetEvent::OwnershipCommit { new_owner, .. } => commit_owners.push(new_owner),
            NetEvent::ClaimRejected { .. } => rejects_to.push(*target),
            _ => {}
        }
    }
    assert!(
        !commit_owners.is_empty() && commit_owners.iter().all(|w| *w == x.id),
        "every commit names exactly the one winner X"
    );
    assert_eq!(rejects_to, vec![y.id], "exactly the loser Y is rejected");

    route_all(&msgs, coord.id, &mut [&mut o, &mut x, &mut y]);

    // Outcome: X wins (Local), Y re-tags to X, the coordinator's proxy re-tags,
    // and the prior owner O demotes — no double authority anywhere.
    assert_eq!(x.owner(proxy_x), x.id, "winner X assumes authority");
    assert_eq!(y.owner(proxy_y), x.id, "loser Y re-tags to the winner X");
    assert_eq!(
        coord.owner(proxy_coord),
        x.id,
        "coordinator's proxy re-tags to X"
    );
    assert_eq!(
        o.owner(e),
        x.id,
        "prior owner O demotes (no double authority)"
    );
    let _ = e;
}

/// AK-H2 — no authority is assumed PRE-COMMIT: a claim flips nothing locally; the
/// claimant stays a remote proxy until it receives its own commit.
#[test]
fn claim_assumes_no_authority_pre_commit() {
    let (o, _coord, mut x, _y, _e, _pc, proxy_x, _py) = coord_mesh();
    assert_eq!(x.owner(proxy_x), o.id, "before: X's proxy is owned by O");

    let routed = x.repl.claim_ownership(&mut x.world, proxy_x);
    assert!(routed.is_some(), "X (not the coordinator) routes a claim");
    assert_eq!(
        x.owner(proxy_x),
        o.id,
        "X assumes NO authority pre-commit — its Owner is unchanged"
    );
}

/// AK-H3 — a rejected loser can RE-CLAIM and win a later round: Y loses round 1,
/// then re-claims as the sole claimant in round 2 and wins (its `{2,coord}`
/// commit outranks X's `{1,coord}`), demoting the former winner X.
#[test]
fn rejected_claimant_can_reclaim_and_win() {
    let (mut o, mut coord, mut x, mut y, _e, _pc, proxy_x, proxy_y) = coord_mesh();

    // Round 1: X and Y claim; X (lower id) wins, Y is rejected.
    route_claim(&mut x, proxy_x, &mut coord);
    route_claim(&mut y, proxy_y, &mut coord);
    let msgs = coord.repl.drain_commits(&mut coord.world);
    route_all(&msgs, coord.id, &mut [&mut o, &mut x, &mut y]);
    assert_eq!(x.owner(proxy_x), x.id, "round 1: X wins");
    assert_eq!(y.owner(proxy_y), x.id, "round 1: Y re-tags to X");

    // Round 2: the rejected Y re-claims — now the SOLE claimant — and wins.
    route_claim(&mut y, proxy_y, &mut coord);
    let msgs = coord.repl.drain_commits(&mut coord.world);
    route_all(&msgs, coord.id, &mut [&mut o, &mut x, &mut y]);
    assert_eq!(
        y.owner(proxy_y),
        y.id,
        "round 2: the re-claiming Y now wins"
    );
    assert_eq!(
        x.owner(proxy_x),
        y.id,
        "and the former winner X demotes to Y"
    );
}

/// AK-H4 — a claim mis-routed to a NON-coordinator is ignored (records nothing,
/// arbitrates nothing) — only the lowest live peer arbitrates.
#[test]
fn claim_to_non_coordinator_is_ignored() {
    let (o, _coord, mut x, _y, e, _pc, _px, _py) = coord_mesh();
    // X=PeerId(2) is not the coordinator (peer 1 is). Hand it a claim directly.
    let id = net_id(o.id, e);
    let claim = encode_event(&EventMsg {
        version: WIRE_VERSION,
        sig: None,
        event: NetEvent::ClaimOwnership { id },
    })
    .expect("encode");
    x.repl.apply_events(&mut x.world, PeerId(3), &claim);
    assert!(
        !x.repl.has_pending_claim(id),
        "a claim to a non-coordinator records nothing"
    );
    assert!(
        x.repl.drain_commits(&mut x.world).is_empty(),
        "a non-coordinator arbitrates nothing"
    );
}

/// AK-H5 — coordinator-migration robustness: equal-seq commits from an OLD
/// (lower-id) and a NEW (higher-id) coordinator resolve toward the NEWER
/// coordinator via the `OwnerSeq` tiebreak, regardless of arrival order.
#[test]
fn newer_coordinator_commit_wins_equal_seq_tie() {
    let mut o = TestPeer::new(9); // spawner/owner (id irrelevant here)
    let mut d = TestPeer::new(5); // observer
    let e = o.spawn(0.0, 0.0, 0.0, 0.0);
    d.deliver_all(o.id, &o.collect_for(d.id));
    let proxy = d.entity_owned_by(o.id);
    let id = net_id(o.id, e);

    let commit = |new_owner: PeerId, coordinator: PeerId| {
        encode_event(&EventMsg {
            version: WIRE_VERSION,
            sig: None,
            event: NetEvent::OwnershipCommit {
                id,
                new_owner,
                seq: OwnerSeq {
                    seq: 5,
                    coordinator,
                },
            },
        })
        .expect("encode")
    };

    // Old coordinator (id 1) commits {5,1} -> owner 7.
    d.repl
        .apply_events(&mut d.world, PeerId(1), &commit(PeerId(7), PeerId(1)));
    assert_eq!(d.owner(proxy), PeerId(7));

    // New coordinator (id 2) commits {5,2} at the SAME seq -> wins the tie -> 8.
    d.repl
        .apply_events(&mut d.world, PeerId(2), &commit(PeerId(8), PeerId(2)));
    assert_eq!(
        d.owner(proxy),
        PeerId(8),
        "the newer (higher-id) coordinator wins the equal-seq tie"
    );

    // Re-deliver the OLD coordinator's commit (reorder) — dropped (lower rank).
    d.repl
        .apply_events(&mut d.world, PeerId(1), &commit(PeerId(7), PeerId(1)));
    assert_eq!(
        d.owner(proxy),
        PeerId(8),
        "the stale old-coordinator commit is dropped"
    );
}

/// AK-H6 — a claim the coordinator CANNOT arbitrate (an entity it does not track —
/// e.g. outside its AOI) is REJECTED, not silently black-holed, so the claimant
/// re-routes/retries instead of hanging (auditor MAJOR).
#[test]
fn unarbitrable_claim_is_rejected_not_blackholed() {
    let mut coord = TestPeer::new(1); // lowest id → the coordinator
    let claimant = PeerId(2);
    coord.track(claimant); // membership: coordinator(1) == 1
    // A claim for an id the coordinator has never seen (no proxy → un-arbitrable).
    let id = NetEntityId {
        spawner: PeerId(9),
        index: 42,
        generation: 0,
    };
    let claim = encode_event(&EventMsg {
        version: WIRE_VERSION,
        sig: None,
        event: NetEvent::ClaimOwnership { id },
    })
    .expect("encode");
    coord.repl.apply_events(&mut coord.world, claimant, &claim);
    assert!(
        coord.repl.has_pending_claim(id),
        "the coordinator recorded the claim"
    );

    let msgs = coord.repl.drain_commits(&mut coord.world);
    let rejected: Vec<PeerId> = msgs
        .iter()
        .filter(|(_, b)| {
            matches!(
                decode_event(b).unwrap().event,
                NetEvent::ClaimRejected { .. }
            )
        })
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(
        rejected,
        vec![claimant],
        "the un-arbitrable claim is explicitly rejected (no silent black-hole)"
    );
    assert!(
        msgs.iter().all(|(_, b)| matches!(
            decode_event(b).unwrap().event,
            NetEvent::ClaimRejected { .. }
        )),
        "and NO commit is emitted for an entity we can't arbitrate"
    );
}

// ═════ Group INT — cross-owner interactions over real replication (ADR-0027) ═════

/// Run `resolve_interactions` once on a peer's world.
fn resolve_once(peer: &mut TestPeer) {
    let mut s = Schedule::default();
    s.add_systems(resolve_interactions);
    s.run(&mut peer.world);
}

/// Make an entity a coarse interactor (radius) with a zeroed contact tally.
fn make_interactable(peer: &mut TestPeer, e: Entity, radius: f32) {
    peer.world
        .entity_mut(e)
        .insert((Interactable { radius }, Contacts::default()));
}

fn contacts(peer: &TestPeer, e: Entity) -> u32 {
    peer.world.get::<Contacts>(e).map(|c| c.0).unwrap_or(0)
}

/// INT-cross-owner-decider — A(owner P) and B(owner Q) overlap over REAL
/// replication (each holds the other as a snap-applied proxy). Exactly ONE peer
/// decides each entity's contact: P applies A's (it owns A) and NOT B's proxy;
/// Q applies B's and NOT A's proxy. The 145 acceptance: one deciding authority
/// per effect, no cross-owner write.
#[test]
fn cross_owner_contact_decided_by_the_affected_owner() {
    let mut p = TestPeer::new(1);
    let mut q = TestPeer::new(2);
    let a = p.spawn(0.0, 0.0, 0.0, 0.0); // P owns A at origin
    let b = q.spawn(0.5, 0.0, 0.0, 0.0); // Q owns B, overlapping A

    // Replicate positions both ways so each peer holds the other as a proxy.
    q.deliver_all(p.id, &p.collect_for(q.id));
    p.deliver_all(q.id, &q.collect_for(p.id));
    let proxy_b = p.entity_owned_by(q.id); // B's proxy on P
    let proxy_a = q.entity_owned_by(p.id); // A's proxy on Q

    // Both peers mark both entities interactable (local gameplay state — Contacts
    // is not on the wire; a receiver attaches it to the proxy).
    make_interactable(&mut p, a, 1.0);
    make_interactable(&mut p, proxy_b, 1.0);
    make_interactable(&mut q, b, 1.0);
    make_interactable(&mut q, proxy_a, 1.0);

    resolve_once(&mut p);
    resolve_once(&mut q);

    assert_eq!(
        contacts(&p, a),
        1,
        "P decides + applies A's contact (it owns A)"
    );
    assert_eq!(
        contacts(&p, proxy_b),
        0,
        "P does NOT apply B's contact — B's owner Q does (no cross-owner write)"
    );
    assert_eq!(contacts(&q, b), 1, "Q decides + applies B's contact");
    assert_eq!(contacts(&q, proxy_a), 0, "Q does NOT apply A's contact");
}

/// INT-no-resim — resolving an interaction only READS the other entity's
/// replicated state: after `resolve_interactions` on P, B's proxy Position AND
/// Velocity are BIT-IDENTICAL to before (never re-simulated / written).
#[test]
fn interaction_never_resimulates_the_remote_entity() {
    let mut p = TestPeer::new(1);
    let mut q = TestPeer::new(2);
    let a = p.spawn(0.0, 0.0, 0.0, 0.0);
    q.spawn(0.5, 0.0, 3.0, 7.0); // Q owns B with a distinctive velocity

    q.deliver_all(p.id, &p.collect_for(q.id));
    p.deliver_all(q.id, &q.collect_for(p.id));
    let proxy_b = p.entity_owned_by(q.id);
    make_interactable(&mut p, a, 1.0);
    make_interactable(&mut p, proxy_b, 1.0);

    let before_pos = p.pos(proxy_b);
    let before_vel = *p.world.get::<Velocity>(proxy_b).unwrap();

    resolve_once(&mut p);

    let after_pos = p.pos(proxy_b);
    let after_vel = *p.world.get::<Velocity>(proxy_b).unwrap();
    assert_eq!(
        (before_pos.x.to_bits(), before_pos.y.to_bits()),
        (after_pos.x.to_bits(), after_pos.y.to_bits()),
        "the remote proxy's Position is untouched by the interaction (read-only)"
    );
    assert_eq!(
        (before_vel.x.to_bits(), before_vel.y.to_bits()),
        (after_vel.x.to_bits(), after_vel.y.to_bits()),
        "and its Velocity is never re-integrated"
    );
}

// ═════ Group SM — coordinator sole-minter push (ADR-0028 a) ═════

/// Route an owner's PUSH handoff request to the coordinator (asserting it targets
/// `coord`).
fn route_transfer_request(owner: &mut TestPeer, entity: Entity, to: PeerId, coord: &mut TestPeer) {
    if let Some((target, bytes)) = owner.repl.request_transfer(&mut owner.world, entity, to) {
        assert_eq!(
            target, coord.id,
            "a transfer-request routes to the coordinator"
        );
        coord.repl.apply_events(&mut coord.world, owner.id, &bytes);
    }
}

/// SM-converge — a concurrent PUSH (owner O requests →X) and PULL (Y claims) on
/// ONE entity, both routed to the coordinator, resolve to a SINGLE committed owner
/// with a SINGLE coordinator-minted rank — the push/pull double-mint collision
/// cannot arise (the coordinator is the sole minter). The 148 acceptance.
#[test]
fn concurrent_push_and_pull_converge_to_one_owner() {
    let (mut o, mut coord, mut x, mut y, e, proxy_coord, proxy_x, proxy_y) = coord_mesh();

    // O (owner, NOT the coordinator) requests a push to X; concurrently Y pulls.
    route_transfer_request(&mut o, e, x.id, &mut coord);
    route_claim(&mut y, proxy_y, &mut coord);
    assert_eq!(
        o.owner(e),
        o.id,
        "the requesting owner assumes NOTHING pre-commit (no local flip/mint)"
    );

    // The coordinator mints exactly ONE commit; winner = elect({X=2, Y=3}) = X.
    let msgs = coord.repl.drain_commits(&mut coord.world);
    let commit_owners: Vec<PeerId> = msgs
        .iter()
        .filter_map(|(_, b)| match decode_event(b).unwrap().event {
            NetEvent::OwnershipCommit { new_owner, .. } => Some(new_owner),
            _ => None,
        })
        .collect();
    assert!(
        !commit_owners.is_empty() && commit_owners.iter().all(|w| *w == x.id),
        "a single committed owner across the mesh: X"
    );

    route_all(&msgs, coord.id, &mut [&mut o, &mut x, &mut y]);

    // Everyone converges to owner X (a single owner — no double-authority), and the
    // rank is the SINGLE coordinator mint {1, coord} (not two colliding {1,·}).
    assert_eq!(o.owner(e), x.id, "prior owner O converges to X");
    assert_eq!(coord.owner(proxy_coord), x.id);
    assert_eq!(x.owner(proxy_x), x.id, "X assumes authority");
    assert_eq!(y.owner(proxy_y), x.id, "loser Y converges to X");
    assert_eq!(
        x.repl.owner_seq(proxy_x),
        Some(OwnerSeq {
            seq: 1,
            coordinator: coord.id
        }),
        "a single coordinator-minted rank (no push/pull double-mint)"
    );
}

/// SM-route — a NON-coordinator owner's `request_transfer` routes to the
/// coordinator, flips NO `Owner`, and mints NO local rank (the sole-minter
/// guarantee); the coordinator records it. A self-coordinator records directly.
#[test]
fn transfer_request_routes_and_never_mints_locally() {
    let (mut o, mut coord, x, _y, e, _pc, _px, _py) = coord_mesh();
    let id = net_id(o.id, e);
    let seq_before = o.repl.owner_seq(e);

    let routed = o.repl.request_transfer(&mut o.world, e, x.id);
    assert_eq!(
        routed.as_ref().map(|(t, _)| *t),
        Some(coord.id),
        "a non-coordinator owner routes the request to the coordinator"
    );
    assert_eq!(o.owner(e), o.id, "no Owner flip pre-commit");
    assert_eq!(
        o.repl.owner_seq(e),
        seq_before,
        "no LOCAL rank mint — only the coordinator mints (sole-minter)"
    );
    let (_, bytes) = routed.expect("routed");
    coord.repl.apply_events(&mut coord.world, o.id, &bytes);
    assert!(
        coord.repl.has_pending_transfer_request(id),
        "the coordinator recorded the transfer-request"
    );

    // Self-coordinator: coord owns its own entity and requests a transfer → records
    // directly (no self-send), returns None.
    let ce = coord.spawn(0.0, 0.0, 0.0, 0.0);
    let _ = coord.collect_all(); // mint ce's id
    let cid = net_id(coord.id, ce);
    let self_routed = coord.repl.request_transfer(&mut coord.world, ce, x.id);
    assert!(
        self_routed.is_none(),
        "the coordinator records its own request"
    );
    assert!(coord.repl.has_pending_transfer_request(cid));
}

// ═════ Group MC — membership reconciliation (ADR-0028 b) ═════

/// MC-reconcile — a split view converges to ONE coordinator once membership
/// reconciles via the AUTHORITATIVE `poll_peers` signal (modeled here by
/// `track_peer`, as the pump calls on a Connected). B (id 3) knows only a
/// HIGHER-id peer S=5, so it self-elects as coordinator; on partition-heal it
/// tracks the lower-id A=2 and DEFERS to A (routes claims to it) — the
/// deterministic `coordinator()` resolves the split with no consensus protocol.
/// (No observed-traffic belt: `apply_events` must never resurrect a departed
/// peer — untrack is one-shot; `poll_peers` is the sole membership authority.)
#[test]
fn membership_reconciles_to_the_lower_coordinator() {
    let mut s = TestPeer::new(5); // a HIGHER-id peer B is connected to
    let mut b = TestPeer::new(3);
    s.spawn(0.0, 0.0, 0.0, 0.0);
    b.deliver_all(s.id, &s.collect_for(b.id));
    let proxy = b.entity_owned_by(s.id);
    b.track(s.id); // B tracks S via poll_peers (S=5 > B=3, so B is still the lowest it knows)

    // SPLIT (B partitioned from the lower-id A): B is the lowest peer it knows, so
    // it self-elects → a claim records LOCALLY (returns None).
    let first = b.repl.claim_ownership(&mut b.world, proxy);
    assert!(
        first.is_none(),
        "split: B is the lowest peer it knows → its own coordinator"
    );

    // HEAL: poll_peers reports the lower-id A=2 Connected → B tracks it.
    b.track(PeerId(2));

    // RECONCILED: a fresh claim now ROUTES to the lower coordinator A instead of
    // self-arbitrating — the split resolved to one coordinator.
    let second = b.repl.claim_ownership(&mut b.world, proxy);
    assert_eq!(
        second.map(|(t, _)| t),
        Some(PeerId(2)),
        "reconciled: B defers claims to the lower-id coordinator A"
    );
}

// ═════ Group CAP — AOI size-cap: bound the state datagram to one MTU (ADR-0029) ═════

/// Despawn ids in an outbox's events.
fn despawn_ids(outbox: &Outbox) -> Vec<NetEntityId> {
    outbox
        .events
        .iter()
        .filter_map(|e| match decode_event(e).unwrap().event {
            NetEvent::Despawn { id } => Some(id),
            _ => None,
        })
        .collect()
}

/// CAP-fits + CAP-nearest — a scene far past one datagram's worth of entities is
/// capped so the state datagram fits `SAFE_DATAGRAM_BYTES`, keeping the NEAREST to
/// the AOI center and dropping the farthest.
#[test]
fn aoi_cap_bounds_datagram_and_keeps_the_nearest() {
    let mut a = TestPeer::new(1);
    let b = TestPeer::new(2);
    let near = a.spawn(0.0, 0.0, 0.0, 0.0);
    for i in 1..199 {
        a.spawn(i as f32, 0.0, 0.0, 0.0);
    }
    let far = a.spawn(1000.0, 0.0, 0.0, 0.0); // the farthest from the center
    a.set_aoi(b.id, (0.0, 0.0), 5000.0); // radius covers all 200
    let out = a.collect_for(b.id);

    let bytes = out.state.as_ref().expect("state present").len();
    assert!(
        bytes <= replication::SAFE_DATAGRAM_BYTES,
        "state datagram {bytes} B must fit the {}-B budget",
        replication::SAFE_DATAGRAM_BYTES
    );
    let ids: Vec<NetEntityId> = state_entries(&out).iter().map(|s| s.id).collect();
    assert!(
        ids.len() < 200,
        "capped below the full set (kept {})",
        ids.len()
    );
    assert!(!ids.is_empty(), "keeps at least the nearest entities");
    assert!(
        ids.contains(&net_id(a.id, near)),
        "the NEAREST entity (x=0) is kept"
    );
    assert!(
        !ids.contains(&net_id(a.id, far)),
        "the FARTHEST entity (x=1000) is capped out"
    );
}

/// CAP-readcheat — a capped-out entity's EXISTENCE is withheld (no Spawn): the cap
/// only TIGHTENS visibility, so the Mode-3 read-cheat defense can only strengthen.
#[test]
fn aoi_cap_withholds_existence_of_capped_entities() {
    let mut a = TestPeer::new(1);
    let b = TestPeer::new(2);
    let near = a.spawn(0.0, 0.0, 0.0, 0.0);
    for i in 1..199 {
        a.spawn(i as f32, 0.0, 0.0, 0.0);
    }
    let far = a.spawn(1000.0, 0.0, 0.0, 0.0);
    a.set_aoi(b.id, (0.0, 0.0), 5000.0);
    let out = a.collect_for(b.id);

    let spawned = spawn_ids(&out.events);
    assert!(
        !spawned.contains(&net_id(a.id, far)),
        "a capped-out entity is never Spawned (existence withheld)"
    );
    assert!(
        spawned.contains(&net_id(a.id, near)),
        "a kept entity IS Spawned"
    );
}

/// CAP-fastpath — a small scene (well under budget) is unaffected: every entity is
/// sent and NOTHING is despawned (the cap's fast path is a no-op).
#[test]
fn aoi_cap_fastpath_small_scene_unaffected() {
    let mut a = TestPeer::new(1);
    let b = TestPeer::new(2);
    for i in 0..8 {
        a.spawn(i as f32, 0.0, 0.0, 0.0);
    }
    a.set_aoi(b.id, (0.0, 0.0), 1000.0);
    let out = a.collect_for(b.id);
    assert_eq!(
        state_entries(&out).len(),
        8,
        "a small scene sends all 8 entities (no cap)"
    );
    assert_eq!(
        despawn_count(&out),
        0,
        "no spurious despawn on the fast path"
    );
}

/// CAP-evicts-known — when the scene grows past budget, a now-far KNOWN entity is
/// evicted via the audited AOI-EXIT path (Despawn + baseline drop → sound
/// re-baseline on re-entry), NOT a silent state-entry deferral.
#[test]
fn aoi_cap_evicts_a_now_far_known_entity_via_despawn() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let outer = a.spawn(500.0, 0.0, 0.0, 0.0); // initially the only entity → known
    a.set_aoi(b.id, (0.0, 0.0), 5000.0);
    b.deliver_all(a.id, &a.collect_for(b.id)); // b now knows `outer`

    // Crowd the center so `outer` (x=500) becomes one of the FARTHEST → capped out.
    for i in 0..200 {
        a.spawn(i as f32 * 0.1, 0.0, 0.0, 0.0);
    }
    let out = a.collect_for(b.id);
    assert!(
        despawn_ids(&out).contains(&net_id(a.id, outer)),
        "the now-far known entity is evicted via Despawn (AOI-exit), not silently deferred"
    );
}

/// CAP-unbounded — an UNBOUNDED peer (no AOI set) with more than a datagram's worth
/// of owned entities is still capped (by NetEntityId order) so its datagram fits.
#[test]
fn aoi_cap_bounds_an_unbounded_peer() {
    let mut a = TestPeer::new(1);
    let b = TestPeer::new(2);
    for _ in 0..200 {
        a.spawn(0.0, 0.0, 0.0, 0.0);
    }
    // No set_aoi → unbounded (sees all owned).
    let out = a.collect_for(b.id);
    let bytes = out.state.as_ref().expect("state present").len();
    assert!(
        bytes <= replication::SAFE_DATAGRAM_BYTES,
        "an unbounded peer's datagram {bytes} B must still fit the budget"
    );
    assert!(
        state_entries(&out).len() < 200,
        "capped below the full owned set"
    );
}

/// CAP-determinism — two collects of the same over-budget scene keep the IDENTICAL
/// set (deterministic distance rank + NetEntityId tiebreak).
#[test]
fn aoi_cap_is_deterministic() {
    let mut a = TestPeer::new(1);
    let b = TestPeer::new(2);
    for i in 0..200 {
        a.spawn(i as f32, 0.0, 0.0, 0.0);
    }
    a.set_aoi(b.id, (0.0, 0.0), 5000.0);
    let mut ids1: Vec<NetEntityId> = state_entries(&a.collect_for(b.id))
        .iter()
        .map(|s| s.id)
        .collect();
    let mut ids2: Vec<NetEntityId> = state_entries(&a.collect_for(b.id))
        .iter()
        .map(|s| s.id)
        .collect();
    ids1.sort();
    ids2.sort();
    assert_eq!(
        ids1, ids2,
        "the cap keeps a deterministic set across collects"
    );
}
