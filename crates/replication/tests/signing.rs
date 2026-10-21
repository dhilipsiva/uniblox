//! Signed reliable events (ADR-0049; HIGH). Every reliable event except `Ack` carries the sender's
//! ed25519 public key and a signature over the sender, the recipient and the event. A receiver pins
//! each peer's key on first sight and drops an event that is unsigned, badly signed, signed for
//! another recipient or signed with a different key. An owner change must be minted by its signer
//! (`seq.coordinator == sender`). Written before the implementation.

use bevy_ecs::prelude::*;
use engine_core::{Owner, Position, Velocity, insert_sim, spawn_owned};
use protocol::{EventMsg, NetEntityId, NetEvent, OwnerSeq, PeerId, decode_event, encode_event};
use replication::{Replication, SIGNATURE_BYTES, SigningKey, seal_event};

const A: PeerId = PeerId(1);
const B: PeerId = PeerId(2);
const C: PeerId = PeerId(3);

fn key(peer: PeerId) -> SigningKey {
    SigningKey::from_bytes(&[peer.0 as u8; 32])
}

struct Peer {
    world: World,
    repl: Replication,
}

impl Peer {
    fn new(id: PeerId) -> Self {
        let mut world = World::new();
        insert_sim(&mut world, id, 0.5);
        let repl = Replication::new(&mut world, key(id));
        Peer { world, repl }
    }

    /// Everything this peer sends `to` in one collect (events only).
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

    fn owner_of(&mut self, spawner: PeerId) -> PeerId {
        let mut q = self.world.query::<(&Owner, &Position)>();
        let owners: Vec<PeerId> = q.iter(&self.world).map(|(o, _)| o.0).collect();
        assert_eq!(
            owners.len(),
            1,
            "one entity in these tests (spawned by {spawner:?})"
        );
        owners[0]
    }
}

/// A owns an entity B knows about; returns (a, b, A's entity, its id).
fn setup() -> (Peer, Peer, Entity, NetEntityId) {
    let mut a = Peer::new(A);
    let mut b = Peer::new(B);
    let e = spawn_owned(
        &mut a.world,
        A,
        Position { x: 1.0, y: 2.0 },
        Velocity { x: 0.0, y: 0.0 },
    );
    let spawn = a.events_for(B);
    assert!(!spawn.is_empty(), "A announces its entity to B");
    b.deliver(A, &spawn);
    assert_eq!(b.owner_of(A), A);
    let id = NetEntityId {
        spawner: A,
        index: e.index_u32(),
        generation: e.generation().to_bits(),
    };
    (a, b, e, id)
}

/// A hands its entity to B; the transfer A sends B.
fn transfer_bytes(a: &mut Peer, e: Entity) -> Vec<Box<[u8]>> {
    a.repl
        .transfer_ownership(&mut a.world, e, B)
        .expect("A owns it");
    a.events_for(B)
}

fn is_transfer(bytes: &[u8]) -> bool {
    matches!(
        decode_event(bytes).map(|m| m.event),
        Ok(NetEvent::OwnershipTransfer { .. })
    )
}

#[test]
fn a_signed_transfer_is_applied() {
    let (mut a, mut b, e, _) = setup();
    let events = transfer_bytes(&mut a, e);
    assert!(events.iter().any(|ev| is_transfer(ev)));
    b.deliver(A, &events);
    assert_eq!(b.owner_of(A), B);
}

/// The Done-when test: an unsigned `OwnershipTransfer` is rejected, whether a signed one stripped of
/// its signature or one crafted without.
#[test]
fn an_unsigned_ownership_transfer_is_rejected() {
    let (mut a, mut b, e, id) = setup();
    let stripped: Vec<Box<[u8]>> = transfer_bytes(&mut a, e)
        .into_iter()
        .map(|ev| {
            let mut msg = decode_event(&ev).unwrap();
            msg.sig = None;
            encode_event(&msg).unwrap().into_boxed_slice()
        })
        .collect();
    b.deliver(A, &stripped);
    assert_eq!(b.owner_of(A), A, "a stripped transfer is dropped");
    let crafted = encode_event(&EventMsg {
        version: protocol::WIRE_VERSION,
        sig: None,
        event: NetEvent::OwnershipTransfer {
            id,
            new_owner: B,
            seq: OwnerSeq {
                seq: 99,
                coordinator: A,
            },
        },
    })
    .unwrap();
    b.repl.apply_events(&mut b.world, A, &crafted);
    assert_eq!(b.owner_of(A), A, "a crafted unsigned transfer is dropped");
}

