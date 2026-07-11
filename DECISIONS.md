# DECISIONS.md — Architecture Decision Record (ADR) log

Append-only record of **why**. `CLAUDE.md` is the operating summary of the settled
invariants; this file is the durable rationale so future sessions record new
decisions instead of relitigating old ones. Full reasoning: `docs/CONTEXT.md`.

Each ADR: **Context / Decision / Consequences / Status.** Never edit an accepted
ADR's decision — supersede it with a new, higher-numbered ADR.

---

## ADR-0001 — Cargo workspace, multi-crate, `crates/*` glob layout
- **Context:** the repo began as a single hello-world crate; the settled design is a
  9-crate workspace (`docs/final-todo.md` §Workspace layout).
- **Decision:** virtual workspace manifest at the root; members nested under `crates/`
  via the glob `members = ["crates/*"]`. Flat root-level crates rejected (clutters the
  root control plane; glob avoids editing `members` on every new crate).
- **Consequences:** `/new-crate` just drops a directory under `crates/`; the root stays
  a clean control plane (docs, .claude, scripts, trackers).
- **Status:** Accepted (Phase 1 scaffolding).

## ADR-0002 — Two WASM builds, not one
- **Context:** Bevy cannot serve WebGPU and WebGL2 from one binary (issue #13168 open;
  the `webgpu` feature overrides `webgl2`). Prior reports claimed a single runtime-selecting
  binary — refuted.
- **Decision:** ship two `cargo build --target wasm32-unknown-unknown` artifacts (WebGPU with
  `--features webgpu` + `RUSTFLAGS=--cfg=web_sys_unstable_apis`; WebGL2 default) + JS capability detection.
- **Consequences:** 2× build/CI; a capability-detection page (`crates/client/web/index.html`) picks the build.
- **Status:** Accepted.

## ADR-0003 — Single-threaded WASM at launch; no COOP/COEP
- **Context:** SharedArrayBuffer/threads require cross-origin isolation (COOP/COEP), which
  severs `window.opener` and breaks the OAuth sign-in and payment-checkout popups Mode 3 needs.
- **Decision:** single-threaded at launch; set no cross-origin-isolation headers anywhere.
- **Consequences:** single-thread stutter accepted; native is the performance tier. Audio may
  later move to a Web Audio worklet (its own thread, no COOP/COEP) — deferred investigation.
- **Status:** Accepted.

## ADR-0004 — Custom replication protocol (not lightyear / replicon / renet)
- **Context:** no existing crate backs all three modes over WebRTC DataChannels by varying only
  authority. lightyear defers IO to aeronet (no WebRTC-DataChannel layer); bevy_replicon is
  server→client-only.
- **Decision:** a first-party per-entity replication protocol in `crates/replication` (HIGH-RISK).
- **Consequences:** more work + the dominant "compiles but subtly wrong" risk ⇒ TDD + `netcode-auditor`.
- **Status:** Accepted.

