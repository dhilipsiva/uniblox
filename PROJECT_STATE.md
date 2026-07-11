# PROJECT_STATE.md

Living snapshot of where uniblox is. Update it when a phase's status changes.
The **why** behind decisions lives in `DECISIONS.md`; the **what/how** lives in
`docs/final-buildspec.md`; the backlog lives in `TODO.md`.

**Current phase: PHASE 1 COMPLETE; PHASE 2 (transport hardening) IN PROGRESS.** The slice proved the
authority-swap (gate PASSED, ADR-0014) and **the Bevy client renders in-browser (ADR-0017)** with every
slice measurement taken — real two-build sizes (3.38/3.40 MB brotli), cold-load, in-browser ed25519 —
and the size-budget gate PASSES. The only measurement the slice could not take (STUN-only success rate)
is real-network-gated and lives in Phase 2's telemetry bullet. Phase 2 so far: the str0m native/server
peer (ADR-0015) + ICE policy tiers with the TURN relay proof (ADR-0016).

## Done
- **Cargo workspace** — virtual manifest, 9 crates under `crates/*` (glob members),
  size-optimized `[profile.release]`. `cargo build` + `cargo test` green (9 smoke tests).
- **Single-threaded stance** — no COOP/COEP anywhere (serve script + capability page + ADR-0003).
- **AI-workflow scaffolding** — per-crate `CLAUDE.md`, `DECISIONS.md`, four subagents
  (`test-writer`, `netcode-auditor`, `sandbox-auditor`, `reviewer`), five slash commands
  (`/build-wasm`, `/slice-check`, `/review-netcode`, `/new-crate`, `/write-tests`),
  four hooks (`.claude/settings.json` + `scripts/hooks/`), git pre-commit gate.
- **Build-pipeline scaffolding** — `scripts/build-wasm.sh`, `scripts/slice-check.sh`,
  `scripts/serve.sh`, `crates/client/web/index.html` (capability detection). `build-wasm.sh`
  runs end-to-end (tools via the flake); output is meaningless until a rendering Bevy client exists.
- **`.mcp.json`** scaffold (github, read-only postgres, docs/Context7, playwright).
- **Nix flake devShell + direnv** (ADR-0010) — `flake.nix`/`flake.lock`/`.envrc` provide a pinned
  Rust toolchain (1.96.1, wasm32 target) + `wasm-bindgen`/`wasm-opt`/`brotli`/`twiggy`/`node`,
  auto-activated on `cd` and via `direnv exec .`. Cargo/tool scripts self-activate.
- **Rhai ↔ Bevy-ECS bridge** (ADR-0011, first real deps — rhai 1.25 non-sync + bevy_ecs 0.19) —
  locked-down `new_raw()` engine + all `set_max_*` limits + `eval` disabled, held as a NonSend resource,
  mutating a whitelisted `Health` component per tick; in-memory + file hot-reload. 8 TDD tests green,
  sandbox-audited, compiles for wasm32. Full hardening is Phase 12.
- **The mode-agnostic mini-game sim** (`engine-core` + `protocol::PeerId`) — `Position`/`Velocity`/`Owner`
  components, `LocalPeer`/`SimDt` resources, `authority_of` as THE single authority decision point, one
  branching `simulate` system (Local computes; Remote is the documented apply-path placeholder — never
  re-simulates). 8 TDD tests green incl. the **Mode-2 two-perspective and Mode-3 shape proofs** — the
  authority-swap demonstrated at the unit level before transport exists. netcode-audited; wasm32-clean.
- **matchbox two-channel transport core** (ADR-0012) — `crates/transport` (matchbox 0.14; state=0
  unreliable, events=1 reliable), `crates/services` full-mesh signaling binary, hermetic native↔native
  two-peer datachannel test green, wasm client demo + `scripts/e2e-two-tab.mjs`. The nibli prior-art note
  was resolved obsolete (repo repurposed). **Browser-tab run VERIFIED (2026-07-11):** two tabs of a
  desktop-class Chromium on the Windows host (WSL2 mirrored networking; services in WSL2) each logged the
  other peer Connected plus `[STATE]`+`[EVENT]` receipts — real P2P WebRTC, data on both channels, webgpu
  build. The WSL2-HEADLESS limitation (ICE gathering never completes; matchbox wasm waits on it) still
  applies to headless CI — `scripts/e2e-two-tab.mjs` needs a non-WSL host.
- **The custom replication protocol** (ADR-0013, HIGH) — `protocol` wire format (postcard, spawner-stable
  `NetEntityId`, quantized `QVec2`, reserved signature field) + `replication` (authority-gated cached-
  `SystemState` sender, newest-seq LWW receiver, current-Owner validity, keyframes, `transfer_ownership`,
  late-join replay). **27-test battery green incl. e2e over real WebRTC** — tests committed before impl;
  netcode-audited (owned-ghost fix + documented cross-sender handoff gaps for Phase 3 resync). Snap-apply
  per decision — interpolation buffers are Phase 3.
