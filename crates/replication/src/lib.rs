//! `replication` — the custom per-entity, authority-gated replication protocol.
//! **HIGH-RISK.** ADR-0013; see `CLAUDE.md` in this crate before touching.
//!
//! Transport-free: `protocol::PeerId` + bytes in/out; the caller pumps the
//! transport and drives the network tick (cadence is the caller's — fixed
//! sim timestep ≠ wall-clock send timing).
//!
//! Load-bearing rules (each guards a settled invariant):
//! - **Authority gates everything.** The sender collects ONLY entities where
//!   `authority_of(owner, local) == Local`; the receiver applies ONLY where it
//!   is `Remote`. There is deliberately NO `Changed<T>` query filter anywhere
//!   in this crate — change masks come from `Ref::is_changed()` AFTER the
//!   authority gate, so remote-applied writes (which also fire change
//!   detection) can never echo back.
//! - **Presence-delta, ABSOLUTE values.** A component is present iff it
//!   changed, but its value is always full quantized state — never an
//!   arithmetic delta (the state channel is lossy; arithmetic deltas compound
//!   loss into permanent drift). Phase 3's acked baselines own that.
//! - **LWW = newest-seq wins**, not latest-arrival: the state channel is
//!   unordered, so a whole message is dropped iff its seq ≤ the last seen
//!   from that sender.
//! - **Identity ≠ authority.** `NetEntityId` is spawner-stable (minted once);
//!   current authority lives only in the proxy's `Owner` component, mutated
//!   only by reliable `OwnershipTransfer` events. State from a sender that is
//!   not the CURRENT owner is dropped — the only sound arbiter for handoff
//!   races (per-sender seq streams are incomparable).
//! - **Change detection uses one cached `SystemState`** — a fresh
//!   `query_filtered::<_, Changed<T>>` in a manually-driven World anchors to
//!   `world.last_change_tick()` (which only advances on `clear_trackers()`)
//!   and silently reports everything as changed forever. The cached
//!   `SystemState` re-anchors on every fetch. NOTE for the Mode-3 server
//!   (Phase 3): a `SystemState` held outside schedules is not visited by
//!   `World::check_change_ticks()`; over ~2^31 change ticks the comparison
//!   inverts — recreate it periodically or hook check-ticks on long-lived
//!   servers.
//! - **Known accepted gaps** (documented, warn-logged, healed by Phase 3
//!   anti-entropy resync — do NOT "fix" ad hoc):
//!   - cross-SENDER event reordering after a handoff: a third peer may see
//!     the new owner's `Despawn` before the original `Spawn` (orphaned
//!     proxy), and a CHAINED handoff A→B→C may deliver T2(B→C) before
//!     T1(A→B) at a fourth peer — T2 is dropped, the proxy records owner B
//!     forever, and C's state is rejected until resync (frozen, wrong-owner
//!     proxy; no packet loss required, just cross-sender skew);
//!   - late-join replay of entities whose spawner no longer owns them.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemState;
use engine_core::{Authority, LocalPeer, Owner, Position, Velocity, authority_of, spawn_owned};
use protocol::{
    EventMsg, NetEntityId, NetEvent, PeerId, QVec2, StateEntry, StateMsg, WIRE_VERSION,
    decode_event, decode_state, dequantize, encode_event, encode_state, quantize_vec2,
};

/// A full-mask snapshot of every owned entity is forced every N collects —
/// the interim guard against "the last packet before an entity stopped moving
/// was lost, so the receiver holds a wrong final position forever" (until
/// Phase 3 acks/resync replace it).
pub const KEYFRAME_INTERVAL: u32 = 30;

/// The safe single-datagram budget for the unreliable channel: beyond this,
/// SCTP fragments the message and any one lost fragment loses ALL of it.
/// Collect warns above this size; splitting is Phase 3 (interest management).
pub const SAFE_DATAGRAM_BYTES: usize = 1150;

/// Attempted to transfer ownership of an entity this peer is not currently
/// authoritative over (or that does not exist).
#[derive(Debug)]
pub struct NotAuthoritative;

impl std::fmt::Display for NotAuthoritative {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot transfer ownership: not the current authority")
    }
}

impl std::error::Error for NotAuthoritative {}

