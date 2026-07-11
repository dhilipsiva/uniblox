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
//!   in this crate, and the authority gate runs BEFORE any change/baseline
//!   consultation, so remote-applied writes can never echo back.
//! - **Delta vs last-acked baseline, ABSOLUTE values (ADR-0020).** A component
//!   is included while its QUANTIZED value differs from the per-entity baseline
//!   OR that baseline value is not yet confirmed (acked) by every tracked peer;
//!   the value is always full quantized state, never an arithmetic delta (the
//!   channel is lossy; arithmetic deltas compound loss into permanent drift).
//!   The fixed keyframe is GONE — an unconfirmed value is re-sent in EVERY
//!   message of its contiguous run until acked, so a lost final value recovers
//!   continuously (a new peer, acked nothing, gets a full targeted re-send).
//!   Receivers ack their newest applied seq per sender (`drain_acks` →
//!   `NetEvent::Ack`); the sender advances a per-peer baseline. Deeper desync
//!   is the separate anti-entropy-resync item.
//! - **LWW = newest-seq wins**, not latest-arrival: the state channel is
//!   unordered, so a whole message is dropped iff its seq ≤ the last seen
//!   from that sender.
//! - **Identity ≠ authority.** `NetEntityId` is spawner-stable (minted once);
//!   current authority lives only in the proxy's `Owner` component, mutated
//!   only by reliable `OwnershipTransfer` events. State from a sender that is
//!   not the CURRENT owner is dropped — the only sound arbiter for handoff
//!   races (per-sender seq streams are incomparable).
//! - **State trigger = quantized-value diff, not change detection (ADR-0020).**
//!   The sender queries all owned entities (one cached `SystemState`) and
//!   compares each component's current quantized value against the last value
//!   it committed to the baseline — so a same-value write is not re-sent, and
//!   there is no dependency on Bevy change ticks (the earlier `Ref::is_changed`
//!   / `check_change_ticks` hazard on a long-lived server is retired).
//! - **Known accepted gaps** (documented, warn-logged, healed by Phase 3
//!   anti-entropy resync — do NOT "fix" ad hoc):
//!   - cross-SENDER event reordering after a handoff: a third peer may see
//!     the new owner's `Despawn` before the original `Spawn` (orphaned
//!     proxy), and a CHAINED handoff A→B→C may deliver T2(B→C) before
//!     T1(A→B) at a fourth peer — T2 is dropped, the proxy records owner B
//!     forever, and C's state is rejected until resync (frozen, wrong-owner
//!     proxy; no packet loss required, just cross-sender skew);
//!   - late-join replay of entities whose spawner no longer owns them.

use std::collections::{HashMap, HashSet};

use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemState;
use engine_core::{Authority, LocalPeer, Owner, Position, Velocity, authority_of, spawn_owned};
use protocol::{
    EventMsg, NetEntityId, NetEvent, PeerId, QVec2, StateEntry, StateMsg, WIRE_VERSION,
    decode_event, decode_state, dequantize, encode_event, encode_state, quantize_vec2,
};

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

/// Delta-baseline send record for ONE component of an owned entity (ADR-0020):
/// the value we are trying to get every peer to confirm, the state-msg `seq`
/// at which this value's current contiguous send-run began, and the last seq
/// the component was included in (gap detection).
#[derive(Clone, Copy)]
struct CompSend {
    value: QVec2,
    run_start: u64,
    last_sent: u64,
}

/// Per owned-entity send record: one `CompSend` per replicated component.
#[derive(Clone, Copy, Default)]
struct EntitySend {
    pos: Option<CompSend>,
    vel: Option<CompSend>,
}

