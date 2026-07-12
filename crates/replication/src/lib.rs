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
//! - **Interest management: the sender is PER-PEER (ADR-0021).** `collect_all`
//!   returns one AOI-gated `Outbox` per tracked peer — each with its OWN delta
//!   baseline (`send_state[peer][entity]`), seq stream, and `known` set. A peer
//!   sees only entities within its `Aoi` (`set_aoi`; unset ⇒ unbounded); an
//!   out-of-AOI entity is withheld in BOTH state AND existence (spawn-on-enter /
//!   despawn-on-exit) — the Mode-3 read-cheat defense. Per-peer order is
//!   load-bearing: **dead → transfer → exit → enter → state** (dead wins over a
//!   pending transfer; exit drops the baseline so a re-enter re-baselines at a
//!   fresh seq; enter Spawns only `spawner==local`; the id-map prunes only after
//!   every peer is told). Emissions are sorted by `NetEntityId` (peers by
//!   `PeerId`) so the wire output is DETERMINISTIC. The RECEIVER is unchanged —
//!   each receiver sees one continuous per-sender stream regardless of AOI.
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
//!   - late-join replay of entities whose spawner no longer owns them;
//!   - (ADR-0021) a chained handoff to a NEVER-witnessed new owner of an
//!     ADOPTED entity (we cannot mint in another peer's namespace) is orphaned;
//!     a dropped Outbox desyncs `known[peer]` from the receiver; an entity
//!     oscillating across the AOI edge churns Spawn/Despawn (hysteresis later).

use std::collections::{HashMap, HashSet};

use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemState;
use engine_core::{
    Authority, Controlled, ControlledBy, Input, InputHistory, Intent, InterpBuffer, LocalPeer,
    Owner, PendingInputs, Position, ProcessedInput, Snapshot, Tick, Velocity, authority_of,
    push_pending_input, push_snapshot, spawn_owned,
};
use protocol::{
    EventMsg, NetEntityId, NetEvent, PeerId, QVec2, StateEntry, StateMsg, WIRE_VERSION,
    decode_event, decode_state, dequantize, encode_event, encode_state, quantize_vec2,
};