- **THE AUTHORITY-SWAP GATE: PASSED** (ADR-0014, HIGH) — `crates/server` is the Mode-3 headless runtime
  (standalone bevy_app+bevy_time, 64 Hz FixedUpdate, exclusive net pump at ~20 Hz virtual-clock cadence,
  Messages<AppExit>). The M1–M4 battery is the documented side-by-side run: same simulation, same
  replication, Mode 2 vs Mode 3 differing ONLY in spawn/ownership data; Mode-3 clients emit zero
  messages; ~64 ticks/s evidence; net cadence decoupled from the fixed tick. netcode-audited (no hidden
  fork; its reproduced M2 replay-order flake fixed + soaked 6/6).
- **Ownership handoff (exercise once)** — CLOSED, auditor-verified against committed tests: reliable-channel
  Transfer mid-session (T26, matchbox channel config verified in source), clean transfer + stable identity
  (T18), no double-ownership (same-tick local flip + T19/T24 gates), no dropped entity (T18/T20/T26/T27),
  apply→compute switch (T10 before / T18 after — the slice's interpolate→predict stand-in per ADR-0013).
  Auditor findings actioned: T26 now genuinely asserts the replicate-back to A (the old Owner-view predicate
  was trivially true); T18 asserts the old owner's entity freezes; new T28 covers the mint-on-transfer arm.
  Phase-3 carries: adoption-switch re-verification with real buffers; hand-back/repeated/loss handoffs.
- **str0m native/server WebRTC peer** (ADR-0015, Phase 2) — `transport::Str0mPeer` (native-only): sans-IO
  str0m 0.21 (`rust-crypto`, Nix-friendly) speaking matchbox's exact signaling wire (via `matchbox_protocol`
  + blocking tungstenite) with one connection thread per remote peer running the canonical drain-to-Timeout
  loop; matchbox's negotiated no-DCEP channels (ids 0/1, labels `matchbox_socket_{i}`) pre-declared both
  roles. Interop proven hermetically: str0m↔native-matchbox BOTH role directions + str0m↔str0m, both
  channels both ways (3 tests, soaked 4/4). Fresh-reviewer gate caught the trickle-order bug (candidates
  before offer/answer are DROPPED by native matchbox — masked in tests by prflx discovery). Channel
  semantics (reliability/ordering/retransmit) are parameterized in one `CHANNEL_SPECS` source of truth that
  BOTH stacks derive from, locked by config tests (cross-stack parity by construction). Residuals:
  browser pairing (desktop-browser, ADR-0012), TLS signaling, non-loopback bind, reconnect (later items).
  **BROWSER pairing VERIFIED 2026-07-11** (`examples/str0m_browser_demo.rs` + the wasm demo in a
  desktop Chromium; both role directions, all four channel/direction combos) — and it caught a real bug the
  hermetic tests couldn't: `encode_candidate` hardcoded `sdpMid:"0"` but str0m emits a random mid, which
  Chrome rejects (`OperationError`) and matchbox-wasm panics on; webrtc-rs was lenient. Fixed by identifying
  the m-line by index only (`sdpMid:None, sdpMLineIndex:0` — single BUNDLE'd data m-line). Reviewer: MERGE.
- **STUN/TURN policy + relay proof** (ADR-0016) — `IceConfig` tiers (free = STUN-only default; Mode 3 =
  STUN+TURN with per-session paid credentials, carried never minted) + `Transport::connect_with_ice`.
  Hermetic coturn 4.13 (flake-provided) tests: relay-only webrtc-rs peers exchange a payload through the
  allocation (THE relay proof — host/srflx excluded by policy); wrong credentials gather zero relay
  candidates and never open; matchbox pass-through with only the TURN url + creds connects on both
  channels. Gotchas recorded: coturn blocks loopback peers by default (test-only `--allow-loopback-peers`);
  UDP readiness ≠ TCP readiness (STUN-binding probe in the harness). Residuals: STUN-only failure RATE is
  a real-network fleet metric; production coturn + credential issuance ride Phase 6.
- **Connection telemetry instrument** (ADR-0018) — `Str0mPeer::telemetry()` records per-peer ICE outcome
  (Connecting/Connected/Failed), winning local-candidate kind (Host today), selected addrs, and RTT
  mean/jitter from str0m's ICE keepalive stats (the candidate-pair `current_round_trip_time`, NOT the
  media-only `PeerStats.rtt` — the load-bearing detail that hung the first test). A fleet aggregates these
  into the STUN-only success fraction + RTT/jitter distributions. Hermetic str0m↔str0m test (Connected +
  RTT + Host + addrs, soaked 4/4) + finalize unit tests; live demo prints `outcome=Connected local=Host
  rtt=0.6ms jitter=0.2ms`. Real NUMBERS need a deployed fleet; browser `getStats()` classification is a
  follow-up.
