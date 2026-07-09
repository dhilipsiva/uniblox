# CLAUDE.md — `transport`

**Purpose:** matchbox (browser) + str0m (native/server) abstraction; two-channel
config — unreliable/unordered state + reliable/ordered events.
**Risk tier:** standard (the str0m sans-IO poll/timeout loop is MIXED — human-review it in Phase 2).

## Status
Stub (Phase 1 scaffolding). No functional code yet.

## Crate-local invariants
- **WebRTC DataChannels only — no media, no SFU, anywhere.**
- Exactly two channels: Channel 0 unreliable `{ ordered: false, maxRetransmits: 0 }`
  (state); Channel 1 reliable `{ ordered: true }` (events/handoffs/resync).
- Set **at most one** of `maxRetransmits` / `maxPacketLifeTime` (both is an error).
- Evaluate `github.com/dhilipsiva/nibli` (existing browser-native WebRTC P2P
  transport) before hand-rolling plumbing.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