mod interest;
use interest::{Aoi, DEFAULT_CELL, SpatialGrid};

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
/// commit IF included (ADR-0020 contiguous-run cumulative-ack, now PER-PEER for
/// ADR-0021 AOI). Pure — the caller commits only when the message is sent.
/// `acked` is THIS peer's confirmed seq of our stream to it.
///
/// - never sent / value changed ⇒ always include, starting a fresh run at `seq`.
/// - unchanged ⇒ include only while UNCONFIRMED (`acked < run_start`). A gap
///   since the last inclusion restarts the run so "acked ≥ run_start ⇒ the peer
///   received a message carrying this value" stays sound. Under per-peer AOI
///   streams an in-view unconfirmed component is in EVERY message ⇒ no gaps, so
///   the gap-reset is dormant insurance (its coverage moved to the exit/re-enter
///   re-baseline test); it re-earns its keep if a scheduler ever skips a tick.
fn decide_component(
    current: Option<CompSend>,
    value: QVec2,
    seq: u64,
    acked: u64,
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
            if acked >= c.run_start {
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
    /// SENDER, per-peer (ADR-0021 AOI): each peer gets a DIFFERENT per-tick
    /// message (its AOI subset), so each has its own monotonic seq stream and
    /// its own delta baseline. Absent for a peer ⇒ defaults (seq 1, empty).
    next_seq: HashMap<PeerId, u64>,
    /// RECEIVER, per sender: newest snapshot seq SEEN (LWW gate). Unrelated to
    /// the sender-side `next_seq` above despite the peer-keyed shape.
    last_seq: HashMap<PeerId, u64>,
    /// SENDER delta baseline (ADR-0020/0021), now PER-PEER: per peer, per owned
    /// entity, the confirm state of each component. A peer sees only its AOI, so
    /// its baseline is independent of every other peer's.
    send_state: HashMap<PeerId, HashMap<Entity, EntitySend>>,
    /// SENDER, per peer: the entities that peer currently has spawned (its proxy
    /// set) — drives spawn-on-AOI-enter / despawn-on-AOI-exit (existence gating).
    known: HashMap<PeerId, HashSet<Entity>>,
    /// SENDER, per peer: its area of interest. Absent ⇒ unbounded (sees all
    /// owned entities) — the backward-compatible default.
    aoi: HashMap<PeerId, Aoi>,
    /// SENDER: one-shot ownership-transfer intents (entity → new owner), drained
    /// by the next `collect_all` (announced to peers that know the entity + the
    /// new owner regardless of AOI, then cleared).
    pending_transfers: HashMap<Entity, PeerId>,
    /// SENDER, per peer: the highest seq of OUR stream that peer has acked.
    acked_seq: HashMap<PeerId, u64>,
    /// RECEIVER, per sender: the highest seq whose entries we FULLY applied
    /// (every entry resolved to a proxy and passed the owner/authority gates).
    /// This — NOT `last_seq` (which advances even when entries drop) — is what
    /// we ack, so we never confirm a value we did not hold (auditor F1).
    applied_seq: HashMap<PeerId, u64>,
    /// RECEIVER, per sender: the highest seq we have acked back.
    ack_sent: HashMap<PeerId, u64>,
    /// CLIENT, per controlled entity: the highest input seq we have SENT to its
    /// authority (ADR-0022 Stage B), so `drain_inputs` sends each input once.
    input_sent: HashMap<Entity, u64>,
    /// The tracked peer set (added on connect, dropped on departure).
    peers: HashSet<PeerId>,
}

impl Replication {
    pub fn new(world: &mut World) -> Self {
        Replication {
            change_query: SystemState::new(world),
            map: NetIdMap::default(),
            next_seq: HashMap::new(),
            last_seq: HashMap::new(),
            send_state: HashMap::new(),
            known: HashMap::new(),
            aoi: HashMap::new(),
            pending_transfers: HashMap::new(),
            acked_seq: HashMap::new(),
            applied_seq: HashMap::new(),
            ack_sent: HashMap::new(),
            input_sent: HashMap::new(),
            peers: HashSet::new(),
        }
    }

    /// One network tick of the SENDER, PER PEER (ADR-0021 interest management).
    /// Returns a per-peer outbox for every tracked peer that has something to
    /// send. Each peer sees only entities in its AOI (set via [`set_aoi`];
    /// unset ⇒ unbounded/sees-all), and gets its OWN delta baseline + seq
    /// stream. Out-of-AOI entities are withheld in BOTH state AND existence
    /// (spawn-on-enter / despawn-on-exit) — the structural read-cheat defense.
    ///
    /// PRECONDITION: only TRACKED peers ([`track_peer`]/[`on_peer_connected`])
    /// get outboxes; the pump must track every connected peer.
    ///
    /// One snapshot per call. Peer processing order is load-bearing:
    /// **dead → transfer → exit → enter → state** (dead wins over a transfer so
    /// a corpse is never handed off; the id-map is pruned only AFTER every peer
    /// has been told, so a two-peer despawn reaches both).
    pub fn collect_all(&mut self, world: &mut World) -> Vec<(PeerId, Outbox)> {
        let local = world.resource::<LocalPeer>().0;
        // The authoritative sim tick stamped into every snapshot (ADR-0022), the
        // interpolation time axis. Absent (a world without the resource) ⇒ 0.
        let tick = world.get_resource::<Tick>().map_or(0, |t| t.0);
        // Per-peer newest PROCESSED input seq — the reconciliation marker
        // (ADR-0022 Stage B). Cloned so the per-peer loop can read it while the
        // world is borrowed for other reads. Empty ⇒ 0 for every peer.
        let processed_input = world
            .get_resource::<ProcessedInput>()
            .map(|p| p.0.clone())
            .unwrap_or_default();

        // 1. Snapshot owned+alive rows; mint ids (map only — existence is
        //    announced PER PEER on AOI-enter, never globally); build the grid +
        //    an owned-entity lookup. The authority gate runs HERE (before any
        //    baseline/AOI consultation), so remote-applied writes never echo.
        struct Row {
            entity: Entity,
            owner: PeerId,
            pos: Position,
            vel: Velocity,
        }
        let rows: Vec<Row> = match self.change_query.get(world) {
            Ok(query) => query
                .iter()
                .filter(|(_, owner, _, _)| authority_of(owner.0, local) == Authority::Local)
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
        let mut grid = SpatialGrid::new(DEFAULT_CELL);
        let mut owned: HashMap<Entity, (NetEntityId, Position, Velocity)> = HashMap::new();
        for row in &rows {
            let id = match self.map.by_entity.get_mut(&row.entity) {
                Some(rec) => {
                    rec.last_owner = row.owner;
                    rec.id
                }
                None => {
                    let id = NetEntityId {
                        spawner: local,
                        index: row.entity.index_u32(),
                        generation: row.entity.generation().to_bits(),
                    };
                    self.map.insert(id, row.entity, local);
                    id
                }
            };
            grid.insert(row.entity, (row.pos.x, row.pos.y));
            owned.insert(row.entity, (id, row.pos, row.vel));
        }

        // 2. Dead = mapped entities no longer alive. Dead WINS over a pending
        //    transfer (a corpse is never handed off — the owned-ghost guard).
        let dead: HashSet<Entity> = self
            .map
            .by_entity
            .keys()
            .copied()
            .filter(|e| !world.entities().contains(*e))
            .collect();
        for e in &dead {
            self.pending_transfers.remove(e);
        }

        // Deterministic emission order (by NetEntityId): the per-peer wire
        // output must not depend on HashSet/HashMap iteration seed — reproducible
        // captures and stable tests. Dead + transfers are shared across peers, so
        // sort them once here; per-peer exits/enters/state are sorted below.
        let mut dead_sorted: Vec<Entity> = dead.iter().copied().collect();
        dead_sorted.sort_by_key(|e| self.map.by_entity.get(e).map(|r| r.id));
        let mut transfers_sorted: Vec<(Entity, PeerId)> = self
            .pending_transfers
            .iter()
            .map(|(&e, &q)| (e, q))
            .collect();
        transfers_sorted.sort_by_key(|(e, _)| self.map.by_entity.get(e).map(|r| r.id));

        // 3. Per tracked peer, in order: dead → transfer → exit → enter → state.
        let mut peers: Vec<PeerId> = self.peers.iter().copied().collect();
        peers.sort();
        let mut result = Vec::new();
        for p in peers {
            let mut known_p = self.known.remove(&p).unwrap_or_default();
            let mut send_p = self.send_state.remove(&p).unwrap_or_default();
            let mut seq = self.next_seq.get(&p).copied().unwrap_or(1);
            let acked = self.acked_seq.get(&p).copied().unwrap_or(0);
            let aoi_p = self.aoi.get(&p).copied();
            let mut events: Vec<EventMsg> = Vec::new();

            // DEAD — despawn to peers that knew the corpse; drop its baseline.
            for &e in &dead_sorted {
                if known_p.remove(&e) {
                    if let Some(rec) = self.map.by_entity.get(&e) {
                        events.push(event(NetEvent::Despawn { id: rec.id }));
                    }
                    send_p.remove(&e);
                }
            }

            // TRANSFER — a peer that already KNOWS the entity gets an
            // OwnershipTransfer (proxy kept under the new owner, NOT despawned).
            // A NEW OWNER that doesn't yet know it is told the Transfer too, plus
            // a Spawn — but the Spawn is emitted ONLY for entities in OUR
            // namespace (spawner == local): we cannot introduce a foreign-
            // namespace (adopted) entity, whose Spawn the receiver rejects. The
            // bare Transfer is ALWAYS sent (auditor): harmless if the new owner
            // has no proxy (dropped as unknown id), but load-bearing if it
            // WITNESSED the entity via the original owner (its existing proxy
            // flips) — so a chained handoff to a witnessing peer completes. Only
            // a never-witnessed new owner of an adopted entity is orphaned until
            // Phase-3 resync.
            for &(e, q) in &transfers_sorted {
                let Some(id) = self.map.by_entity.get(&e).map(|r| r.id) else {
                    continue;
                };
                if known_p.remove(&e) {
                    events.push(event(NetEvent::OwnershipTransfer { id, new_owner: q }));
                    send_p.remove(&e);
                } else if p == q {
                    if id.spawner == local {
                        let pos = world
                            .get::<Position>(e)
                            .copied()
                            .unwrap_or(Position { x: 0.0, y: 0.0 });
                        let vel = world
                            .get::<Velocity>(e)
                            .copied()
                            .unwrap_or(Velocity { x: 0.0, y: 0.0 });
                        events.push(event(NetEvent::Spawn {
                            id,
                            pos: qpos(&pos),
                            vel: qvel(&vel),
                        }));
                    }
                    events.push(event(NetEvent::OwnershipTransfer { id, new_owner: q }));
                }
            }

            // This peer's visible set: its AOI circle, or all owned if unbounded.
            let visible: HashSet<Entity> = match aoi_p {
                Some(a) => grid.in_radius(a.center, a.radius).into_iter().collect(),
                None => owned.keys().copied().collect(),
            };

            // AOI-EXIT — known but no longer visible (still owned): despawn the
            // proxy and drop its baseline (so a re-enter re-baselines from
            // scratch — the run-start soundness across the visibility gap).
            let mut exits: Vec<Entity> = known_p
                .iter()
                .copied()
                .filter(|e| owned.contains_key(e) && !visible.contains(e))
                .collect();
            exits.sort_by_key(|e| owned.get(e).map(|t| t.0));
            for e in exits {
                known_p.remove(&e);
                send_p.remove(&e);
                if let Some((id, ..)) = owned.get(&e) {
                    events.push(event(NetEvent::Despawn { id: *id }));
                }
            }

            // AOI-ENTER — newly visible owned entities. We emit a Spawn only for
            // entities in OUR namespace (spawner == local); an ADOPTED entity
            // (transferred to us, spawner ≠ us) is instead stated to peers that
            // already hold its proxy (from the original owner) — we cannot mint
            // in another peer's namespace (the receiver rejects such a Spawn),
            // so a peer that never saw the original Spawn drops our state until
            // Phase-3 resync (the documented cross-sender late-join gap). Either
            // way it enters `known` so the state pass below sends it (send_p
            // None ⇒ fresh run).
            let mut enters: Vec<Entity> = visible
                .iter()
                .copied()
                .filter(|e| owned.contains_key(e) && !known_p.contains(e))
                .collect();
            enters.sort_by_key(|e| owned.get(e).map(|t| t.0));
            for e in enters {
                if let Some((id, pos, vel)) = owned.get(&e) {
                    if id.spawner == local {
                        events.push(event(NetEvent::Spawn {
                            id: *id,
                            pos: qpos(pos),
                            vel: qvel(vel),
                        }));
                    }
                    known_p.insert(e);
                }
            }

            // STATE DELTA over what the peer now knows AND sees. Decide/commit
            // split: the CompSend updates + seq bump happen only if a message is
            // actually encoded (an events-only tick consumes no seq).
            let mut entries: Vec<StateEntry> = Vec::new();
            let mut commits: Vec<(Entity, Option<CompSend>, Option<CompSend>)> = Vec::new();
            let mut visible_known: Vec<Entity> = known_p
                .iter()
                .copied()
                .filter(|e| visible.contains(e))
                .collect();
            visible_known.sort_by_key(|e| owned.get(e).map(|t| t.0));
            for &e in &visible_known {
                let Some((id, pos, vel)) = owned.get(&e) else {
                    continue;
                };
                let prior = send_p.get(&e).copied().unwrap_or_default();
                let (send_pos, next_pos) = decide_component(prior.pos, qpos(pos), seq, acked);
                let (send_vel, next_vel) = decide_component(prior.vel, qvel(vel), seq, acked);
                if send_pos || send_vel {
                    entries.push(StateEntry {
                        id: *id,
                        pos: send_pos.then(|| qpos(pos)),
                        vel: send_vel.then(|| qvel(vel)),
                    });
                    commits.push((
                        e,
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
                    tick,
                    // Per-peer reconciliation marker (ADR-0022): the newest input
                    // seq from THIS peer that the authority has processed.
                    last_input: processed_input.get(&p).copied().unwrap_or(0),
                    entries,
                };
                match encode_state(&msg) {
                    Ok(bytes) => {
                        if bytes.len() > SAFE_DATAGRAM_BYTES {
                            log::warn!(
                                "StateMsg to {p:?} is {}B (> {SAFE_DATAGRAM_BYTES}B safe datagram) \
                                 — fragmentation loss amplification; split further in Phase 3",
                                bytes.len()
                            );
                        }
                        for (e, pos, vel) in commits {
                            let slot = send_p.entry(e).or_default();
                            if let Some(c) = pos {
                                slot.pos = Some(c);
                            }
                            if let Some(c) = vel {
                                slot.vel = Some(c);
                            }
                        }
                        seq += 1;
                        Some(bytes.into_boxed_slice())
                    }
                    Err(err) => {
                        log::error!("state encode to {p:?} failed (dropping tick): {err}");
                        None
                    }
                }
            };

            // Write this peer's state back.
            self.known.insert(p, known_p);
            self.send_state.insert(p, send_p);
            self.next_seq.insert(p, seq);

            let events: Vec<Box<[u8]>> = events
                .iter()
                .filter_map(|msg| match encode_event(msg) {
                    Ok(bytes) => Some(bytes.into_boxed_slice()),
                    Err(err) => {
                        log::error!("event encode failed (dropping event): {err}");
                        None
                    }
                })
                .collect();
            if state.is_some() || !events.is_empty() {
                result.push((p, Outbox { state, events }));
            }
        }

        // 4. Every peer has now been told about the dead + transfers — retire
        //    them from the shared id-map / intent queue. A dead entity we did
        //    NOT own (a remote proxy despawned locally without a wire Despawn)
        //    is a divergence signal — warn (it heals via Phase-3 resync).
        for e in &dead {
            if let Some(rec) = self.map.by_entity.get(e)
                && authority_of(rec.last_owner, local) != Authority::Local
            {
                log::warn!(
                    "mapped remote proxy {:?} died without a wire Despawn — divergence; \
                     Phase 3 resync heals",
                    rec.id
                );
            }
            self.map.remove_by_entity(*e);
        }
        self.pending_transfers.clear();

        result
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
        let msg_tick = msg.tick; // captured before the loop moves msg.entries
        let msg_last_input = msg.last_input;
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
            // ADR-0022: record this snapshot for interpolation IF this proxy is
            // interpolated (carries an InterpBuffer). Uses the current
            // authoritative Position (post-apply) at the message's tick — a
            // vel-only entry records the unchanged position (the entity holds
            // between snapshots). Owned/predicted entities have no buffer ⇒ skip.
            let cur = world.get::<Position>(proxy).copied();
            if let Some(pos) = cur
                && let Some(mut buf) = world.get_mut::<InterpBuffer>(proxy)
            {
                push_snapshot(
                    &mut buf,
                    Snapshot {
                        tick: msg_tick,
                        x: pos.x,
                        y: pos.y,
                    },
                );
            }
        }

        // Reconciliation (ADR-0022 Stage B): this authority has processed our
        // inputs through `last_input`, and the entries above are the state that
        // results — so drop input-history entries with `seq <= last_input` for
        // every entity WE control that THIS sender owns. The next `predict`
        // replays only the survivors from the freshly-snapped Position anchor,
        // re-pinning the prediction to server truth (no accumulation, no
        // oscillation). Skipped when last_input is 0 (nothing processed).
        if msg_last_input > 0 {
            let controlled: Vec<Entity> = world
                .query::<(Entity, &Owner, &Controlled)>()
                .iter(world)
                .filter(|(_, owner, _)| owner.0 == from)
                .map(|(e, ..)| e)
                .collect();
            for e in controlled {
                if let Some(mut hist) = world.get_mut::<InputHistory>(e) {
                    while hist.0.front().is_some_and(|i| i.seq <= msg_last_input) {
                        hist.0.pop_front();
                    }
                }
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
                // ADR-0022: a remote proxy is interpolated — attach its snapshot
                // buffer. (Stage C removes it if the proxy is later adopted to
                // Local, or is the locally-controlled predicted avatar.)
                world.entity_mut(proxy).insert(InterpBuffer::default());
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
            NetEvent::Input { seq, intent } => {
                // AUTHORITY side (ADR-0022 Stage B): queue this client's input
                // for the entity it controls (the one marked `ControlledBy(from)`
                // — set by session join / the test). `apply_input` drains one per
                // tick. A message for a peer that controls nothing is dropped.
                let target = world
                    .query::<(Entity, &ControlledBy)>()
                    .iter(world)
                    .find(|(_, cb)| cb.0 == from)
                    .map(|(e, _)| e);
                if let Some(entity) = target
                    && let Some(mut pending) = world.get_resource_mut::<PendingInputs>()
                {
                    push_pending_input(
                        &mut pending,
                        entity,
                        Input {
                            seq,
                            intent: Intent {
                                vx: dequantize(intent.x),
                                vy: dequantize(intent.y),
                            },
                        },
                    );
                }
            }
        }
    }

    /// CLIENT → AUTHORITY inputs (ADR-0022 Stage B): for every entity WE control,
    /// produce a DIRECTED `NetEvent::Input` (to its authority, i.e. its `Owner`)
    /// for each `InputHistory` entry not yet sent (`seq > input_sent`). Returns
    /// `(target, bytes)`; the caller does `send_event(target, bytes)`. Call once
    /// per pump after recording inputs; the reliable channel delivers each once.
    pub fn drain_inputs(&mut self, world: &mut World) -> Vec<(PeerId, Box<[u8]>)> {
        // Snapshot (entity, authority, unsent inputs) so the world borrow ends
        // before we mutate `input_sent`.
        let pending: Vec<(Entity, PeerId, Vec<Input>)> = {
            let mut q = world.query::<(Entity, &Owner, &Controlled, &InputHistory)>();
            q.iter(world)
                .filter_map(|(entity, owner, _, hist)| {
                    let sent = self.input_sent.get(&entity).copied().unwrap_or(0);
                    let inputs: Vec<Input> =
                        hist.0.iter().copied().filter(|i| i.seq > sent).collect();
                    (!inputs.is_empty()).then_some((entity, owner.0, inputs))
                })
                .collect()
        };
        let mut out = Vec::new();
        for (entity, authority, inputs) in pending {
            let mut max_sent = self.input_sent.get(&entity).copied().unwrap_or(0);
            for input in inputs {
                let msg = event(NetEvent::Input {
                    seq: input.seq,
                    intent: quantize_vec2(input.intent.vx, input.intent.vy),
                });
                match encode_event(&msg) {
                    Ok(bytes) => {
                        out.push((authority, bytes.into_boxed_slice()));
                        max_sent = max_sent.max(input.seq);
                    }
                    Err(err) => log::error!("input encode failed: {err}"),
                }
            }
            self.input_sent.insert(entity, max_sent);
        }
        out
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

    /// Add a peer to the tracked set (ADR-0020/0021). Only tracked peers get
    /// outboxes from [`collect_all`], so the pump MUST track every connected
    /// peer. Per-peer state (`known`/`send_state`/`next_seq`) defaults lazily on
    /// the first collect; set the peer's AOI via [`set_aoi`] (unset ⇒ sees-all).
    pub fn track_peer(&mut self, peer: PeerId) {
        self.peers.insert(peer);
    }

    /// Set (or update) a peer's area of interest — a circle in world space.
    /// Entities outside it are withheld ENTIRELY (state AND existence); a peer
    /// with no AOI set sees every owned entity (unbounded). The pump sets this
    /// each tick from the peer's focus (e.g. its avatar / camera position).
    ///
    /// NOTE (auditor): the unbounded default is FAIL-OPEN — it is a bandwidth
    /// convenience, NOT a security guarantee. A pump relying on AOI for the
    /// Mode-3 read-cheat defense MUST `set_aoi` for every peer; a forgotten call
    /// silently reveals all owned entities. (Mode 3's demo leaves it unset on
    /// purpose — clients see everything until a real gameplay focus exists.)
    pub fn set_aoi(&mut self, peer: PeerId, center: (f32, f32), radius: f32) {
        self.aoi.insert(peer, Aoi { center, radius });
    }

    /// Drop a departed peer from the tracked set and ALL per-peer state. The
    /// per-peer clears are load-bearing (ADR-0021): a same-id peer that
    /// reconnects with a fresh world must NOT be seen as already-`known`, or its
    /// AOI-enter Spawns would never fire (permanent invisibility) — and they
    /// prevent an unbounded leak of `known`/`send_state`.
    pub fn untrack_peer(&mut self, peer: PeerId) {
        self.peers.remove(&peer);
        self.acked_seq.remove(&peer);
        self.ack_sent.remove(&peer);
        self.applied_seq.remove(&peer);
        self.last_seq.remove(&peer);
        self.known.remove(&peer);
        self.send_state.remove(&peer);
        self.next_seq.remove(&peer);
        self.aoi.remove(&peer);
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
        // mint it now so the transfer + the new owner's Spawn can resolve it.
        // Existence is announced PER PEER by collect_all (no global Spawn here).
        if !self.map.by_entity.contains_key(&entity) {
            let id = NetEntityId {
                spawner: local,
                index: entity.index_u32(),
                generation: entity.generation().to_bits(),
            };
            self.map.insert(id, entity, local);
        }
        // Record the one-shot intent; the next `collect_all` announces it — to
        // every peer that knows the entity, plus the new owner regardless of AOI
        // (Spawn+Transfer) so it can build+own the proxy — then clears it.
        self.pending_transfers.insert(entity, to);
        // Flip the local Owner NOW so no double-authority window exists: from
        // this instant we stop computing and collecting it (the ≤½-RTT
        // nobody-simulates freeze is the safe direction).
        if let Some(mut owner) = world.get_mut::<Owner>(entity) {
            owner.0 = to;
        }
        if let Some(rec) = self.map.by_entity.get_mut(&entity) {
            rec.last_owner = to;
        }
        // We no longer author this entity — drop its per-peer delta baselines
        // (ADR-0021); if it ever comes back to us it re-baselines from scratch.
        if authority_of(to, local) != Authority::Local {
            for m in self.send_state.values_mut() {
                m.remove(&entity);
            }
        }
        Ok(())
    }

    /// A peer connected: track it (ADR-0021). Its entities are announced by the
    /// next [`collect_all`] via AOI-ENTER — only the ones inside its AOI, so the
    /// read-cheat holds even at join time. There is deliberately NO blanket
    /// all-owned Spawn replay anymore (that would leak the existence of
    /// out-of-AOI entities). Per-peer state defaults lazily on the first collect.
    pub fn on_peer_connected(&mut self, peer: PeerId) {
        self.track_peer(peer);
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
