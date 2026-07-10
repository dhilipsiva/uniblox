//! Tier B — deterministic two/three-World replication tests (T9–T25).
//! Locked FIRST (TDD). Bytes are hand-ferried between worlds; reordering,
//! loss, and duplication are expressed by delivery order — no transport, no
//! timing, fully deterministic.

use bevy_ecs::prelude::*;
use engine_core::{Owner, Position, Velocity, insert_sim, simulate, spawn_owned};
use protocol::{NetEntityId, NetEvent, PeerId, StateEntry, decode_event, decode_state};
use replication::{KEYFRAME_INTERVAL, Outbox, Replication};

const DT: f32 = 0.5;
/// Quantization tolerance: 1/2048.
const TOL: f32 = 0.5 / 1024.0;

struct TestPeer {
    id: PeerId,
    world: World,
    schedule: Schedule,
    repl: Replication,
}

impl TestPeer {
    fn new(id: u64) -> Self {
        let mut world = World::new();
        insert_sim(&mut world, PeerId(id), DT);
        let mut schedule = Schedule::default();
        schedule.add_systems(simulate);
        let repl = Replication::new(&mut world);
        TestPeer {
            id: PeerId(id),
            world,
            schedule,
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

    fn collect(&mut self) -> Outbox {
        self.repl.collect(&mut self.world)
    }

    fn deliver_state(&mut self, from: PeerId, bytes: &[u8]) {
        self.repl.apply_state(&mut self.world, from, bytes);
    }

    fn deliver_events(&mut self, from: PeerId, events: &[Box<[u8]>]) {
        for ev in events {
            self.repl.apply_events(&mut self.world, from, ev);
        }
    }

    /// Deliver a whole outbox (events first — spawn-before-state is the
    /// common case; tests that need the adverse order do it by hand).
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

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() <= TOL
}

// ───────────────────────────── T9 ─────────────────────────────

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
        let out_a = a.collect();
        let out_b = b.collect();
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

// ───────────────────────────── T10 ─────────────────────────────

/// T10 — the receiver NEVER re-simulates a remote entity: with replicated
/// nonzero velocity and no further delivery, the proxy stays bit-identical.
#[test]
fn receiver_never_resimulates_remote() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(1.0, 1.0, 2.0, 2.0);
    let out = a.collect();
    b.deliver_all(a.id, &out);

    let proxy = b.entity_owned_by(a.id);
    let before = b.pos(proxy);
    for _ in 0..10 {
        b.sim_tick(); // Remote arm must be a no-op
    }
    let after = b.pos(proxy);
    assert_eq!(before.x.to_bits(), after.x.to_bits());
    assert_eq!(before.y.to_bits(), after.y.to_bits());
}

// ───────────────────────────── T11 ─────────────────────────────

/// T11 ★ THE echo-back test: applying remote state fires Bevy change
/// detection, but the receiver's next collect must emit NOTHING (authority
/// gate first, never Changed alone).
#[test]
fn applied_state_never_echoed() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(1.0, 1.0, 2.0, 0.0);
    let out = a.collect();
    b.deliver_all(a.id, &out);

    let out_b = b.collect();
    assert!(out_b.state.is_none(), "B must not echo A's applied state");
    assert!(out_b.events.is_empty(), "B must not announce A's proxy");
}

// ───────────────────────────── T12 ─────────────────────────────

/// T12 ★ the mask tracks exactly the changed component set.
#[test]
fn mask_tracks_single_component_change() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(1.0, 1.0, 0.0, 0.0);
    let out = a.collect();
    b.deliver_all(a.id, &out);

    // Drain: nothing changed since the first collect.
    let out = a.collect();
    assert!(out.state.is_none(), "no change -> no state message");

    // Mutate ONLY Position.
    a.world.get_mut::<Position>(e).unwrap().x = 5.0;
    let entries = state_entries(&a.collect());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].mask(), 0b01);
    assert!(entries[0].pos.is_some() && entries[0].vel.is_none());

    // Mutate ONLY Velocity.
    a.world.get_mut::<Velocity>(e).unwrap().y = 3.0;
    let entries = state_entries(&a.collect());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].mask(), 0b10);
    assert!(entries[0].pos.is_none() && entries[0].vel.is_some());
}