/// One network tick's outgoing bytes. `state` is broadcast on the unreliable
/// state channel; `events` are broadcast in order on the reliable channel.
pub struct Outbox {
    pub state: Option<Box<[u8]>>,
    pub events: Vec<Box<[u8]>>,
}

struct NetIdRecord {
    id: NetEntityId,
    /// Last known owner — kept so a despawn can still be announced after the
    /// entity (and its `Owner` component) is gone from the world.
    last_owner: PeerId,
}

/// Bidirectional identity registry. Keyed by the FULL `NetEntityId` (not
/// (owner, index)): Bevy never reuses an (index, generation) pair, so a
/// stale-generation message simply finds no mapping and is inert forever —
/// no tombstones, no cross-sender ordering hazards on reuse.
#[derive(Default)]
struct NetIdMap {
    by_id: HashMap<NetEntityId, Entity>,
    by_entity: HashMap<Entity, NetIdRecord>,
}

impl NetIdMap {
    fn insert(&mut self, id: NetEntityId, entity: Entity, last_owner: PeerId) {
        self.by_id.insert(id, entity);
        self.by_entity
            .insert(entity, NetIdRecord { id, last_owner });
    }

    fn remove_by_entity(&mut self, entity: Entity) {
        if let Some(rec) = self.by_entity.remove(&entity) {
            self.by_id.remove(&rec.id);
        }
    }

    fn remove_by_id(&mut self, id: &NetEntityId) {
        if let Some(entity) = self.by_id.remove(id) {
            self.by_entity.remove(&entity);
        }
    }
}

type ChangeQuery = SystemState<
    Query<
        'static,
        'static,
        (
            Entity,
            &'static Owner,
            Ref<'static, Position>,
            Ref<'static, Velocity>,
        ),
    >,
>;

/// The replication endpoint for one World: sender + receiver + identity map.
pub struct Replication {
    change_query: ChangeQuery,
    map: NetIdMap,
    next_seq: u64,
    last_seq: HashMap<PeerId, u64>,
    ticks_since_keyframe: u32,
    pending_events: Vec<EventMsg>,
}

impl Replication {
    pub fn new(world: &mut World) -> Self {
        Replication {
            change_query: SystemState::new(world),
            map: NetIdMap::default(),
            next_seq: 1,
            last_seq: HashMap::new(),
            ticks_since_keyframe: 0,
            pending_events: Vec::new(),
        }
    }