/// Decide whether to include one component this tick and the `CompSend` to
/// commit IF included (ADR-0020, contiguous-run cumulative-ack). Pure — the
/// caller commits only when the overall message is actually sent.
///
/// - never sent / value changed ⇒ always include, starting a fresh run at `seq`.
/// - unchanged ⇒ include only while UNCONFIRMED (some tracked peer's acked seq
///   is < the run start). A gap since the last inclusion restarts the run so
///   "acked ≥ run_start ⇒ the peer received a message carrying this value"
///   stays sound. Empty peer set ⇒ treated as confirmed (nothing to re-send
///   to) — degrades to plain changed-only sending.
fn decide_component(
    current: Option<CompSend>,
    value: QVec2,
    seq: u64,
    acked_seq: &HashMap<PeerId, u64>,
    peers: &HashSet<PeerId>,
) -> (bool, CompSend) {
    match current {
        None => (
            true,
            CompSend {
                value,
                run_start: seq,
                last_sent: seq,
            },
        ),
        Some(c) if c.value != value => (
            true,
            CompSend {
                value,
                run_start: seq,
                last_sent: seq,
            },
        ),
        Some(c) => {
            let confirmed = peers.is_empty()
                || peers
                    .iter()
                    .all(|p| acked_seq.get(p).copied().unwrap_or(0) >= c.run_start);
            if confirmed {
                (false, c)
            } else {
                // Re-send; a gap since the last inclusion starts a new run.
                let run_start = if c.last_sent + 1 == seq {
                    c.run_start
                } else {
                    seq
                };
                (
                    true,
                    CompSend {
                        value,
                        run_start,
                        last_sent: seq,
                    },
                )
            }
        }
    }
}

/// The replication endpoint for one World: sender + receiver + identity map.
pub struct Replication {
    change_query: ChangeQuery,
    map: NetIdMap,
    next_seq: u64,
    last_seq: HashMap<PeerId, u64>,
    pending_events: Vec<EventMsg>,
    /// Delta baseline (ADR-0020). Sender side: per owned entity, the confirm
    /// state of each component; per peer, the highest seq of OUR stream they
    /// have acked. Receiver side: per sender, the highest seq we have acked
    /// back. `peers` is the broadcast set (tracked on connect / departure).
    send_state: HashMap<Entity, EntitySend>,
    acked_seq: HashMap<PeerId, u64>,
    /// Per sender, the highest seq whose entries we FULLY applied (every entry
    /// resolved to a proxy and passed the owner/authority gates). This — NOT
    /// `last_seq` (which is the LWW "seen" high-water and advances even when
    /// entries are dropped) — is what we ack, so we never confirm a value we
    /// did not actually hold (auditor F1: a state msg racing its Spawn, or a
    /// handoff owner-mismatch, must NOT falsely confirm and silence the sender).
    applied_seq: HashMap<PeerId, u64>,
    ack_sent: HashMap<PeerId, u64>,
    peers: HashSet<PeerId>,
}

impl Replication {
    pub fn new(world: &mut World) -> Self {
        Replication {
            change_query: SystemState::new(world),
            map: NetIdMap::default(),
            next_seq: 1,
            last_seq: HashMap::new(),
            pending_events: Vec::new(),
            send_state: HashMap::new(),
            acked_seq: HashMap::new(),
            applied_seq: HashMap::new(),
            ack_sent: HashMap::new(),
            peers: HashSet::new(),
        }
    }

    /// One network tick of the SENDER: lifecycle diff (spawn/despawn events),
    /// then authority-gated state entries — a component is included while
    /// changed OR not yet confirmed (acked) by all peers (ADR-0020 delta
    /// baseline; the fixed keyframe is gone — its "recover a lost final value"
    /// job is now continuous, driven by acks).
    ///
    /// PRECONDITION (auditor F4): every peer this endpoint broadcasts to MUST be
    /// registered via [`track_peer`]/[`on_peer_connected`]. With an EMPTY peer
    /// set, `decide_component` treats unchanged values as already-confirmed and
    /// sends changed-only with no re-send safety net — correct for a solo
    /// endpoint, but a silent lost-final-value bug for a real multi-peer pump
    /// that forgets to track. The keyframe that used to paper over that is gone.
    pub fn collect(&mut self, world: &mut World) -> Outbox {
        let local = world.resource::<LocalPeer>().0;

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
                        // Acks are directed (drain_acks), never queued in
                        // pending_events — keep any (there should be none).
                        NetEvent::Ack { .. } => return true,
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
            self.send_state.remove(&entity); // no baseline for a gone entity
        }

        // Snapshot the query results so the borrow on `world` ends before we
        // mutate the map / push events.
        struct Row {
            entity: Entity,
            owner: PeerId,
            pos: Position,
            vel: Velocity,
        }
        // 0.19: SystemState::get returns Result<Query, SystemParamValidationError>;
        // a plain read-only Query cannot realistically fail validation, but per
        // the no-unwrap rule we degrade to an empty tick. (ADR-0020: the send
        // trigger is now a QUANTIZED-VALUE diff against the per-peer baseline,
        // not `Ref::is_changed()` — so a write that leaves the quantized value
        // unchanged is not re-sent, and the authority gate below still prevents
        // any echo of remote-applied state.)
        let rows: Vec<Row> = match self.change_query.get(world) {
            Ok(query) => query
                .iter()
                .map(|(entity, owner, pos, vel)| Row {
                    entity,
                    owner: owner.0,
                    pos: *pos,
                    vel: *vel,
                })
                .collect(),
            Err(err) => {
                log::error!("change query validation failed (empty tick): {err}");
                Vec::new()
            }
        };