## ADR-0005 — Thin custom Rhai bridge (not `bevy_mod_scripting`)
- **Context:** BMS lists WASM as unsupported (issue #166) — disqualifying for a browser-first platform.
- **Decision:** a thin first-party bridge in `crates/scripting` — `Engine::new_raw()` + explicit
  `register_*`, all `set_max_*` limits, `unchecked`/`internals` features OFF.
- **Consequences:** every capability is explicit; the sandbox protects the machine, not the game.
- **Status:** Accepted.

## ADR-0006 — Size-optimized release profile; tests keep unwind
- **Context:** WASM size is a load-bearing risk; the ship build wants aggressive size optimization.
- **Decision:** `[profile.release]` = `opt-level="z"`, `lto=true`, `codegen-units=1`, `strip=true`,
  `panic="abort"`. Scoped to release ONLY — `cargo test` builds under the dev/test profile (unwind),
  so the harness is unaffected.
- **Consequences:** slow release builds (lto + 1 codegen unit) accepted. **Do not run `cargo test
  --release`** (it would build the harness under the abort profile).
- **Status:** Accepted.

## ADR-0007 — Version pins centralized in `[workspace.dependencies]`
- **Context:** the Bevy ecosystem moves fast (pre-1.0, ~3-month cadence); per-crate pins drift.
- **Decision:** every third-party version pin lives once in the root `[workspace.dependencies]`;
  member crates use `dep.workspace = true`.
- **Consequences:** one place to bump per Bevy cycle; no cross-crate version skew.
- **Status:** Accepted (table empty during scaffolding — zero deps).

## ADR-0008 — `tests/` edits gated by a `.claude/allow-test-edits` sentinel
- **Context:** the AI workflow commits tests before implementation so `git diff tests/` proves the
  tests were not tampered with to make implementation pass. A hook must block `tests/` edits during
  implementation turns but allow them during test-writing turns.
- **Decision:** a PreToolUse hook (`scripts/hooks/guard-tests.sh`) blocks edits to any `tests/`
  path unless the sentinel file `.claude/allow-test-edits` exists. `/write-tests` (and the
  `test-writer` subagent) create the sentinel; the Stop hook clears it so it never leaks past a turn.
- **Consequences:** implementation turns cannot silently rewrite tests; the sentinel is gitignored.
- **Status:** Accepted.

## ADR-0009 — Blocking gate split: clippy on Stop, full tests on pre-commit
- **Context:** the always-do gate is `cargo clippy --all-targets -- -D warnings` + `cargo test`.
  Running the full suite on every turn-end through the WSL bridge is too slow/disruptive.
- **Decision:** the `Stop` hook runs clippy only (incremental, fast, blocking on warnings); the full
  `cargo test` hard gate runs at the commit boundary via a tracked git `pre-commit` hook
  (`scripts/git-hooks/pre-commit`, wired with `git config core.hooksPath scripts/git-hooks`).
- **Consequences:** clippy blocks turn-end; both clippy and tests block a commit. Least-disruptive
  design that still satisfies "blocking clippy + test gate."
- **Status:** Accepted.

## ADR-0010 — Nix flake devShell owns Rust + the WASM toolchain; direnv auto-activation
- **Context:** `wasm-bindgen`, `wasm-opt`, `brotli`, `twiggy`, and `node`/`npx` were all absent on the WSL2
  host (the ambient `npx` was a `/mnt/c/...` Windows-interop shim over a missing WSL node), blocking the build
  pipeline and the MCP servers. The host already had Nix 2.33 (flakes on) + direnv 2.37 + nix-direnv 3.1.2.
  This **supersedes** the earlier informal "there is no Nix dev-shell — call `cargo` directly" stance recorded
  in `PROJECT_STATE.md` / the skill.
- **Decision:** a per-repo `flake.nix` `devShells.default` provides a **pinned Rust toolchain** (via
  `oxalica/rust-overlay`, `wasm32-unknown-unknown` target + clippy/rustfmt/rust-src) **and** `wasm-bindgen-cli`,
  `binaryen` (wasm-opt), `brotli`, `twiggy`, `nodejs`. `.envrc` is `use flake`; `flake.lock` is committed so the
  whole toolchain (including cargo/rustc — realizing ADR-0007's pinning intent for Rust itself) is reproducible.
  Interactive `cd` auto-activates via the existing direnv hook + nix-direnv. The non-interactive WSL wrapper is
  **targeted**: cargo/WASM-tool/npx commands get a `direnv exec .` prefix; pure git/python3-only commands are
  unchanged. Cargo/tool-bearing **scripts self-activate** (`eval "$(direnv export bash)" 2>/dev/null || true`
  after their `cd`) — `scripts/build-wasm.sh`, `scripts/hooks/gate-clippy.sh`, `scripts/hooks/fmt-on-write.sh`,
  `scripts/git-hooks/pre-commit` — so they hit the flake however invoked, falling back to ambient rustup if the
  env is unavailable.
- **Consequences:** one-time `direnv allow` per clone; first build fetches the toolchain from the nix cache
  (minutes, cold). rustup (cargo 1.92) still exists ambiently — only *routed* commands use the flake (1.96.1);
  the ambient fallback is benign. `wasm-bindgen-cli` must match the `wasm-bindgen` crate version once Bevy is
  added — handled by a commented `.override` in `flake.nix`, decoupled from the nixpkgs rev. `.mcp.json` `npx`
  invocations route through `direnv exec`. `.direnv/` and `/result` are gitignored.
- **Status:** Accepted (2026-07-09).

## ADR-0011 — Rhai engine stored as a Bevy `NonSend` resource; rhai `sync` OFF
- **Context:** the `scripting` bridge holds a Rhai `Engine`/`AST`/`Scope`, which are `Rc`/`RefCell`-based
  and NOT `Send + Sync` unless rhai's `sync` feature is enabled. A normal Bevy `Resource` requires
  `Send + Sync + 'static`. Two options: enable `rhai/sync` (`Arc`/`RwLock`) + `#[derive(Resource)]`, or store
  the engine as a **NonSend resource** and keep `sync` off.
- **Decision:** store the `ScriptEngine` via `world.insert_non_send(engine)` (accessed with `NonSendMut`),
  rhai `sync` **OFF**. rhai is pinned `default-features = false, features = ["std"]` — drops
  `ahash/runtime-rng` (needs entropy on wasm → fixed keys, WASM-safe); `unchecked`/`internals` stay off.
- **Consequences:** faster per-tick interpreter (no atomic refcounts/locks in a tree-walking interpreter
  already ~2× slower than Python); matches the single-threaded-WASM-at-launch invariant (ADR-0003);
  sidesteps the Bevy 0.19 `Resource`-implies-`Component` collision (the wrapper is a plain non-derived
  struct). Cost: the script system is pinned to the main thread even under a future `multi_threaded`
  schedule — acceptable while scripts stay thin; flipping to `sync` + `#[derive(Resource)]` later is a
  localized change (only the insert call + trait bounds move).
- **Status:** Accepted (2026-07-09).

## ADR-0012 — matchbox 0.14 two-channel transport; embedded full-mesh signaling; nibli note resolved
- **Context:** the transport item prescribes matchbox with two DataChannels (unreliable state + reliable
  events) and a room-based signaling server. Verified live: matchbox_socket/matchbox_signaling **0.14.0**
  (TODO's 0.13 was stale). The TODO's prior-art note pointing at `github.com/dhilipsiva/nibli` as a reusable
  "browser-native WebRTC P2P gossip transport" is **obsolete** — that repo has been repurposed into a Lojban
  theorem prover; there is no transport code to reuse.
- **Decision:** `crates/transport` wraps `matchbox_socket` with the settled channel layout baked in
  (`CHANNEL_STATE`=0 unreliable/unordered `{ordered:false, max_retransmits:Some(0)}`; `CHANNEL_EVENTS`=1
  reliable/ordered `{ordered:true, max_retransmits:None}`; channel index = builder insertion order).
  `crates/services` embeds `matchbox_signaling`'s full-mesh topology as the signaling binary (rooms = URL
  path). **`?next=N` matchmaking is NOT in the library's FullMesh topology** (it lives in the separate
  `matchbox_server` binary's custom topology) — it lands with Phase 5's custom `SignalingTopology` extension.
  matchbox's transport `PeerId` (UUID, signaling-assigned) is distinct from `protocol::PeerId(u64)`; the
  mapping is a session-layer concern deferred to replication/session join.
- **Consequences / findings:**
  - The native↔native two-peer datachannel test (novel — matchbox itself only tests signaling) proves both
    channels flow through real WebRTC + real signaling, hermetically (empty ICE config → loopback host
    candidates; **native-only**: browsers reject an ICE-server entry with no URLs, so `connect_hermetic` is
    cfg-gated off wasm).
  - **matchbox 0.14 wasm handshake waits for ICE-gathering-complete before sending its offer** (non-trickle;
    upstream TODO). Under WSL2 headless Chrome, gathering never reaches 'complete' when any iceServers entry
    is set (reproduced: shell + full chromium, all flag combos) ⇒ the browser two-tab acceptance cannot run
    headlessly in THIS environment; it passes wherever gathering completes (desktop browser / non-WSL CI).
    `scripts/e2e-two-tab.mjs` encodes the automated check + the findings.
  - tokio enters the tree (signaling + native webrtc); the client wasm demo gained `console_error_panic_hook`
    + `console_log` (Rust panics and matchbox internals visible in the browser console — essential for wasm
    debugging).
- **Status:** Accepted (2026-07-10). **Browser-tab residual CLOSED (2026-07-11):** two tabs of a
  desktop-class Chromium on the Windows host (WSL2 `networkingMode=mirrored`, so localhost is shared;
  signaling + static server in WSL2) each logged the other peer `Connected` and received
  `[uniblox-demo][STATE] state-hello` + `[uniblox-demo][EVENT] event-hello` — two real browser tabs P2P
  over WebRTC, data on both channels (webgpu build). The WSL2-HEADLESS gathering limitation above still
  holds for `scripts/e2e-two-tab.mjs` (needs a non-WSL host).

## ADR-0013 — The custom replication protocol: wire format + authority-gated sender/receiver
- **Context:** the Phase-1 slice's HIGH-risk core — per-entity, authority-gated state replication over the
  two-channel transport, with no existing crate to adopt (settled). Designed adversarially (out-of-order
  delivery, cross-channel and cross-sender races, entity-identity aliasing, echo-back); full 26-test battery
  user-specified and committed before implementation; fresh netcode-auditor on the diff.
- **Decision:**
  - **Wire format (postcard, `crates/protocol`):** versioned messages (postcard is not self-describing);
    `NetEntityId{spawner, index, generation}` is a **spawner-stable identity** minted once — identity ≠
    authority (current authority lives only in the proxy's `Owner`, mutated only by reliable
    `OwnershipTransfer` events). Quantized fixed-point `QVec2` (scale 1024; tolerance ≤1/2048 for |v|≤16384;
    saturating). `StateEntry` uses **Options-only presence** (mask is derived — cannot disagree with payload)
    and **ABSOLUTE values, never arithmetic deltas** (lossy channel; Phase 3's acked baselines own those).
    `EventMsg.sig` is reserved (always None) for Phase-6 signing. `PeerId::from_uuid_bytes` = first 8 bytes
    BE of the transport UUID — a pure function so all peers agree; interim until Phase-5 session join.
  - **Sender (`crates/replication`):** ONE cached `SystemState` for change detection (a fresh
    `Changed<T>` query in a manually-driven World anchors to `last_change_tick` and reports everything
    changed forever); authority gate (`authority_of`) strictly precedes any `is_changed` consult and no
    `Changed` filter exists in the crate (echo-back structurally impossible); ids resolved via the
    bidirectional map, minting reachable only for self-spawned entities (adopted ones are mapped at Spawn
    receipt — no namespace aliasing); keyframe (full masks) every 30 collects as the interim
    stale-forever guard; per-message size warn above ~1150B (SCTP fragmentation loss amplification;
    splitting is Phase 3). Same-tick transfer-then-despawn purges the queued corpse events and announces a
    valid Despawn instead (auditor F1 — prevents an unhealable owned ghost).
  - **Receiver:** whole-message newest-seq-wins LWW (unordered channel; `last_seq` advances even when all
    entries drop); full-`NetEntityId` map keying makes stale-generation/post-despawn/pre-spawn state inert
    with no tombstones; **ownership validity: sender must be the CURRENT owner** (the only sound arbiter for
    handoff races — per-sender seq streams are incomparable); `authority_of == Remote` apply-gate;
    **snap-apply** (interpolation buffers are Phase 3; smoothing is the render boundary's job).
  - **Handoff:** initiator flips local `Owner` the same tick it queues the reliable Transfer ⇒ no
    double-authority window (≤½-RTT nobody-simulates freeze is the safe direction).
- **Accepted gaps (documented + warn-logged; healed by Phase-3 anti-entropy resync — do NOT fix ad hoc):**
  cross-sender event reordering after handoffs (Despawn-before-Spawn orphan; chained A→B→C transfers can
  leave a fourth peer with a frozen wrong-owner proxy); late-join replay excludes entities the spawner no
  longer owns; no peer-departure cleanup yet (`last_seq`/proxy maps grow; departed peers' proxies freeze —
  Phase 3's owner-drop reassignment + session lifecycle own this). Bevy-0.19 note: a long-lived
  `SystemState` outside schedules is not tick-clamped — recreate periodically on the Mode-3 server.
- **Status:** Accepted (2026-07-10).

## ADR-0014 — The authority-swap gate: PASSED
- **Context:** the architecture go/no-go gate — demonstrate that the SAME simulation yields Mode 2 (P2P) and
  Mode 3 (authoritative server) by changing ONLY authority assignment, with no logic fork. Failure would have
  stopped everything downstream (services, billing, publish all assume the swap works).
- **Decision / evidence — the documented side-by-side run (all green, netcode-audited):**
  - **M1** `crates/replication/tests/mode_proof.rs`: ONE parameterized harness (`run_session(ids, spawns)`);
    the mode-2 and mode-3 tests differ ONLY in the spawn/ownership DATA. Mode 2: both peers compute their own
    entity, converge cross-wise. Mode 3: the server computes all; both clients emit ZERO state messages and
    ZERO events for the entire session and converge to the server's truth.
  - **M2** `crates/replication/tests/e2e_mode3_star.rs`: the same over REAL WebRTC + signaling (server + two
    clients); clients' send counters end at zero.
  - **M3/M4** `crates/server/tests/headless_app.rs`: the real headless App converges an external client,
    FixedUpdate self-regulates to ~64 Hz (tick-counter evidence), and state sends track the ~20 Hz network
    interval, decoupled from the fixed tick.
  - Auditor verdict: no hidden mode fork — `authority_of` has exactly one gameplay call site plus
    replication's documented gates; the server crate adds zero authority branches; no `Mode` type exists in
    the workspace.
- **The Mode-3 runtime (`crates/server`):** standalone `bevy_app` + `bevy_time` assembly (`TaskPoolPlugin` +
  `TimePlugin` + `ScheduleRunnerPlugin::run_loop(1/64 s)`) — the `MinimalPlugins` equivalent without the
  `bevy` umbrella (`MinimalPlugins` lives in `bevy_internal`). `TimePlugin` is mandatory (FixedUpdate
  silently never runs without it). **0.19 renamed buffered Events→Messages**: exit = write `AppExit` to
  `Messages<AppExit>` (never `EventWriter`). `SimDt` is fed from the fixed clock by a boundary adapter
  (`sync_sim_dt`, chained before `simulate`) — engine-core stays free of bevy_app/bevy_time. Networking is a
  NonSend `Net` bundle pumped by an exclusive Update system: receive every frame, collect+send on a
  virtual-clock accumulator at `NET_INTERVAL` (50 ms ≈ 20 Hz, an ASSUMED value — the Instrumentation item
  measures it). `Time<Virtual>` is max_delta-clamped (250 ms): stalls drop sends rather than bursting.
- **Test-robustness findings baked in (auditor):** convergence predicates must not depend on proxy REPLAY
  ORDER (HashMap iteration is arbitrary — all demo/test entities advance on x; a zero-x-velocity entity in
  slot 0 hung the M2 predicate on ~1/6 runs before the fix); e2e deadlines are 120 s (they bound hangs, not
  CPU contention); rate-measurement windows are 2 s (max_delta permanently drops ticks on >250 ms stalls).
- **Status:** Accepted — GATE PASSED (2026-07-10).

## ADR-0015 — str0m native/server WebRTC peer: matchbox-interoperable, thread-per-peer sans-IO
- **Context:** Phase 2 needs native/server WebRTC without a browser stack. The settled stack picks sans-IO
  **str0m** for native (lock-free, no tokio requirement; drives Mode 3's authoritative hub). It must
  interoperate with matchbox peers (browser wasm + native webrtc-rs) — same rooms, same channels, same wire.
- **Decision:** `transport::Str0mPeer` (native-only module; the wasm build gains nothing — verified dep
  tree) mirroring `Transport`'s method surface. **str0m 0.21, `default-features = false,
  features = ["rust-crypto"]`** — pure-Rust DTLS (no aws-lc-sys/cmake/libclang; Nix-friendly); the crypto
  provider is installed process-wide once (`str0m::crypto::from_feature_flags().install_process_default()`,
  idempotent OnceLock).
- **The matchbox wire contract implemented** (verified from vendored 0.14 sources, not docs):
  - Signaling: WS text JSON, externally tagged — in `IdAssigned`/`NewPeer`/`PeerLeft`/`Signal{sender,data}`,
    out `Signal{receiver,data}` + bare `"KeepAlive"` (~10 s). Wire types come from a **`matchbox_protocol`
    dep** (compat by construction); `PeerSignal` (not exported by matchbox) is re-declared shape-identical:
    `Offer`/`Answer` carry RAW SDP strings, `IceCandidate` carries DOUBLE-ENCODED `RTCIceCandidateInit`
    JSON; the browser `"null"` end-of-candidates sentinel is tolerated.
  - **Negotiated channels — NO DCEP** (the key constraint): both sides pre-create stream id 0
    (`matchbox_socket_0`, unreliable/unordered `MaxRetransmits{0}`) and id 1 (`matchbox_socket_1`,
    reliable/ordered) — never reorder. Offerer declares via `sdp_api().add_channel_with_config`; answerer
    via `direct_api().create_data_channel` after `accept_offer`. Connected = ALL channels open (matchbox's
    criterion).
  - Roles both ways: existing peers get `NewPeer` and OFFER (str0m = DTLS server); newcomers answer
    unsolicited offers (str0m = DTLS client). Only an `Offer` from an unknown sender spawns a connection —
    stray non-offer signals (racing `PeerLeft`) are dropped, not answered.
  - **Candidate trickle ORDER matters:** native matchbox DROPS candidates arriving before the offer/answer
    it is waiting on (its handshake loops ignore out-of-phase signals) — so our host candidate is trickled
    AFTER the Offer (offerer) / AFTER the Answer (answerer). Caught by the reviewer: a pre-offer trickle
    "worked" in tests only via peer-reflexive discovery, masking a dead trickle path on real networks.
- **The driving loop (the human-reviewed design):** one blocking-tungstenite signaling thread (read-timeout
  loop that also drains an outbound signal queue + keepalive) and ONE CONNECTION THREAD PER REMOTE PEER,
  each owning a UDP socket + `Rtc`. str0m's hard invariant — after EVERY mutation, drain `poll_output()` to
  `Output::Timeout` before the next mutation — is honored structurally: every command application
  `continue`s back into the drain, and `handle_input` sits at the loop bottom feeding the loop-top drain.
  Socket wait = rtc deadline clamped to [1 ms, 10 ms] (no busy-loop; the command queue stays responsive).
  The API side is non-blocking mpsc drains, mirroring `Transport`'s poll model. Sends before channel-open
  are dropped WITH a warn — callers gate on `PeerState::Connected`, as with matchbox.
- **Evidence** (`crates/transport/tests/str0m_interop.rs`, locked first, TDD): str0m↔native-matchbox in BOTH
  role directions + str0m↔str0m (our encode → our decode), each exchanging distinct payloads on BOTH
  channels BOTH ways through an in-process signaling server; soaked 4/4. The reviewer (fresh session)
  traced the drain invariant per mutation site and verified the wasm dep tree gained nothing.
- **Scope/residuals:** `ws://` signaling only (no TLS yet); loopback UDP bind (real binds + STUN/TURN are
  later Phase-2 items); no reconnect/ICE-restart (separate item).
- **Status:** Accepted (2026-07-10). **BROWSER pairing VERIFIED (2026-07-11) — and it caught a real bug.**
  A runnable peer (`crates/transport/examples/str0m_browser_demo.rs`) joined the `uniblox-demo` room with
  the wasm demo in a desktop-class Chromium on the Windows host (WSL2 `networkingMode=mirrored` — str0m's
  `127.0.0.1` host candidate is reachable from the browser; str0m learns the browser via peer-reflexive
  discovery, so it needs nothing from the browser's mDNS candidates). Proven BOTH role directions
  (str0m offers / str0m answers), all four: browser `[STATE]`+`[EVENT]` from str0m, str0m `[STATE]`+`[EVENT]`
  from the browser, no console errors.
  - **The bug (invisible to the hermetic tests):** `encode_candidate` hardcoded `sdpMid:"0"`, but str0m
    generates a RANDOM m-line mid (captured live: `a=mid:SrN`). Chrome validates the candidate's `sdpMid`
    against the actual m-lines, rejects the mismatch (`OperationError`), and matchbox-wasm `.unwrap()`s that
    into a PANIC that breaks the browser peer's message loop (it connected only intermittently, via the
    SDP-embedded candidate). **webrtc-rs is LENIENT** (ignores the wrong mid, uses `sdpMLineIndex`), so the
    str0m↔native-matchbox hermetic tests passed while the browser crashed — the exact "browsers are stricter
    than the native impl you tested against" gap this verification exists to find.
  - **The fix:** identify the m-line by INDEX only — `sdpMid: None, sdpMLineIndex: Some(0)`. The offer/answer
    always has exactly ONE BUNDLE'd data m-line at index 0 (DataChannels-only; both channels share one SCTP
    m-line), so index-0 is unambiguous for both roles, and both Chrome and webrtc-rs accept it. Hermetic
    interop + TURN suites stay green; fresh reviewer verdict MERGE.
  - WSL2-HEADLESS gathering limitation (ADR-0012) still holds for automated CI; this run used the desktop
    Browser pane.

## ADR-0016 — ICE policy: STUN-only free tier, coturn TURN as the Mode-3 paid feature
- **Context:** the buildspec's connectivity economics — STUN-only P2P fails for an estimated ~15–25% of
  peers (symmetric NAT / restrictive firewalls); relaying costs real bandwidth money. The settled stance:
  free modes accept silent STUN-only failure; Mode 3 (paid) provides TURN.
- **Decision:** `transport::IceConfig` encodes the tier as data:
  - `IceConfig::stun_only()` — the free default (matchbox's default public-STUN servers; what plain
    `Transport::connect` already used); no credentials, no relay.
  - `IceConfig::with_turn(urls, username, credential)` — Mode 3: TURN alongside STUN, with **paid-only,
    PER-SESSION credentials** provisioned at session join by the platform's entitlement boundary
    (Phase 6). The transport only CARRIES credentials — long-lived TURN secrets never ship in a client.
  - `Transport::connect_with_ice(room, ice)` maps it onto matchbox's `RtcIceServerConfig` (wasm + native).
    The Mode-3 str0m SERVER needs no TURN client — it runs on publicly reachable addresses (that is part
    of what the paid tier buys); TURN serves clients behind hostile NATs.
- **Evidence** (`crates/transport/tests/turn_relay.rs`, hermetic against a flake-provided coturn 4.13):
  - **The relay proof**: two RAW webrtc-rs peers under `ice_transport_policy = Relay` (host/srflx
    excluded ⇒ data can ONLY flow through the TURN allocation) connect a DataChannel and exchange a
    payload with valid credentials — "TURN relay works with credentials" end to end. (matchbox does not
    expose relay-only, so the proof runs at the webrtc-rs layer matchbox native is built on.)
  - **Negative**: wrong credentials ⇒ BOTH sides gather ZERO relay candidates and the channel never
    opens (bounded window).
  - **Pass-through**: two `Transport::connect_with_ice` peers configured with ONLY the coturn url +
    credentials (a TURN server answers STUN binding requests too) connect and exchange on both channels —
    the IceConfig plumbing through matchbox is live.
- **Hermetic-coturn gotchas (recorded for reuse):** coturn **rejects loopback PEER addresses** by default
  (CVE-2020-26262 hardening) — the tests pass `--allow-loopback-peers`, which must NEVER be set in
  production; TCP-connect readiness is NOT UDP readiness — a lost first Allocate makes relay-only
  gathering complete with zero candidates, so the harness probes with a real STUN Binding request over
  UDP until answered.
- **Residuals:** the **STUN-only failure RATE** is a real-network fleet metric (already a recorded
  measurement gap) — it cannot be measured hermetically; production coturn deployment + per-session
  credential minting are Phase-9 bullets (gated on the Phase-6 entitlement boundary they authenticate
  against); str0m-side srflx/TURN gathering (if native CLIENTS ever sit behind NATs) rides the
  non-loopback-bind residual.
- **Status:** Accepted (2026-07-10).

## ADR-0017 — The Bevy client renders (wasm-only); real sizes; size-budget gate PASSED
- **Context:** every remaining Phase-1 measurement (cold-load TTI, meaningful two-build sizes,
  feature-prune deltas) was gated on a rendering Bevy client. This lands the minimal one and takes the
  measurements.
- **Decision — Bevy 0.19 as a `wasm32`-ONLY dependency of `client`:** native Bevy/winit would drag
  alsa/udev/X11 system libs into the Nix devShell and every native test/clippy run; nothing before
  Phase 14 (native parity) needs native rendering. Native `client` main stays the stub. Feature set:
  `default-features = false, features = ["2d", "bevy_winit", "webgl2"]`; the crate's `webgpu` cargo
  feature forwards to `bevy/webgpu` for the second build (webgpu OVERRIDES webgl2 — the two-build split,
  ADR-0002). Minimal scene: `Camera2d` + one asset-free bouncing sprite into canvas `#uniblox-canvas`
  (`fit_canvas_to_parent`), a run-once `[uniblox-metrics] first-frame` marker; the transport demo +
  metrics harness start before `app.run()` (which never returns on wasm). wasm-bindgen stayed at the
  pinned =0.2.121 — no CLI lockstep move needed.
- **Two pipeline bugs found live and fixed in `build-wasm.sh`:**
  1. **The page served the UNOPTIMIZED wasm**: `client.js` fetches `client_bg.wasm`, but the pipeline
     wrote the optimized artifact to `client_bg.opt.wasm` — the optimized file was never loaded. Fixed:
     the optimized artifact takes the `client_bg.wasm` name.
  2. **`wasm-opt -all` emitted encodings stable browsers REJECT** (`invalid heap type 'exact'` — an
     experimental custom-descriptors proposal): the optimized artifact had been unloadable all along,
     masked by bug 1. Fixed: enumerate exactly the BASELINE features rustc emits
     (bulk-memory(+opt), sign-ext, mutable-globals, nontrapping-fptoint, reference-types, multivalue) —
     wasm-bindgen strips `target_features`, so auto-detection cannot work. This also un-broke twiggy.
- **Measured (2026-07-11; local server; headless chromium for webgl2, SwiftShader):**
  - Sizes per build: raw ~21 MB → bindgen ~18.3 MB → wasm-opt ~15.6 MB → **brotli 3.38 (webgl2) /
    3.40 (webgpu) MB**; the two builds now genuinely differ.
  - **Feature-prune delta** (default features vs `2d` prune, webgl2): brotli **5.16 → 3.38 MB (−34%)**,
    wasm-opt 18.7 → 15.6 MB. `--converge` delta: −35 KB (~0.27%) vs plain `-Oz`.
  - **Cold-load** (optimized artifact, localhost): wasm instantiate **351 ms**, **first Bevy frame
    381 ms** from navigation start. Computed download at link speed: ~2.7 s @10 Mbps / ~5.4 s @5 Mbps.
  - **Size-budget gate: PASS** — 3.38/3.40 ≤ ~8 MB brotli; first frame ≈ 3.1 s @10 Mbps (inside the
    2–5 s target), ≈ 5.8 s @5 Mbps (marginal — prune further when real assets land). Re-check per
    release (the gate is standing, TODO §Gates).
  - Two-tab `[STATE]`/`[EVENT]` receipts re-verified with Bevy in-binary; ed25519 numbers unchanged.
- **Environment gotchas (recorded):** Bevy's derive macros scan `[dependencies]` for the facade and
  emit `bevy_ecs::` paths when the dep is target-scoped — `use bevy::ecs as bevy_ecs;` fixes it.
  winit's web loop runs on requestAnimationFrame, which browsers SUSPEND for hidden tabs — the app
  pauses when not visible (transport keeps running on setTimeout); headless chromium fires rAF, so
  the render metrics run headless (playwright chromium in `~/.cache/ms-playwright`).
- **Status:** Accepted (2026-07-11). PHASE 1 IS COMPLETE — the only untaken slice measurement
  (STUN-only success rate) is real-network-gated and lives in Phase 2's telemetry bullet.

## ADR-0018 — str0m connection telemetry: the STUN-only-failure-rate + RTT/jitter instrument
- **Context:** the "measure the STUN-only failure rate + real-network RTT/jitter" item. The NUMBERS are
  hard-blocked by the environment (they need peers behind diverse real NATs; on a single host every
  connection succeeds via host candidates → 0% failure, ~0.6 ms RTT). Approved scope: build the telemetry
  INSTRUMENT the acceptance refers to ("telemetry reports … once real sessions run") so a deployed fleet
  auto-produces the numbers. Native/str0m side; browser `getStats()` is a follow-up.
- **Decision:** `Str0mPeer` records per-peer `PeerTelemetry { outcome (Connecting/Connected/Failed),
  time_to_connect, local_candidate (Host/ServerReflexive/PeerReflexive/Relayed/Unknown),
  selected_local/remote_addr, rtt_samples, rtt_mean, rtt_jitter }`, exposed via
  `Str0mPeer::telemetry() -> Vec<(PeerId, PeerTelemetry)>`. A fleet aggregates: **STUN-only success
  fraction = Connected / attempted**; **RTT/jitter distributions** from the per-peer values.
- **How it's sourced from str0m (verified in `str0m-0.21.0/src/stats.rs`):**
  - Stats are OFF by default → build the `Rtc` via
    `RtcConfig::new().set_stats_interval(Some(500ms)).build(start)` so str0m emits `Event::PeerStats`.
  - **RTT comes from `selected_candidate_pair.current_round_trip_time`, NOT `PeerStats.rtt`.** The latter
    is RTP/media-derived and stays `None` for a DataChannels-only session (found live: the hermetic test
    hung 120 s waiting on `stats.rtt`); the candidate-pair RTT is the ICE keepalive/consent RTT and is
    "available even on receive-only endpoints." This is the load-bearing detail.
  - `CandidateStats` exposes only the `addr`, not the kind — so the winning LOCAL candidate is classified
    by matching `selected.local.addr` against the candidates we added (each has `Candidate::kind()`).
    **`Host` today** — `Str0mPeer` gathers only a host candidate; srflx/relay await the non-loopback-bind
    / STUN-gathering residual, at which point the classification lights up with no code change.
  - `Failed` is finalized when the per-peer thread exits without ever reaching `Connected` (deadline / ICE
    disconnect / dead remote); a peer that connected then dropped STAYS `Connected` (it did connect — a
    disconnect is not a STUN failure).
- **Drain invariant preserved:** handling `Event::PeerStats` is READ-ONLY (fold RTT, classify, update a
  Mutex) — not an `Rtc` mutation, so it sits inside the existing drain with no change to the
  drain-to-Timeout discipline.
- **Evidence:** hermetic str0m↔str0m test (`tests/str0m_interop.rs`) — both peers record `Connected`,
  time-to-connect, `Host` candidate, selected addrs, and an RTT sample with mean+jitter (soaked 4/4);
  three unit tests on the `Failed`/`Connected`-stays-`Connected` finalize transitions. Live str0m↔str0m:
  `[TELEMETRY] outcome=Connected local=Host rtt=0.6ms jitter=0.2ms samples=19` (the ICE RTT is tighter
  than the app-ping's ~4 ms, which is poll-bounded). Fresh reviewer on the diff.
- **Residuals:** real-network NUMBERS need a deployed fleet (unchanged gate); browser-side candidate-pair
  classification via `getStats()` is a follow-up (matchbox-wasm doesn't surface it); srflx/relay local
  classification lights up with the deferred str0m gathering work. The telemetry map is retained per-peer
  (intentional — a fleet wants the historical outcome record); a long-lived Mode-3 hub accumulates one
  small `PeerTelemetry` per distinct remote, so bounded retention / snapshot-and-drain is a
  pre-Mode-3-production follow-up (reviewer NIT).
- **Status:** Accepted (2026-07-11).