    /// One network tick of the SENDER: lifecycle diff (spawn/despawn events),
    /// then authority-gated changed-component state entries.
    pub fn collect(&mut self, world: &mut World) -> Outbox {
        let local = world.resource::<LocalPeer>().0;

        self.ticks_since_keyframe += 1;
        let keyframe = self.ticks_since_keyframe >= KEYFRAME_INTERVAL;
        if keyframe {
            self.ticks_since_keyframe = 0;
        }

        let mut events: Vec<EventMsg> = std::mem::take(&mut self.pending_events);

        // Lifecycle: announce despawns for dead mapped entities. An entity can
        // die in the SAME tick as queued-but-unsent events about it (e.g.
        // transfer-then-despawn): those events describe a corpse and must be
        // purged, or the new owner would adopt an entity that no longer exists
        // — an unhealable OWNED ghost (auditor finding F1). The wire never saw
        // the queued transfer, so from the receivers' perspective WE are still
        // the owner and may validly announce the Despawn; a queued-but-unsent
        // Spawn means the wire never met the entity at all — announce nothing.
        let dead: Vec<Entity> = self
            .map
            .by_entity
            .keys()
            .copied()
            .filter(|e| !world.entities().contains(*e))
            .collect();
        for entity in dead {
            // Presence in by_entity is guaranteed: `dead` was built from its keys.
            if let Some(rec) = self.map.by_entity.get(&entity) {
                let id = rec.id;
                let mut purged_spawn = false;
                let mut purged_transfer = false;
                events.retain(|ev| {
                    let (subject, is_spawn, is_transfer) = match &ev.event {
                        NetEvent::Spawn { id, .. } => (*id, true, false),
                        NetEvent::Despawn { id } => (*id, false, false),
                        NetEvent::OwnershipTransfer { id, .. } => (*id, false, true),
                    };
                    if subject == id {
                        purged_spawn |= is_spawn;
                        purged_transfer |= is_transfer;
                        false
                    } else {
                        true
                    }
                });

                if purged_spawn {
                    // Never introduced on the wire — nothing to announce.
                } else if purged_transfer || authority_of(rec.last_owner, local) == Authority::Local
                {
                    // Either we are the wire-visible owner, or we queued (and
                    // just purged) a transfer the wire never saw — in both
                    // cases receivers still consider us the owner.
                    events.push(event(NetEvent::Despawn { id }));
                } else {
                    log::warn!(
                        "mapped remote proxy {id:?} died without a wire Despawn — divergence; Phase 3 resync heals"
                    );
                }
            }
            self.map.remove_by_entity(entity);
        }

        // Snapshot the query results so the borrow on `world` ends before we
        // mutate the map / push events.
        struct Row {
            entity: Entity,
            owner: PeerId,
            pos: Position,
            vel: Velocity,
            pos_changed: bool,
            vel_changed: bool,
        }
        // 0.19: SystemState::get returns Result<Query, SystemParamValidationError>;
        // a plain read-only Query cannot realistically fail validation, but per
        // the no-unwrap rule we degrade to an empty tick.
        let rows: Vec<Row> = match self.change_query.get(world) {
            Ok(query) => query
                .iter()
                .map(|(entity, owner, pos, vel)| Row {
                    entity,
                    owner: owner.0,
                    pos: *pos,
                    vel: *vel,
                    pos_changed: pos.is_changed(),
                    vel_changed: vel.is_changed(),
                })
                .collect(),
            Err(err) => {
                log::error!("change query validation failed (empty tick): {err}");
                Vec::new()
            }
        };

        let mut entries: Vec<StateEntry> = Vec::new();
        for row in rows {
            // THE authority gate — before any change-mask consultation.
            if authority_of(row.owner, local) != Authority::Local {
                continue;
            }

            let id = match self.map.by_entity.get_mut(&row.entity) {
                Some(rec) => {
                    rec.last_owner = row.owner;
                    rec.id
                }
                None => {
                    // Mint: only ever reachable for entities WE spawned —
                    // adopted (transferred-in) entities were mapped when their
                    // Spawn event arrived, so they can never re-mint (which
                    // would alias our id namespace).
                    let id = NetEntityId {
                        spawner: local,
                        index: row.entity.index_u32(),
                        generation: row.entity.generation().to_bits(),
                    };
                    self.map.insert(id, row.entity, local);
                    events.push(event(NetEvent::Spawn {
                        id,
                        pos: qpos(&row.pos),
                        vel: qvel(&row.vel),
                    }));
                    id
                }
            };

            let send_pos = row.pos_changed || keyframe;
            let send_vel = row.vel_changed || keyframe;
            if send_pos || send_vel {
                entries.push(StateEntry {
                    id,
                    pos: send_pos.then(|| qpos(&row.pos)),
                    vel: send_vel.then(|| qvel(&row.vel)),
                });
            }
        }

        let state = if entries.is_empty() {
            None
        } else {
            let msg = StateMsg {
                version: WIRE_VERSION,
                seq: self.next_seq,
                entries,
            };
            match encode_state(&msg) {
                Ok(bytes) => {
                    // Instrument, don't assume: one lost SCTP fragment kills the
                    // whole unreliable message, so an oversized StateMsg (esp. a
                    // keyframe, which carries EVERY owned entity) multiplies the
                    // effective loss rate. Splitting is Phase 3 (interest
                    // management); until then, make the cliff visible.
                    if bytes.len() > SAFE_DATAGRAM_BYTES {
                        log::warn!(
                            "StateMsg is {}B (> {SAFE_DATAGRAM_BYTES}B safe datagram) — \
                             fragmentation loss amplification; split in Phase 3",
                            bytes.len()
                        );
                    }
                    self.next_seq += 1;
                    Some(bytes.into_boxed_slice())
                }
                Err(err) => {
                    log::error!("state encode failed (dropping tick): {err}");
                    None
                }
            }
        };

        let events = events
            .iter()
            .filter_map(|msg| match encode_event(msg) {
                Ok(bytes) => Some(bytes.into_boxed_slice()),
                Err(err) => {
                    log::error!("event encode failed (dropping event): {err}");
                    None
                }
            })
            .collect();

        Outbox { state, events }
    }