#[test]
fn a_tampered_event_is_rejected() {
    let (mut a, mut b, e, _) = setup();
    let tampered: Vec<Box<[u8]>> = transfer_bytes(&mut a, e)
        .into_iter()
        .map(|ev| {
            let mut msg = decode_event(&ev).unwrap();
            if let NetEvent::OwnershipTransfer { new_owner, .. } = &mut msg.event {
                *new_owner = C; // redirect the handoff, keep A's signature
            }
            encode_event(&msg).unwrap().into_boxed_slice()
        })
        .collect();
    b.deliver(A, &tampered);
    assert_eq!(b.owner_of(A), A);
}

#[test]
fn an_event_signed_for_another_recipient_is_rejected() {
    let (_a, mut b, _, id) = setup();
    let for_c = seal_event(
        &key(A),
        A,
        C,
        NetEvent::OwnershipTransfer {
            id,
            new_owner: C,
            seq: OwnerSeq {
                seq: 1,
                coordinator: A,
            },
        },
    )
    .unwrap();
    b.repl.apply_events(&mut b.world, A, &for_c);
    assert_eq!(b.owner_of(A), A, "C's event replayed to B is dropped");
}

#[test]
fn a_peers_key_cannot_change_mid_session() {
    let (_a, mut b, _, id) = setup();
    // B pinned A's key from the Spawn. A validly signed event under ANOTHER key, claiming to be A:
    let other = SigningKey::from_bytes(&[0xEE; 32]);
    let forged = seal_event(
        &other,
        A,
        B,
        NetEvent::OwnershipTransfer {
            id,
            new_owner: B,
            seq: OwnerSeq {
                seq: 1,
                coordinator: A,
            },
        },
    )
    .unwrap();
    b.repl.apply_events(&mut b.world, A, &forged);
    assert_eq!(b.owner_of(A), A);
}

#[test]
fn an_owner_change_must_be_minted_by_its_signer() {
    let (_a, mut b, _, id) = setup();
    // A validly signs a transfer whose rank names C as its minter: dropped.
    for event in [
        NetEvent::OwnershipTransfer {
            id,
            new_owner: B,
            seq: OwnerSeq {
                seq: 1,
                coordinator: C,
            },
        },
        NetEvent::OwnershipCommit {
            id,
            new_owner: B,
            seq: OwnerSeq {
                seq: 1,
                coordinator: C,
            },
        },
    ] {
        let bytes = seal_event(&key(A), A, B, event).unwrap();
        b.repl.apply_events(&mut b.world, A, &bytes);
        assert_eq!(b.owner_of(A), A);
    }
}

#[test]
fn every_reliable_event_but_ack_is_signed() {
    let (mut a, _b, e, _) = setup();
    let events = transfer_bytes(&mut a, e);
    assert!(!events.is_empty());
    for ev in events {
        let msg = decode_event(&ev).unwrap();
        match msg.event {
            NetEvent::Ack { .. } => {}
            other => {
                let sig = msg.sig.unwrap_or_else(|| panic!("{other:?} is unsigned"));
                assert_eq!(sig.len(), SIGNATURE_BYTES, "{other:?}");
            }
        }
    }
}

#[test]
fn the_public_key_is_the_signing_keys() {
    let mut world = World::new();
    insert_sim(&mut world, A, 0.5);
    let repl = Replication::new(&mut world, key(A));
    assert_eq!(repl.public_key(), key(A).verifying_key().to_bytes());
}
