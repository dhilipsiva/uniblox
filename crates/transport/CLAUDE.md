# CLAUDE.md — `transport`

**Purpose:** matchbox (browser) + str0m (native/server) abstraction; two-channel
config — unreliable/unordered state + reliable/ordered events.
**Risk tier:** standard (the str0m sans-IO driving loop was the MIXED human-review
point — reviewed and landed, ADR-0015).

## Status
Implemented. matchbox 0.14 two-channel wrapper (ADR-0012): `Transport::connect`
builds `CHANNEL_STATE`(0, unreliable/unordered) + `CHANNEL_EVENTS`(1,
reliable/ordered); `connect_hermetic` (native-only — browsers reject empty ICE
entries) for offline tests; proven by a hermetic native↔native two-peer test.
**`Str0mPeer` (ADR-0015, native-only `str0m_peer` module)**: sans-IO str0m 0.21
peer speaking matchbox's signaling protocol (via `matchbox_protocol` +
blocking tungstenite) with one connection thread per remote peer; interop
proven hermetically str0m↔native-matchbox (both role directions) and
str0m↔str0m, both channels, both ways (`tests/str0m_interop.rs`). Residuals:
browser pairing (desktop-browser, ADR-0012), TLS signaling, non-loopback bind,
reconnect/ICE-restart (later Phase-2 items).

## Crate-local invariants
- **WebRTC DataChannels only — no media, no SFU, anywhere.**
- Exactly two channels: Channel 0 unreliable `{ ordered: false, maxRetransmits: 0 }`
  (state); Channel 1 reliable `{ ordered: true }` (events/handoffs/resync).
  **Semantics are defined ONCE in `CHANNEL_SPECS` (`src/lib.rs`)** — both the
  matchbox path (`Transport::build`) and the str0m path (`channel_configs()`)
  derive from it (locked by unit tests). Array index = channel index =
  insertion order = negotiated stream id — never reorder the entries; the
  count is fixed at two (parameterize semantics there, never the layout).
- **matchbox channels are `negotiated` (NO DCEP)**: stream id = channel index,
  labels `matchbox_socket_{i}`. `Str0mPeer` pre-declares both and never waits
  for DCEP opens. Connected = ALL channels open.
- Set **at most one** of `maxRetransmits` / `maxPacketLifeTime` (both is an error).
- **The sans-IO invariant** (`str0m_peer.rs`): after EVERY `Rtc` mutation
  (`handle_input`, SDP change, `channel.write`, `add_remote_candidate`), drain
  `poll_output()` to `Output::Timeout` before the next mutation. The loop
  structure guarantees this — keep it that way (commands `continue` into the
  drain; input feeds the loop-top drain).
- **Trickle candidates AFTER the offer/answer** — native matchbox drops
  out-of-phase candidates (its handshake loops ignore them); a pre-offer
  trickle "works" in tests only via peer-reflexive discovery.
- Transport `PeerId` is matchbox's UUID (signaling-assigned) — distinct from
  `protocol::PeerId`; the mapping is a session-layer concern (replication/join).
- matchbox 0.14 wasm sends its offer only after ICE-gathering-COMPLETE (non-trickle,
  upstream TODO) — under WSL2 headless Chrome gathering never completes with any
  iceServers set, so browser E2E must run on a desktop browser / non-WSL host
  (`scripts/e2e-two-tab.mjs`).

## Raw DataChannel/SCTP parameters (the record)

What each stack actually passes to its WebRTC implementation for the two
channels. The code derives all of this from `CHANNEL_SPECS` (`src/lib.rs`);
this table is the written record so future work (TURN, reconnect, browser
verification, new backends) can check semantics without re-deriving from
three codebases. Values verified from vendored matchbox_socket 0.14
`wasm.rs`/`native.rs` and our `str0m_peer.rs`.

| Parameter | Channel 0 — `CHANNEL_STATE` (LWW snapshots) | Channel 1 — `CHANNEL_EVENTS` (events/handoffs/resync) |
|---|---|---|
| Label | `matchbox_socket_0` | `matchbox_socket_1` |
| Negotiated stream id | `0` (out-of-band, no DCEP) | `1` (out-of-band, no DCEP) |
| `ordered` | `false` | `true` |
| `maxRetransmits` | `0` (send once, never retransmit) | unset (unlimited — reliable) |
| `maxPacketLifeTime` | **never set** | **never set** |

Per stack (identical semantics, different APIs):
- **Browser matchbox (web-sys `RtcDataChannelInit`)**: `set_ordered(spec)`,
  `set_negotiated(true)` + `set_id(index)`, `set_max_retransmits(n)` only when
  the spec has `Some(n)`; binaryType arraybuffer.
- **Native matchbox (webrtc-rs `RTCDataChannelInit`)**: `ordered: Some(spec)`,
  `negotiated: Some(index)` (webrtc-rs conflates negotiated+stream-id into one
  field), `max_retransmits: spec`.
- **str0m (`ChannelConfig`)**: `label`, `ordered: spec`,
  `reliability: Reliable | MaxRetransmits{retransmits}`,
  `negotiated: Some(index)`, `protocol: ""`.

SCTP level: stream id = channel index; channel 0 rides unordered delivery
(U-bit) with the RFC 3758 PR-SCTP limited-retransmission policy (rexmit = 0);
channel 1 is fully reliable/ordered. DCEP (RFC 8832) is NOT used — channel
parameters are agreed out-of-band by both sides constructing them from the
same spec. `maxPacketLifeTime` is deliberately unexpressed in `ChannelSpec`:
matchbox 0.14 cannot set it, and the WebRTC spec forbids setting both it and
`maxRetransmits` — the at-most-one constraint holds by construction.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