    /// RECEIVER, state channel: whole-message newest-seq LWW gate, then
    /// per-entry identity/ownership/authority gates, then snap-apply.
    pub fn apply_state(&mut self, world: &mut World, from: PeerId, bytes: &[u8]) {
        let msg = match decode_state(bytes) {
            Ok(msg) => msg,
            Err(err) => {
                log::warn!("dropping undecodable state msg from {from:?}: {err}");
                return;
            }
        };

        // Newest-seq-wins. Updated even if every entry below drops: last_seq
        // means "newest snapshot SEEN", and a reordered older message must
        // never resurrect regardless of entry applicability.
        let last = self.last_seq.entry(from).or_insert(0);
        if msg.seq <= *last {
            return;
        }
        *last = msg.seq;

        let local = world.resource::<LocalPeer>().0;
        for entry in msg.entries {
            // Unknown id: state-before-spawn, post-despawn straggler, or
            // stale generation — all inert by full-id keying.
            let Some(&proxy) = self.map.by_id.get(&entry.id) else {
                continue;
            };
            let Some(owner) = world.get::<Owner>(proxy) else {
                continue;
            };
            // Ownership validity: only the CURRENT owner may assert state.
            if owner.0 != from {
                continue;
            }
            // Never apply over our own authority (defense-in-depth: implied
            // by the check above whenever from != local, but this is the
            // invariant-bearing call into THE single decision point).
            if authority_of(owner.0, local) != Authority::Remote {
                continue;
            }
            if let Some(q) = entry.pos
                && let Some(mut pos) = world.get_mut::<Position>(proxy)
            {
                pos.x = dequantize(q.x);
                pos.y = dequantize(q.y);
            }
            if let Some(q) = entry.vel
                && let Some(mut vel) = world.get_mut::<Velocity>(proxy)
            {
                vel.x = dequantize(q.x);
                vel.y = dequantize(q.y);
            }
        }
    }

    /// RECEIVER, events channel (reliable + ordered per sender — processed in
    /// arrival order).
    pub fn apply_events(&mut self, world: &mut World, from: PeerId, bytes: &[u8]) {
        let msg = match decode_event(bytes) {
            Ok(msg) => msg,
            Err(err) => {
                log::warn!("dropping undecodable event from {from:?}: {err}");
                return;
            }
        };
        if msg.sig.is_some() {
            // Reserved for Phase 6; nothing signs in the slice.
            log::warn!("ignoring unexpected signature on event from {from:?}");
        }

        match msg.event {
            NetEvent::Spawn { id, pos, vel } => {
                // Only the minting peer may introduce ids in its namespace.
                if id.spawner != from {
                    log::warn!("spawn for {id:?} from non-spawner {from:?} — dropped");
                    return;
                }
                // Idempotent: late-join replay may overlap the original
                // broadcast; a duplicate must never create a second proxy.
                if self.map.by_id.contains_key(&id) {
                    return;
                }
                let proxy = spawn_owned(
                    world,
                    id.spawner,
                    Position {
                        x: dequantize(pos.x),
                        y: dequantize(pos.y),
                    },
                    Velocity {
                        x: dequantize(vel.x),
                        y: dequantize(vel.y),
                    },
                );
                self.map.insert(id, proxy, id.spawner);
            }
            NetEvent::Despawn { id } => {
                let Some(&proxy) = self.map.by_id.get(&id) else {
                    // Known accepted gap: cross-sender reordering after a
                    // handoff can deliver a Despawn before its Spawn (R6).
                    log::warn!(
                        "despawn for unknown {id:?} from {from:?} — dropped (Phase 3 resync heals)"
                    );
                    return;
                };
                let Some(owner) = world.get::<Owner>(proxy) else {
                    self.map.remove_by_id(&id);
                    return;
                };
                if owner.0 != from {
                    log::warn!("despawn for {id:?} from non-owner {from:?} — dropped");
                    return;
                }
                world.despawn(proxy);
                self.map.remove_by_id(&id);
            }
            NetEvent::OwnershipTransfer { id, new_owner } => {
                let Some(&proxy) = self.map.by_id.get(&id) else {
                    log::warn!("transfer for unknown {id:?} from {from:?} — dropped");
                    return;
                };
                let Some(owner) = world.get::<Owner>(proxy) else {
                    return;
                };
                // Only the current authority may give an entity away.
                if owner.0 != from {
                    log::warn!("transfer for {id:?} from non-owner {from:?} — dropped");
                    return;
                }
                if let Some(mut owner) = world.get_mut::<Owner>(proxy) {
                    owner.0 = new_owner;
                }
                if let Some(rec) = self.map.by_entity.get_mut(&proxy) {
                    rec.last_owner = new_owner;
                }
                // If new_owner == LocalPeer, authority_of flips to Local on
                // the next simulate/collect — the receiver switches from
                // apply to compute with no extra code path.
            }
        }
    }