// ───────────────────────────── T13/T14 ─────────────────────────────

/// T13 ★ out-of-order delivery: newest-seq wins, an older msg arriving later
/// mutates nothing.
#[test]
fn out_of_order_state_newest_wins() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(0.0, 0.0, 2.0, 0.0);

    let out1 = a.collect(); // seq 1: spawn + initial state
    a.sim_tick();
    let out2 = a.collect(); // seq 2
    a.sim_tick();
    let out3 = a.collect(); // seq 3

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
    let out = a.collect();
    b.deliver_all(a.id, &out);

    let proxy = b.entity_owned_by(a.id);
    let before = b.pos(proxy);
    let count_before = b.entity_count();
    b.deliver_state(a.id, out.state.as_ref().unwrap()); // duplicate
    assert_eq!(before.x.to_bits(), b.pos(proxy).x.to_bits());
    assert_eq!(count_before, b.entity_count());
}

// ───────────────────────────── T15 ─────────────────────────────

/// T15 — state arriving before its Spawn event is dropped (no panic, no
/// entity), then heals once the spawn lands.
#[test]
fn state_before_spawn_drops_then_heals() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(1.0, 1.0, 2.0, 0.0);
    let out1 = a.collect();

    // Adverse cross-channel order: state first.
    b.deliver_state(a.id, out1.state.as_ref().unwrap());
    assert_eq!(
        b.entity_count(),
        0,
        "state-before-spawn must not create entities"
    );

    // Spawn lands; next snapshot applies.
    b.deliver_events(a.id, &out1.events);
    a.sim_tick();
    let out2 = a.collect();
    b.deliver_state(a.id, out2.state.as_ref().unwrap());

    let proxy = b.entity_owned_by(a.id);
    let e_a = a.entity_owned_by(a.id);
    let truth = a.pos(e_a);
    assert!(approx(b.pos(proxy).x, truth.x));
}

// ───────────────────────────── T16 ─────────────────────────────

/// T16 ★ stale-generation state is NEVER misapplied: a late message addressed
/// to a despawned entity's (index, old-generation) must not touch a proxy
/// living at the same index with a NEWER generation.
///
/// NOTE (verified empirically): bevy_ecs 0.19's allocator does not promptly
/// recycle freed indices (fresh index per spawn, even after flush), so the
/// recycled-id scenario is SYNTHESIZED at the wire level — a Spawn event with
/// the same (spawner, index) but generation+1, exactly what a recycling
/// allocator (older/newer Bevy, long sessions) would mint. The property under
/// test is the RECEIVER's: full-NetEntityId keying keeps the stale entry
/// inert. Bevy's own "recycled index ⇒ higher generation" guarantee is its
/// contract, not ours.
#[test]
fn stale_generation_never_misapplied() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e1 = a.spawn(1.0, 1.0, 0.0, 0.0);
    let out1 = a.collect();
    let old_id = spawn_ids(&out1.events)[0];
    assert_eq!(old_id.index, e1.index_u32());
    b.deliver_all(a.id, &out1);

    // Capture a state message addressed to the OLD generation (undelivered).
    a.world.get_mut::<Position>(e1).unwrap().x = 2.0;
    let stale = a.collect().state.unwrap();

    // Despawn e1 and announce it.
    a.world.despawn(e1);
    let out_despawn = a.collect();
    b.deliver_events(a.id, &out_despawn.events);
    assert_eq!(b.entity_count(), 0);

    // Synthesize the recycled id: same spawner+index, HIGHER generation —
    // as if A's allocator had reused the slot for a new entity at (5, 5).
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

    // Replay the stale message with an artificially high seq so ONLY the
    // generation (not the seq gate) can save us.
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
        "stale generation misapplied"
    );
    assert_eq!(before.y.to_bits(), after.y.to_bits());
    assert_eq!(
        before.x, 5.0,
        "proxy must still hold the new entity's state"
    );
}

