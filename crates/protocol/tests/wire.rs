//! Tier A — codec-pure wire-format tests (T1–T8). Locked FIRST (TDD).
//!
//! No World, no transport: quantization bounds, round-trips, derived masks,
//! version checking, decode robustness, and the PeerId truncation contract.

use protocol::{
    EventMsg, NetEntityId, NetEvent, PeerId, QVec2, StateEntry, StateMsg, WIRE_VERSION, WireError,
    decode_event, decode_state, dequantize, encode_event, encode_state, quantize, quantize_vec2,
};

fn id(spawner: u64, index: u32, generation: u32) -> NetEntityId {
    NetEntityId {
        spawner: PeerId(spawner),
        index,
        generation,
    }
}

/// T1 ★ quantize/dequantize round-trip within tolerance (≤ 1/2048) across the
/// documented envelope |v| ≤ 16384, including edges.
#[test]
fn quantize_round_trip_within_tolerance() {
    let tolerance = 0.5 / 1024.0;
    let mut checked = 0u32;
    // Dense grid: 16k points across the envelope.
    for i in -8192..=8192 {
        let v = i as f32 * 2.0; // covers [-16384, 16384] in steps of 2
        let err = (dequantize(quantize(v)) - v).abs();
        assert!(err <= tolerance, "v={v}: err={err} > {tolerance}");
        checked += 1;
    }
    // Edges and awkward values.
    for v in [
        0.0,
        1.0 / 2048.0,
        -1.0 / 2048.0,
        0.5,
        -0.5,
        16384.0,
        -16384.0,
        0.1,
        -3.75,
        1234.5678,
    ] {
        let err = (dequantize(quantize(v)) - v).abs();
        assert!(err <= tolerance, "v={v}: err={err} > {tolerance}");
        checked += 1;
    }
    assert!(checked > 16000);
}

/// T2 — extreme/non-finite inputs saturate (or map to 0), never panic.
/// NOTE: quantize carries a debug_assert on finiteness (a NaN position is an
/// upstream sim bug) — the saturation contract is exercised in release shape
/// here via the finite extremes; NaN/∞ behavior is documented as saturating
/// `as` semantics.
#[test]
fn quantize_extremes_saturate_not_panic() {
    assert_eq!(quantize(f32::MAX), i32::MAX);
    assert_eq!(quantize(f32::MIN), i32::MIN);
    // Beyond the i32 range but finite: saturates.
    assert_eq!(quantize(3.0e6), i32::MAX);
    assert_eq!(quantize(-3.0e6), i32::MIN);
}

/// T3 ★ StateMsg round-trips exactly (pos-only, vel-only, both).
#[test]
fn state_msg_round_trip_exact() {
    let msg = StateMsg {
        version: WIRE_VERSION,
        seq: 42,
        tick: 9001,
        last_input: 17,
        entries: vec![
            StateEntry {
                id: id(1, 7, 0),
                pos: Some(QVec2 { x: 1024, y: -2048 }),
                vel: None,
            },
            StateEntry {
                id: id(1, 8, 3),
                pos: None,
                vel: Some(QVec2 { x: 0, y: 512 }),
            },
            StateEntry {
                id: id(2, 0, 1),
                pos: Some(QVec2 { x: -1, y: 1 }),
                vel: Some(QVec2 { x: 2, y: -2 }),
            },
        ],
    };
    let bytes = encode_state(&msg).unwrap();
    assert_eq!(decode_state(&bytes).unwrap(), msg);
}

/// T4 ★ a changed-only entry encodes smaller than a full entry, and the
/// DERIVED mask matches the changed set.
#[test]
fn changed_only_entry_smaller_and_mask_matches() {
    let pos_only = StateEntry {
        id: id(1, 7, 0),
        pos: Some(QVec2 { x: 1024, y: 1024 }),
        vel: None,
    };
    let both = StateEntry {
        id: id(1, 7, 0),
        pos: Some(QVec2 { x: 1024, y: 1024 }),
        vel: Some(QVec2 { x: 1024, y: 1024 }),
    };
    assert_eq!(pos_only.mask(), 0b01);
    assert_eq!(both.mask(), 0b11);
    assert_eq!(
        StateEntry {
            id: id(1, 7, 0),
            pos: None,
            vel: Some(QVec2 { x: 1, y: 1 }),
        }
        .mask(),
        0b10
    );

    let wrap = |e: StateEntry| StateMsg {
        version: WIRE_VERSION,
        seq: 1,
        tick: 0,
        last_input: 0,
        entries: vec![e],
    };
    let pos_only_len = encode_state(&wrap(pos_only)).unwrap().len();
    let both_len = encode_state(&wrap(both)).unwrap().len();
    assert!(
        pos_only_len < both_len,
        "pos-only ({pos_only_len}B) must encode smaller than both ({both_len}B)"
    );
}

