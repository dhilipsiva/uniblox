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
str0m↔str0m, both channels, both ways (`tests/str0m_interop.rs`), and
**VERIFIED against a real desktop BROWSER** (2026-07-11,
`examples/str0m_browser_demo.rs` + the wasm demo, both roles) — that run
caught the `sdpMid` bug below.
**`IceConfig` (ADR-0016)**: `stun_only()` free tier / `with_turn(urls, user,
credential)` Mode-3 paid tier + `Transport::connect_with_ice`; the TURN relay
path is proven hermetically against a flake-provided coturn
(`tests/turn_relay.rs` — relay-only webrtc-rs peers, credential negative,
matchbox pass-through).
**`Str0mPeer::telemetry()` + `FleetMetrics::aggregate` (ADR-0018)**: per-peer
ICE outcome (Connecting/Connected/Failed), winning local-candidate kind, and
RTT mean/jitter; the aggregation turns many records into the STUN-only success
fraction + candidate-kind breakdown + RTT/jitter distribution (pure, unit
tested).
**Reconnect / ICE-restart (ADR-0019)**: transient ICE `Disconnected` is
tolerated (self-heal window), the offerer does an in-place `ice_restart` if it
persists (channels survive), the signaling WS reconnects with backoff without
killing live connections, and a hard failure triggers a bounded full reconnect;
`request_ice_restart(peer)` is the ops/test trigger; `reconnects`/`ice_restarts`
telemetry. Residuals: TLS signaling, non-loopback bind
(later Phase-2 items); per-session TURN credential issuance is Phase 6;
real-network telemetry NUMBERS need a deployed fleet; browser `getStats()`
candidate classification is a follow-up.

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
- **Trickled ICE candidates identify the m-line by INDEX, not a hardcoded
  `sdpMid`** (`encode_candidate`): str0m emits a RANDOM mid (`a=mid:SrN`), so a
  hardcoded `sdpMid:"0"` mismatches it and strict browsers (Chrome) REJECT the
  candidate → matchbox-wasm panics. webrtc-rs is lenient (hid it from the
  hermetic tests). We always have one BUNDLE'd data m-line, so
  `sdpMid:None, sdpMLineIndex:Some(0)` is correct for both roles and both
  stacks. **Lesson: browsers are stricter than webrtc-rs — verify real-browser
  interop, don't trust native-matchbox-only tests.**
- **Telemetry RTT comes from the ICE candidate pair, not `PeerStats.rtt`**
  (ADR-0018): `PeerStats.rtt` is RTP/media-derived and stays `None` for a
  DataChannels-only session; the ICE keepalive RTT is
  `selected_candidate_pair.current_round_trip_time`. Stats are OFF by default —
  build the `Rtc` via `RtcConfig::new().set_stats_interval(Some(..)).build()`.
  Handling `Event::PeerStats` is read-only, so the drain invariant is untouched.
- **ICE `Disconnected` is TRANSIENT, not fatal** (ADR-0019): it self-recovers;
  don't tear down. The offerer ICE-restarts in place if it persists past the
  grace; the answerer heals via the re-offer. A **signaling WS drop must NOT
  kill live connections** (WebRTC is independent of signaling) — reconnect the
  WS, don't `close_all`. Only the OFFERER auto-restarts / re-establishes (glare
  avoidance). A **re-offer must reuse existing channels**, not recreate them.
- **ICE policy is tier data** (`IceConfig`, ADR-0016): free = STUN-only,
  Mode 3 = STUN+TURN with per-session credentials carried (never minted) by
  the transport. The hermetic coturn tests need `--allow-loopback-peers` —
  test-only, NEVER on a production coturn.
- Transport `PeerId` is matchbox's UUID (signaling-assigned) — distinct from
  `protocol::PeerId`; the mapping is a session-layer concern (replication/join).
- matchbox 0.14 wasm sends its offer only after ICE-gathering-COMPLETE (non-trickle,
  upstream TODO) — under WSL2 HEADLESS Chrome gathering never completes with any
  iceServers set, so headless browser E2E (`scripts/e2e-two-tab.mjs`) must run on a
  non-WSL host. The two-tab DESKTOP run itself is verified (2026-07-11): Windows-host
  Chromium tabs against the WSL2-hosted services (mirrored networking) exchanged
  `[STATE]`+`[EVENT]` receipts both ways.

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