// ───────────────────────────── T17 ─────────────────────────────

/// T17 — despawn followed by a late state message: no resurrection.
#[test]
fn despawn_then_late_state_no_resurrection() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e1 = a.spawn(1.0, 1.0, 2.0, 0.0);
    let out1 = a.collect();
    b.deliver_all(a.id, &out1);

    a.sim_tick();
    let late = a.collect().state.unwrap(); // captured, delivered late

    a.world.despawn(e1);
    let out_despawn = a.collect();
    b.deliver_events(a.id, &out_despawn.events);
    assert_eq!(b.entity_count(), 0);

    b.deliver_state(a.id, &late);
    assert_eq!(
        b.entity_count(),
        0,
        "late state must not resurrect a despawned entity"
    );
}

// ───────────────────────────── T18 ─────────────────────────────

/// T18 ★ clean A→B ownership handoff: authority flips atomically on A, the
/// NetEntityId stays stable, B switches from apply to compute, and at no
/// point do both peers collect the entity.
#[test]
fn handoff_clean_authority_transfer() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e_a = a.spawn(0.0, 0.0, 2.0, 0.0);
    let out = a.collect();
    let original_id = spawn_ids(&out.events)[0];
    b.deliver_all(a.id, &out);
    let proxy = b.entity_owned_by(a.id);

    // B cannot give away what it does not own.
    assert!(
        b.repl
            .transfer_ownership(&mut b.world, proxy, b.id)
            .is_err()
    );

    // A transfers to B: local Owner flips the same tick; A stops collecting it.
    a.repl
        .transfer_ownership(&mut a.world, e_a, b.id)
        .expect("A owns e_a");
    assert_eq!(a.owner(e_a), b.id, "A's local Owner must flip immediately");

    // A must NOT integrate e_a anymore (Remote arm on A now) — assert the
    // freeze directly, not just the empty collect (auditor hardening).
    let frozen = a.pos(e_a);
    a.sim_tick();
    let after_tick = a.pos(e_a);
    assert_eq!(
        frozen.x.to_bits(),
        after_tick.x.to_bits(),
        "the old owner must stop computing a transferred entity"
    );
    assert_eq!(frozen.y.to_bits(), after_tick.y.to_bits());
    let out_a = a.collect();
    assert!(
        state_entries(&out_a).is_empty(),
        "A must not collect state for a transferred entity"
    );
    let has_transfer = out_a.events.iter().any(|b| {
        matches!(
            decode_event(b).unwrap().event,
            NetEvent::OwnershipTransfer { id, new_owner } if id == original_id && new_owner == PeerId(2)
        )
    });
    assert!(
        has_transfer,
        "transfer event must be queued on the reliable channel"
    );

    // B receives the transfer: proxy Owner flips, B now computes it.
    b.deliver_events(a.id, &out_a.events);
    assert_eq!(b.owner(proxy), b.id);

    let before = b.pos(proxy);
    b.sim_tick(); // Local arm on B now — the entity moves under B's authority
    let after = b.pos(proxy);
    assert!(after.x > before.x, "B must now simulate the adopted entity");

    // B's collect emits state addressed to the ORIGINAL id (spawner-stable).
    let out_b = b.collect();
    let entries = state_entries(&out_b);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].id, original_id,
        "identity must survive the transfer"
    );
}

// ───────────────────────────── T19 ─────────────────────────────

