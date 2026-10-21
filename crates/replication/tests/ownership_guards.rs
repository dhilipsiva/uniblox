//! Ownership guards from the ADR-0049 netcode audit (HIGH). A rank at the top of its range is
//! refused, so no forged rank can overflow the coordinator's next mint. A transfer request is
//! honoured only from the entity's owner. A reconnecting peer gets a fresh key pin. Every event the
//! coordinator sends is signed, `ClaimRejected` included. A `LocalPeer` set after construction is the
//! sender the signatures name. Written before the implementation.

use bevy_ecs::prelude::*;
use engine_core::{Owner, Position, Velocity, insert_sim, spawn_owned};
use protocol::{NetEntityId, NetEvent, OwnerSeq, PeerId, QVec2, decode_event};
use replication::{Replication, SigningKey, seal_event};

fn key(peer: PeerId) -> SigningKey {
    let mut seed = [0x33; 32];
    seed[..8].copy_from_slice(&peer.0.to_le_bytes());
    SigningKey::from_bytes(&seed)
}

struct Peer {
    id: PeerId,
    world: World,
    repl: Replication,
}

impl Peer {
    fn new(id: u64) -> Self {
        let id = PeerId(id);
        let mut world = World::new();
        insert_sim(&mut world, id, 0.5);
        let repl = Replication::new(&mut world, key(id));
        Peer { id, world, repl }
    }

    fn events_for(&mut self, to: PeerId) -> Vec<Box<[u8]>> {
        self.repl.track_peer(to);
        self.repl
            .collect_all(&mut self.world)
            .into_iter()
            .find(|(p, _)| *p == to)
            .map(|(_, o)| o.events)
            .unwrap_or_default()
    }

    fn deliver(&mut self, from: PeerId, events: &[Box<[u8]>]) {
        for ev in events {
            self.repl.apply_events(&mut self.world, from, ev);
        }
    }

    /// The owner of the only entity this peer holds.
    fn owner(&mut self) -> PeerId {
        let mut q = self.world.query::<&Owner>();
        let owners: Vec<PeerId> = q.iter(&self.world).map(|o| o.0).collect();
        assert_eq!(owners.len(), 1);
        owners[0]
    }

    fn proxy(&mut self) -> Entity {
        let mut q = self.world.query_filtered::<Entity, With<Owner>>();
        q.iter(&self.world).next().expect("a proxy")
    }
}

/// `owner` spawns an entity and announces it to `to`; returns its id.
fn announce(owner: &mut Peer, to: &mut Peer) -> NetEntityId {
    let e = spawn_owned(
        &mut owner.world,
        owner.id,
        Position { x: 0.0, y: 0.0 },
        Velocity { x: 0.0, y: 0.0 },
    );
    let spawn = owner.events_for(to.id);
    to.deliver(owner.id, &spawn);
    NetEntityId {
        spawner: owner.id,
        index: e.index_u32(),
        generation: e.generation().to_bits(),
    }
}

fn signed(from: PeerId, to: PeerId, event: NetEvent) -> Box<[u8]> {
    seal_event(&key(from), from, to, event).expect("encode")
}

const TOP: u64 = u64::MAX;

/// A rank at the top of the range is refused on receipt, in every owner-changing event.
#[test]
fn a_saturated_rank_is_refused() {
    let mut owner = Peer::new(2);
    let mut b = Peer::new(4);
    let id = announce(&mut owner, &mut b);
    let m = PeerId(3);
    let rank = OwnerSeq {
        seq: TOP,
        coordinator: m,
    };
    for event in [
        NetEvent::OwnershipTransfer {
            id,
            new_owner: m,
            seq: rank,
        },
        NetEvent::OwnershipCommit {
            id,
            new_owner: m,
            seq: rank,
        },
        NetEvent::ResyncSpawn {
            id,
            pos: QVec2 { x: 0, y: 0 },
            vel: QVec2 { x: 0, y: 0 },
            seq: rank,
        },
    ] {
        b.repl.apply_events(&mut b.world, m, &signed(m, b.id, event));
        assert_eq!(b.owner(), owner.id, "a top rank never applies");
    }
}