- **Instrumentation (native core)** — `slice_metrics` example + `/slice-check` table. **Measured (native
  loopback, 2026-07-10):** state channel 742 B/s per peer @ 2 entities (19.4 msg/s at the 20 Hz net tick,
  ~38 B/msg — comfortably inside the ~1150 B datagram budget); events steady 0 B/s; RTT 4.3 ms ± 0.6 ms
  (loopback, ~1 ms poll-bounded); **ed25519 native sign 13.4 µs / verify 25.7 µs** — AFTER adding an
  opt-level=3 override for the crypto crates (the size-optimized `opt-level="z"` profile made verify ~35×
  slower, 1600 µs — recorded in Cargo.toml as a Phase-6 wasm size-vs-speed consideration). **In-browser
  ed25519 MEASURED (2026-07-11, desktop Chromium, release wasm + the same crypto override): sign
  19.6–23.6 µs / verify 44.5–45.6 µs — only ~1.5–1.8× native, far better than the "several×" estimate;
  an 8-peer mesh at 30 Hz costs ~1% of a core to verify sequentially, so per-frame state-channel signing
  is affordable in-browser before batching.** The crypto's wasm SIZE cost (dalek + override, measured as
  the stub build delta): +106 KB wasm-opt / +55 KB brotli — the Phase-6 tradeoff inputs are now both
  real numbers. A cold-load harness is in `web/index.html` (`[uniblox-metrics] cold-load`); the real TTI
  and STUN success rate remain gated (Bevy client / real network — see TODO).

- **The Bevy client renders + all client-gated measurements** (ADR-0017) — Bevy 0.19 as a wasm32-ONLY
  client dep (`2d`+`bevy_winit`+`webgl2`; `webgpu` feature for build 2), minimal Camera2d + bouncing
  sprite into `#uniblox-canvas`, first-frame metric. Fixed two live pipeline bugs (the page served the
  UNOPTIMIZED wasm; `wasm-opt -all` emitted stable-browser-rejected encodings — baseline feature flags
  now, which also un-broke twiggy). **Measured:** brotli 3.38/3.40 MB per build (wasm-opt ~15.6 MB);
  feature-prune saves 34% wire size (5.16→3.38 MB); --converge −0.27%; cold-load 351 ms instantiate /
  381 ms first frame local (headless webgl2), ~2.7 s download @10 Mbps. **Size-budget gate PASS**
  (≤ ~8 MB brotli; first frame in target @10 Mbps, marginal @5 Mbps). Two-tab [STATE]/[EVENT] receipts
  re-verified with Bevy in-binary. PHASE 1 COMPLETE.

## Blocked / deferred (prerequisites do not exist yet)
- **MCP reachability** (github / read-only postgres / docs / playwright) — `node`/`npx` now provided by
  the flake; still needs a running read-only Postgres role, a GitHub PAT (in `settings.local.json`), and
  Playwright browsers. (`docs` should be reachable on the flake alone; playwright's chromium is now
  installed in `~/.cache/ms-playwright` and drives the headless render/metrics runs.)
- **Web Audio worklet** investigation — needs audio added to the client (the render core exists now,
  ADR-0017; the `2d` prune excludes bevy_audio).
- **STUN-only connection success rate** — real-network peers (Phase-2 telemetry bullet).

## Next
Phase 2 (transport hardening) is underway — str0m is landed; remaining Phase-2 items: two-channel config
parameterization, STUN/TURN, reconnect/ICE-restart, plus the browser-pairing residuals. Other candidates:
the **Bevy client rendering work** (closes every Phase-1 residual: real WASM sizes, cold-load, browser
metrics, Web Audio); Phases 4–8 are LOW/MIXED and delegate-friendly. Phase 3 (replication depth) and
Phase 12 (sandbox hardening) each need a human-owned HIGH pass.

## Toolchain notes
WSL2 Ubuntu. **The toolchain comes from the Nix flake devShell** (ADR-0010): pinned Rust 1.96.1
(edition 2024, wasm32 target) + `wasm-bindgen`/`wasm-opt`/`brotli`/`twiggy`/`node`/`coturn`. Run `direnv allow`
once per clone. Interactive `cd` auto-activates; for the WSL wrapper, prefix cargo/WASM-tool/npx
commands with `direnv exec .`:
`wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && direnv exec . <CMD>"` (compound chains:
`direnv exec . bash -lc '<a && b>'`). Pure git/file commands use the plain wrapper. Ambient rustup
(cargo 1.92) still exists as a fallback for un-routed commands. Hook/build scripts self-activate the
flake and parse event JSON with `/usr/bin/python3` (the rye shim fails non-interactively; `jq` is absent).
No `just` here.