/// T19 ★ in-flight state from the OLD owner arriving at a third peer AFTER
/// the transfer event is dropped by the ownership gate (seq cannot save us —
/// the streams are incomparable).
#[test]
fn stale_old_owner_state_dropped_after_transfer() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut c = TestPeer::new(3);
    let e_a = a.spawn(0.0, 0.0, 2.0, 0.0);
    let out = a.collect();
    b.deliver_all(a.id, &out);
    c.deliver_all(a.id, &out);
    let proxy_on_c = c.entity_owned_by(a.id);

    // A captures a state msg (in flight), THEN transfers.
    a.sim_tick();
    let in_flight = a.collect().state.unwrap(); // seq 2, from A
    a.repl
        .transfer_ownership(&mut a.world, e_a, b.id)
        .expect("A owns e_a");
    let out_transfer = a.collect();
    c.deliver_events(a.id, &out_transfer.events);
    assert_eq!(c.owner(proxy_on_c), b.id);

    // The in-flight old-owner state arrives late: seq passes, ownership drops.
    let before = c.pos(proxy_on_c);
    c.deliver_state(a.id, &in_flight);
    let after = c.pos(proxy_on_c);
    assert_eq!(
        before.x.to_bits(),
        after.x.to_bits(),
        "old-owner state must be dropped"
    );
}

// ───────────────────────────── T20 ─────────────────────────────

/// T20 — state from the NEW owner arriving BEFORE the transfer event is
/// dropped; once the event lands, the next snapshot applies.
#[test]
fn early_new_owner_state_dropped_until_transfer() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let mut c = TestPeer::new(3);
    let e_a = a.spawn(0.0, 0.0, 2.0, 0.0);
    let out = a.collect();
    b.deliver_all(a.id, &out);
    c.deliver_all(a.id, &out);
    let proxy_on_c = c.entity_owned_by(a.id);

    // Full handoff on A→B; B adopts.
    a.repl
        .transfer_ownership(&mut a.world, e_a, b.id)
        .expect("A owns e_a");
    let out_transfer = a.collect();
    b.deliver_events(a.id, &out_transfer.events);

    // B (new owner) emits state; it reaches C BEFORE the transfer event.
    let proxy_on_b = b.entity_owned_by(b.id);
    b.world.get_mut::<Position>(proxy_on_b).unwrap().x = 7.0;
    let early = b.collect().state.unwrap();

    let before = c.pos(proxy_on_c);
    c.deliver_state(b.id, &early);
    assert_eq!(
        before.x.to_bits(),
        c.pos(proxy_on_c).x.to_bits(),
        "early new-owner state must drop"
    );

    // Transfer event lands; the NEXT snapshot from B applies.
    c.deliver_events(a.id, &out_transfer.events);
    b.world.get_mut::<Position>(proxy_on_b).unwrap().x = 8.0;
    let next = b.collect().state.unwrap();
    c.deliver_state(b.id, &next);
    assert!(
        approx(c.pos(proxy_on_c).x, 8.0),
        "post-transfer state must apply"
    );
}

// ───────────────────────────── T21 ─────────────────────────────

/// T21 — the keyframe heals a lost final packet: after a dropped update and
/// no further changes, exactly one keyframe within the interval re-sends full
/// state and the receiver converges.
#[test]
fn keyframe_heals_dropped_final_packet() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(1.0, 1.0, 0.0, 0.0);
    let out = a.collect();
    b.deliver_all(a.id, &out);

    // The "final" update is LOST.
    a.world.get_mut::<Position>(e).unwrap().x = 9.0;
    let _lost = a.collect();

    // Nothing changes anymore; within KEYFRAME_INTERVAL collects exactly one
    // keyframe must emit, and it heals the receiver.
    let mut emissions = 0;
    for _ in 0..=KEYFRAME_INTERVAL {
        let out = a.collect();
        if let Some(state) = &out.state {
            emissions += 1;
            b.deliver_state(a.id, state);
        }
    }
    assert_eq!(emissions, 1, "exactly one keyframe in a quiet interval");
    let proxy = b.entity_owned_by(a.id);
    assert!(
        approx(b.pos(proxy).x, 9.0),
        "keyframe must heal the lost update"
    );
}