/// The coordinator never mints past the range: with an entity at the highest accepted rank, the
/// next claim is rejected, not overflowed (a panic in debug, a silent wrap in release).
#[test]
fn the_coordinator_never_overflows_a_rank() {
    let mut owner = Peer::new(2);
    let mut coord = Peer::new(1);
    let id = announce(&mut owner, &mut coord);
    coord.repl.track_peer(owner.id);
    let claimant = PeerId(5);
    coord.repl.track_peer(claimant);
    // The owner (legitimately, as far as the coordinator can tell) hands it off at the highest
    // rank a receiver accepts.
    let high = signed(
        owner.id,
        coord.id,
        NetEvent::OwnershipTransfer {
            id,
            new_owner: PeerId(6),
            seq: OwnerSeq {
                seq: TOP - 1,
                coordinator: owner.id,
            },
        },
    );
    coord.repl.apply_events(&mut coord.world, owner.id, &high);
    assert_eq!(coord.owner(), PeerId(6));
    let claim = signed(claimant, coord.id, NetEvent::ClaimOwnership { id });
    coord.repl.apply_events(&mut coord.world, claimant, &claim);
    let out = coord.repl.drain_commits(&mut coord.world);
    let events: Vec<NetEvent> = out
        .iter()
        .map(|(_, bytes)| decode_event(bytes).unwrap().event)
        .collect();
    assert!(
        events.iter().any(|e| matches!(e, NetEvent::ClaimRejected { .. })),
        "the claim is rejected: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, NetEvent::OwnershipCommit { .. })),
        "nothing is minted past the range: {events:?}"
    );
    assert_eq!(coord.owner(), PeerId(6));
}

/// The coordinator honours a `TransferRequest` only from the entity's current owner (the rule the
/// sender side already enforces).
#[test]
fn a_transfer_request_from_a_non_owner_is_ignored() {
    let mut owner = Peer::new(2);
    let mut coord = Peer::new(1);
    let id = announce(&mut owner, &mut coord);
    coord.repl.track_peer(owner.id);
    let m = PeerId(3);
    coord.repl.track_peer(m);
    let forged = signed(m, coord.id, NetEvent::TransferRequest { id, to: m });
    coord.repl.apply_events(&mut coord.world, m, &forged);
    assert!(!coord.repl.has_pending_transfer_request(id));
    assert!(coord.repl.drain_commits(&mut coord.world).is_empty());
    assert_eq!(coord.owner(), owner.id);
    let honest = signed(owner.id, coord.id, NetEvent::TransferRequest { id, to: m });
    coord.repl.apply_events(&mut coord.world, owner.id, &honest);
    assert!(coord.repl.has_pending_transfer_request(id));
}

/// A peer that reconnects (a fresh `on_peer_connected`) may present a new key; a straggler event
/// from its previous connection cannot lock it out.
#[test]
fn a_reconnecting_peer_gets_a_fresh_key_pin() {
    let mut a = Peer::new(1);
    let mut b = Peer::new(2);
    let id = announce(&mut a, &mut b); // B pins A's key from the Spawn
    b.repl.untrack_peer(a.id);
    // A straggler from the old connection, under the old key.
    let straggler = signed(a.id, b.id, NetEvent::Despawn { id: NetEntityId { index: 999, ..id } });
    b.repl.apply_events(&mut b.world, a.id, &straggler);
    b.repl.on_peer_connected(a.id);
    let new_key = SigningKey::from_bytes(&[0x77; 32]);
    let transfer = seal_event(
        &new_key,
        a.id,
        b.id,
        NetEvent::OwnershipTransfer {
            id,
            new_owner: b.id,
            seq: OwnerSeq {
                seq: 1,
                coordinator: a.id,
            },
        },
    )
    .unwrap();
    b.repl.apply_events(&mut b.world, a.id, &transfer);
    assert_eq!(b.owner(), b.id, "the reconnected peer's new key is pinned afresh");
}

/// Every event the coordinator sends while arbitrating is signed, `ClaimRejected` included.
#[test]
fn the_coordinators_commits_and_rejections_are_signed() {
    let mut owner = Peer::new(4);
    let mut coord = Peer::new(1);
    let id = announce(&mut owner, &mut coord);
    for p in [owner.id, PeerId(2), PeerId(3)] {
        coord.repl.track_peer(p);
    }
    for claimant in [PeerId(2), PeerId(3)] {
        let claim = signed(claimant, coord.id, NetEvent::ClaimOwnership { id });
        coord.repl.apply_events(&mut coord.world, claimant, &claim);
    }
    let out = coord.repl.drain_commits(&mut coord.world);
    let mut kinds = Vec::new();
    for (_, bytes) in &out {
        let msg = decode_event(bytes).unwrap();
        assert!(
            msg.sig.as_ref().is_some_and(|s| s.len() == replication::SIGNATURE_BYTES),
            "{:?} is unsigned",
            msg.event
        );
        kinds.push(msg.event);
    }
    assert!(kinds.iter().any(|e| matches!(e, NetEvent::ClaimRejected { .. })));
    assert!(kinds.iter().any(|e| matches!(e, NetEvent::OwnershipCommit { .. })));
}

/// A host that sets `LocalPeer` after constructing the replicator still signs as that peer.
#[test]
fn a_local_peer_set_after_construction_is_the_signer() {
    let mut world = World::new();
    let repl = Replication::new(&mut world, key(PeerId(1)));
    insert_sim(&mut world, PeerId(1), 0.5);
    let mut a = Peer {
        id: PeerId(1),
        world,
        repl,
    };
    let mut b = Peer::new(2);
    let _ = announce(&mut a, &mut b);
    let _ = b.proxy(); // B accepted A's signed Spawn
}