/// T5 ★ all event variants round-trip; the reserved signature field survives
/// as None AND can carry an ed25519-sized payload.
#[test]
fn event_round_trip_reserves_signature() {
    let events = [
        NetEvent::Spawn {
            id: id(1, 7, 0),
            pos: quantize_vec2(1.0, -2.0),
            vel: quantize_vec2(0.5, 0.0),
        },
        NetEvent::Despawn { id: id(1, 7, 0) },
        NetEvent::OwnershipTransfer {
            id: id(1, 7, 0),
            new_owner: PeerId(9),
        },
    ];
    for event in events {
        let unsigned = EventMsg {
            version: WIRE_VERSION,
            sig: None,
            event: event.clone(),
        };
        let bytes = encode_event(&unsigned).unwrap();
        let decoded = decode_event(&bytes).unwrap();
        assert_eq!(decoded, unsigned);
        assert!(decoded.sig.is_none());

        // The reserved field must be able to carry a 64-byte ed25519 signature.
        let signed = EventMsg {
            version: WIRE_VERSION,
            sig: Some(vec![0xAB; 64]),
            event,
        };
        let bytes = encode_event(&signed).unwrap();
        assert_eq!(decode_event(&bytes).unwrap(), signed);
    }
}

/// T6 — a flipped version byte is rejected by both decoders.
#[test]
fn decode_rejects_wrong_version() {
    let state = StateMsg {
        version: WIRE_VERSION + 1,
        seq: 1,
        tick: 0,
        last_input: 0,
        entries: vec![],
    };
    let bytes = postcard::to_stdvec(&state).unwrap();
    assert!(matches!(
        decode_state(&bytes),
        Err(WireError::VersionMismatch { got }) if got == WIRE_VERSION + 1
    ));

    let event = EventMsg {
        version: WIRE_VERSION + 1,
        sig: None,
        event: NetEvent::Despawn { id: id(1, 0, 0) },
    };
    let bytes = postcard::to_stdvec(&event).unwrap();
    assert!(matches!(
        decode_event(&bytes),
        Err(WireError::VersionMismatch { got }) if got == WIRE_VERSION + 1
    ));
}

/// T7 — decoders never panic on garbage: every truncation of a valid message
/// plus seeded pseudo-random byte strings return Ok or Err, never abort.
#[test]
fn decode_never_panics_on_garbage() {
    let valid = encode_state(&StateMsg {
        version: WIRE_VERSION,
        seq: u64::MAX,
        tick: u64::MAX,
        last_input: u64::MAX,
        entries: vec![StateEntry {
            id: id(u64::MAX, u32::MAX - 1, u32::MAX),
            pos: Some(QVec2 {
                x: i32::MIN,
                y: i32::MAX,
            }),
            vel: None,
        }],
    })
    .unwrap();
    for cut in 0..valid.len() {
        let _ = decode_state(&valid[..cut]);
        let _ = decode_event(&valid[..cut]);
    }
    // Deterministic LCG garbage (no rand dep).
    let mut x: u64 = 0x1234_5678_9abc_def0;
    for len in 0..200 {
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (x >> 56) as u8
            })
            .collect();
        let _ = decode_state(&bytes);
        let _ = decode_event(&bytes);
    }
}

/// T8 — PeerId::from_uuid_bytes is a stable pure function of the FIRST 8
/// bytes (big-endian), documented truncation included: UUIDs differing only in
/// the last 8 bytes map to the SAME PeerId. All peers must agree on this map.
#[test]
fn peer_id_from_uuid_bytes_stable_and_documented() {
    let uuid: [u8; 16] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00,
        0x11,
    ];
    let expected = u64::from_be_bytes([0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
    assert_eq!(PeerId::from_uuid_bytes(uuid), PeerId(expected));
    // Pure: same input, same output.
    assert_eq!(PeerId::from_uuid_bytes(uuid), PeerId::from_uuid_bytes(uuid));
    // Documented truncation: last-8-bytes differences do NOT change the id.
    let mut uuid2 = uuid;
    uuid2[15] = 0x99;
    assert_eq!(
        PeerId::from_uuid_bytes(uuid),
        PeerId::from_uuid_bytes(uuid2)
    );
    // First-8-bytes differences DO change it.
    let mut uuid3 = uuid;
    uuid3[0] = 0xFF;
    assert_ne!(
        PeerId::from_uuid_bytes(uuid),
        PeerId::from_uuid_bytes(uuid3)
    );
}
