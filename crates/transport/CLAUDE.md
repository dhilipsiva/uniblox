# CLAUDE.md — `transport`

**Purpose:** matchbox (browser) + str0m (native/server) abstraction; two-channel
config — unreliable/unordered state + reliable/ordered events.
**Risk tier:** standard (the str0m sans-IO poll/timeout loop is MIXED — human-review it in Phase 2).

## Status
Implemented (matchbox 0.14 two-channel wrapper, ADR-0012). `Transport::connect`
builds `CHANNEL_STATE`(0, unreliable/unordered) + `CHANNEL_EVENTS`(1,
reliable/ordered); `connect_hermetic` (native-only — browsers reject empty ICE
entries) for offline tests. Proven by a hermetic native↔native two-peer
datachannel test through an in-process signaling server. The str0m abstraction
(Phase 2) slots in behind the same API.

## Crate-local invariants
- **WebRTC DataChannels only — no media, no SFU, anywhere.**
- Exactly two channels: Channel 0 unreliable `{ ordered: false, maxRetransmits: 0 }`
  (state); Channel 1 reliable `{ ordered: true }` (events/handoffs/resync).
  Channel index = builder insertion order — never reorder the two `add_*_channel` calls.
- Set **at most one** of `maxRetransmits` / `maxPacketLifeTime` (both is an error).
- Transport `PeerId` is matchbox's UUID (signaling-assigned) — distinct from
  `protocol::PeerId`; the mapping is a session-layer concern (replication/join).
- matchbox 0.14 wasm sends its offer only after ICE-gathering-COMPLETE (non-trickle,
  upstream TODO) — under WSL2 headless Chrome gathering never completes with any
  iceServers set, so browser E2E must run on a desktop browser / non-WSL host
  (`scripts/e2e-two-tab.mjs`).

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
