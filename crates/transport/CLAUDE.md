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

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