// ───────────────────────────── T22/T23 ─────────────────────────────

/// T22 — late join: on_peer_connected replays Spawns for existing entities;
/// overlap with the original broadcast stays idempotent.
#[test]
fn late_join_spawn_replay() {
    let mut a = TestPeer::new(1);
    a.spawn(1.0, 1.0, 0.0, 0.0);
    a.spawn(2.0, 2.0, 0.0, 0.0);
    let original = a.collect(); // broadcast "to nobody" (B not connected yet)

    let mut b = TestPeer::new(2);
    let replay = a.repl.on_peer_connected(&mut a.world, b.id);
    b.deliver_events(a.id, &replay);
    assert_eq!(b.entity_count(), 2, "replay must create both proxies");

    // The original broadcast arrives too (dup) — still exactly 2.
    b.deliver_events(a.id, &original.events);
    assert_eq!(b.entity_count(), 2, "duplicate spawns must be idempotent");
}

/// T23 — the same Spawn bytes twice create exactly one proxy.
#[test]
fn duplicate_spawn_is_noop() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    a.spawn(1.0, 1.0, 0.0, 0.0);
    let out = a.collect();
    b.deliver_events(a.id, &out.events);
    b.deliver_events(a.id, &out.events);
    assert_eq!(b.entity_count(), 1);
}

// ───────────────────────────── T24 ─────────────────────────────