    /// Initiate an A→B ownership handoff for an entity WE currently own.
    /// Queues the reliable `OwnershipTransfer` and flips the local `Owner` in
    /// the same tick — from this instant we stop computing and stop
    /// collecting it, so no double-authority window can exist (the ≤½-RTT
    /// nobody-simulates freeze is the safe direction).
    pub fn transfer_ownership(
        &mut self,
        world: &mut World,
        entity: Entity,
        to: PeerId,
    ) -> Result<(), NotAuthoritative> {
        let local = world.resource::<LocalPeer>().0;
        let Some(owner) = world.get::<Owner>(entity) else {
            return Err(NotAuthoritative);
        };
        if authority_of(owner.0, local) != Authority::Local {
            return Err(NotAuthoritative);
        }

        // An entity transferred before it was ever collected has no id yet:
        // mint + announce it first so receivers can resolve the transfer.
        let id = match self.map.by_entity.get(&entity) {
            Some(rec) => rec.id,
            None => {
                let id = NetEntityId {
                    spawner: local,
                    index: entity.index_u32(),
                    generation: entity.generation().to_bits(),
                };
                self.map.insert(id, entity, local);
                let (pos, vel) = (
                    world
                        .get::<Position>(entity)
                        .copied()
                        .unwrap_or(Position { x: 0.0, y: 0.0 }),
                    world
                        .get::<Velocity>(entity)
                        .copied()
                        .unwrap_or(Velocity { x: 0.0, y: 0.0 }),
                );
                self.pending_events.push(event(NetEvent::Spawn {
                    id,
                    pos: qpos(&pos),
                    vel: qvel(&vel),
                }));
                id
            }
        };

        self.pending_events
            .push(event(NetEvent::OwnershipTransfer { id, new_owner: to }));
        if let Some(mut owner) = world.get_mut::<Owner>(entity) {
            owner.0 = to;
        }
        if let Some(rec) = self.map.by_entity.get_mut(&entity) {
            rec.last_owner = to;
        }
        Ok(())
    }

    /// Late-join replay: re-announce entities WE minted and still own, so a
    /// newly-connected peer can build proxies. Send the returned events to
    /// that peer only. (Entities we adopted via transfer cannot be replayed —
    /// their spawner's namespace guard would reject us; documented gap, owned
    /// by Phase 3/5 session sync.)
    pub fn on_peer_connected(&mut self, world: &mut World, _peer: PeerId) -> Vec<Box<[u8]>> {
        let local = world.resource::<LocalPeer>().0;
        let mut out = Vec::new();
        for (&entity, rec) in &self.map.by_entity {
            if rec.id.spawner != local {
                continue;
            }
            let Some(owner) = world.get::<Owner>(entity) else {
                continue;
            };
            if authority_of(owner.0, local) != Authority::Local {
                log::warn!(
                    "late-join replay skips {:?} (transferred away) — new peer relies on Phase 3 resync",
                    rec.id
                );
                continue;
            }
            let (Some(pos), Some(vel)) =
                (world.get::<Position>(entity), world.get::<Velocity>(entity))
            else {
                continue;
            };
            let msg = event(NetEvent::Spawn {
                id: rec.id,
                pos: qpos(pos),
                vel: qvel(vel),
            });
            match encode_event(&msg) {
                Ok(bytes) => out.push(bytes.into_boxed_slice()),
                Err(err) => log::error!("late-join spawn encode failed: {err}"),
            }
        }
        out
    }
}

fn event(event: NetEvent) -> EventMsg {
    EventMsg {
        version: WIRE_VERSION,
        sig: None, // reserved; nothing signs in the slice (Phase 6)
        event,
    }
}

fn qpos(pos: &Position) -> QVec2 {
    quantize_vec2(pos.x, pos.y)
}

fn qvel(vel: &Velocity) -> QVec2 {
    quantize_vec2(vel.x, vel.y)
}