        // Decide each owned entity's entry at the PROVISIONAL seq. `commits`
        // stages the per-component `CompSend` updates — applied only if the
        // message is actually sent, so an empty tick consumes no seq and
        // mutates no baseline (the seq-consumption invariant).
        let seq = self.next_seq;
        let mut entries: Vec<StateEntry> = Vec::new();
        let mut commits: Vec<(Entity, Option<CompSend>, Option<CompSend>)> = Vec::new();
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

            let prior = self
                .send_state
                .get(&row.entity)
                .copied()
                .unwrap_or_default();
            let (send_pos, next_pos) =
                decide_component(prior.pos, qpos(&row.pos), seq, &self.acked_seq, &self.peers);
            let (send_vel, next_vel) =
                decide_component(prior.vel, qvel(&row.vel), seq, &self.acked_seq, &self.peers);
            if send_pos || send_vel {
                entries.push(StateEntry {
                    id,
                    pos: send_pos.then(|| qpos(&row.pos)),
                    vel: send_vel.then(|| qvel(&row.vel)),
                });
                commits.push((
                    row.entity,
                    send_pos.then_some(next_pos),
                    send_vel.then_some(next_vel),
                ));
            }
        }

        let state = if entries.is_empty() {
            None
        } else {
            let msg = StateMsg {
                version: WIRE_VERSION,
                seq,
                entries,
            };
            match encode_state(&msg) {
                Ok(bytes) => {
                    // Instrument, don't assume: one lost SCTP fragment kills the
                    // whole unreliable message, so an oversized StateMsg
                    // multiplies the effective loss rate. Splitting is a later
                    // Phase-3 item (interest management); until then, warn.
                    if bytes.len() > SAFE_DATAGRAM_BYTES {
                        log::warn!(
                            "StateMsg is {}B (> {SAFE_DATAGRAM_BYTES}B safe datagram) — \
                             fragmentation loss amplification; split in Phase 3",
                            bytes.len()
                        );
                    }
                    // Commit: the message is sent, so this seq is consumed and
                    // every included component's baseline advances.
                    for (entity, pos, vel) in commits {
                        let slot = self.send_state.entry(entity).or_default();
                        if let Some(c) = pos {
                            slot.pos = Some(c);
                        }
                        if let Some(c) = vel {
                            slot.vel = Some(c);
                        }
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

        // Newest-seq-wins. `last_seq` (the LWW "seen" high-water) is updated
        // even if every entry below drops — a reordered older message must
        // never resurrect regardless of entry applicability. This is SEPARATE
        // from the ack basis (`applied_seq`), tracked below.
        let last = self.last_seq.entry(from).or_insert(0);
        if msg.seq <= *last {
            return;
        }
        *last = msg.seq;

        let local = world.resource::<LocalPeer>().0;
        // Ack soundness (auditor F1): we advance the APPLIED high-water — the
        // seq we ack — only if EVERY entry in this message actually applied.
        // A dropped entry (proxy not yet spawned, a handoff owner-mismatch, or
        // our own authority) means we do NOT hold that value from this sender;
        // acking this seq would let the sender mark it confirmed and STOP
        // re-sending, and with the keyframe gone that divergence is permanent.
        // Withholding the ack keeps the sender re-sending (the value is in every
        // message of its run) until a later message fully applies — bounded,
        // for the state-before-spawn race, by the reliable Spawn's arrival.
        let mut fully_applied = true;
        for entry in msg.entries {
            // Unknown id: state-before-spawn, post-despawn straggler, or
            // stale generation — all inert by full-id keying.
            let Some(&proxy) = self.map.by_id.get(&entry.id) else {
                fully_applied = false;
                continue;
            };
            let Some(owner) = world.get::<Owner>(proxy) else {
                fully_applied = false;
                continue;
            };
            // Ownership validity: only the CURRENT owner may assert state.
            if owner.0 != from {
                fully_applied = false;
                continue;
            }
            // Never apply over our own authority (defense-in-depth: implied
            // by the check above whenever from != local, but this is the
            // invariant-bearing call into THE single decision point).
            if authority_of(owner.0, local) != Authority::Remote {
                fully_applied = false;
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
        // Ack ONLY a message we fully held. Monotonic across increasing seqs
        // (LWW already dropped older ones); a partial message leaves the
        // high-water where it was, so the sender keeps re-sending.
        //
        // Granularity note (auditor NIT): the ack is whole-message, per-sender.
        // A single persistently-unresolvable entry (e.g. the documented frozen
        // wrong-owner proxy from cross-sender handoff reordering) therefore
        // withholds acks for that sender's ENTIRE stream, so it re-sends all its
        // entities every tick until the entry resolves. Bandwidth-only (every
        // valid entry still applied; the receiver holds correct values), bounded
        // by the documented resync gaps, and still better than the old keyframe.
        // Per-entry acks are a future optimization, not a correctness need.
        if fully_applied {
            let applied = self.applied_seq.entry(from).or_insert(0);
            *applied = (*applied).max(msg.seq);
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
            NetEvent::Ack { seq } => {
                // Delta baseline (ADR-0020): `from` confirms it has applied up
                // to `seq` of OUR stream. Monotonic (reliable+ordered), but
                // clamp defensively so a stray/reordered ack can never lower a
                // baseline the peer already holds.
                let slot = self.acked_seq.entry(from).or_insert(0);
                *slot = (*slot).max(seq);
            }
        }
    }

    /// RECEIVER → SENDER acks (ADR-0020): for every sender whose newest FULLY
    /// APPLIED seq (`applied_seq`, NOT the "seen" `last_seq`) has advanced since
    /// we last acked, produce a DIRECTED ack event to send back to that sender
    /// (the caller does `send_event(target, bytes)`). Acking only fully-applied
    /// seqs is the F1 soundness guarantee — we never confirm a value we dropped.
    /// Drives the sender's delta baseline; call once per pump after applying
    /// state.
    pub fn drain_acks(&mut self) -> Vec<(PeerId, Box<[u8]>)> {
        let mut acks = Vec::new();
        for (&from, &seq) in &self.applied_seq {
            if seq > self.ack_sent.get(&from).copied().unwrap_or(0) {
                let msg = event(NetEvent::Ack { seq });
                match encode_event(&msg) {
                    Ok(bytes) => {
                        acks.push((from, bytes.into_boxed_slice()));
                        self.ack_sent.insert(from, seq);
                    }
                    Err(err) => log::error!("ack encode failed for {from:?}: {err}"),
                }
            }
        }
        acks
    }

    /// Add a peer to the broadcast/confirmation set (ADR-0020). The delta
    /// baseline re-sends unconfirmed values ONLY to tracked peers, so the pump
    /// MUST track every connected peer (else a lost value is never recovered —
    /// the keyframe that used to recover it is gone). `on_peer_connected` calls
    /// this; unit tests call it to declare who they broadcast to.
    pub fn track_peer(&mut self, peer: PeerId) {
        self.peers.insert(peer);
    }

    /// Drop a departed peer from the confirmation set + its baseline/ack state.
    pub fn untrack_peer(&mut self, peer: PeerId) {
        self.peers.remove(&peer);
        self.acked_seq.remove(&peer);
        self.ack_sent.remove(&peer);
        self.applied_seq.remove(&peer);
        self.last_seq.remove(&peer);
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
        // We no longer author this entity's state — drop its delta baseline
        // (ADR-0020); if it ever comes back to us it re-baselines from scratch.
        if authority_of(to, local) != Authority::Local {
            self.send_state.remove(&entity);
        }
        Ok(())
    }

    /// Late-join replay: re-announce entities WE minted and still own, so a
    /// newly-connected peer can build proxies. Send the returned events to
    /// that peer only. (Entities we adopted via transfer cannot be replayed —
    /// their spawner's namespace guard would reject us; documented gap, owned
    /// by Phase 3/5 session sync.)
    pub fn on_peer_connected(&mut self, world: &mut World, peer: PeerId) -> Vec<Box<[u8]>> {
        // Track for delta baselining (ADR-0020) — a new peer has confirmed
        // nothing, so everything it should have is re-sent until it acks.
        self.track_peer(peer);
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