/// T24 — a sender may only assert state for entities it currently owns:
/// state from a non-owner is dropped, and nobody can overwrite the
/// receiver's own authoritative entities.
#[test]
fn receiver_rejects_unowned_sender_state() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let c_id = PeerId(3);

    // B has a proxy owned by A.
    a.spawn(1.0, 1.0, 0.0, 0.0);
    let out_a = a.collect();
    let a_entity_id = spawn_ids(&out_a.events)[0];
    b.deliver_all(a.id, &out_a);
    let proxy = b.entity_owned_by(a.id);

    // C claims A's entity: dropped (owner is A, sender is C).
    let forged = protocol::encode_state(&protocol::StateMsg {
        version: protocol::WIRE_VERSION,
        seq: 999,
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

    // A claims B's OWN entity: never applied over local authority.
    let e_b = b.spawn(5.0, 5.0, 0.0, 0.0);
    let out_b = b.collect(); // maps e_b so its id exists
    let b_entity_id = spawn_ids(&out_b.events)[0];
    let forged = protocol::encode_state(&protocol::StateMsg {
        version: protocol::WIRE_VERSION,
        seq: 1000,
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

// ───────────────────────────── T28 ─────────────────────────────

/// T28 (auditor test-debt) — transferring an entity that was NEVER collected
/// (no NetEntityId minted yet) must mint + announce it first: the reliable
/// channel carries Spawn THEN OwnershipTransfer, in that order, so receivers
/// can resolve the transfer. The receiver ends with one proxy owned by B.
#[test]
fn transfer_of_uncollected_entity_mints_then_transfers() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e = a.spawn(3.0, 4.0, 2.0, 0.0);

    // NO collect between spawn and transfer: the mint-on-transfer arm.
    a.repl
        .transfer_ownership(&mut a.world, e, b.id)
        .expect("A owns e");
    let out = a.collect();

    let events: Vec<NetEvent> = out
        .events
        .iter()
        .map(|bytes| decode_event(bytes).unwrap().event)
        .collect();
    let spawn_idx = events
        .iter()
        .position(|ev| matches!(ev, NetEvent::Spawn { .. }))
        .expect("a Spawn must be announced for the unmapped entity");
    let transfer_idx = events
        .iter()
        .position(|ev| matches!(ev, NetEvent::OwnershipTransfer { .. }))
        .expect("the OwnershipTransfer must be announced");
    assert!(
        spawn_idx < transfer_idx,
        "Spawn must precede Transfer on the reliable channel"
    );
    let spawn_id = match events[spawn_idx] {
        NetEvent::Spawn { id, .. } => id,
        _ => unreachable!(),
    };
    match events[transfer_idx] {
        NetEvent::OwnershipTransfer { id, new_owner } => {
            assert_eq!(id, spawn_id, "both events must address the same identity");
            assert_eq!(new_owner, b.id);
        }
        _ => unreachable!(),
    }
    // A no longer owns it at collect time — no state entry.
    assert!(state_entries(&out).is_empty());

    // Receiver resolves the pair: one proxy, owned by B, at the announced state.
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 1);
    let proxy = b.entity_owned_by(b.id);
    let pos = b.pos(proxy);
    assert!(approx(pos.x, 3.0) && approx(pos.y, 4.0));

    // B is now the authority: its sim computes the adopted entity.
    b.sim_tick();
    assert!(b.pos(proxy).x > 3.0, "B must compute the adopted entity");
}

// ───────────────────────────── T27 ─────────────────────────────

/// T27 (auditor F1 regression) — transfer-then-despawn in the SAME tick must
/// not ship the queued Transfer for a corpse: the wire never saw the transfer,
/// so the initiator (still the wire-visible owner) announces a Despawn instead,
/// and receivers end with NO entity — never an unhealable owned ghost.
#[test]
fn transfer_then_despawn_same_tick_yields_no_ghost() {
    let mut a = TestPeer::new(1);
    let mut b = TestPeer::new(2);
    let e_a = a.spawn(1.0, 1.0, 0.0, 0.0);
    let out = a.collect();
    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 1);

    // Same tick: give it away, then destroy it before collect().
    a.repl
        .transfer_ownership(&mut a.world, e_a, b.id)
        .expect("A owns e_a");
    a.world.despawn(e_a);

    let out = a.collect();
    let mut saw_transfer = false;
    let mut saw_despawn = false;
    for ev in &out.events {
        match decode_event(ev).unwrap().event {
            NetEvent::OwnershipTransfer { .. } => saw_transfer = true,
            NetEvent::Despawn { .. } => saw_despawn = true,
            NetEvent::Spawn { .. } => {}
        }
    }
    assert!(!saw_transfer, "a transfer for a dead entity must be purged");
    assert!(
        saw_despawn,
        "the initiator must announce the despawn instead"
    );

    b.deliver_all(a.id, &out);
    assert_eq!(b.entity_count(), 0, "receiver must not keep an owned ghost");
}

// ───────────────────────────── T25 ─────────────────────────────

/// T25 — first collect over a world with pre-existing owned entities emits
/// their Spawns plus a full-mask snapshot, seq == 1.
#[test]
fn first_collect_announces_and_snapshots_everything() {
    let mut world = World::new();
    insert_sim(&mut world, PeerId(1), DT);
    spawn_owned(
        &mut world,
        PeerId(1),
        Position { x: 1.0, y: 1.0 },
        Velocity { x: 0.0, y: 0.0 },
    );
    spawn_owned(
        &mut world,
        PeerId(1),
        Position { x: 2.0, y: 2.0 },
        Velocity { x: 1.0, y: 1.0 },
    );

    let mut repl = Replication::new(&mut world);
    let out = repl.collect(&mut world);

    assert_eq!(spawn_ids(&out.events).len(), 2, "both entities announced");
    let msg = decode_state(out.state.as_ref().unwrap()).unwrap();
    assert_eq!(msg.seq, 1, "first state message carries seq 1");
    assert_eq!(msg.entries.len(), 2);
    for entry in &msg.entries {
        assert_eq!(entry.mask(), 0b11, "first snapshot is full-mask");
    }
}
