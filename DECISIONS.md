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
- **Accepted gaps (documented + warn-logged):** cross-sender event reordering after handoffs (Despawn-before-
  Spawn orphan; chained A→B→C transfers can leave a fourth peer with a frozen wrong-owner proxy) — **now HEALED
  by ADR-0024 anti-entropy resync (do NOT fix ad hoc — resync corrects it)**; late-join replay excludes
  entities the spawner no longer owns; no peer-departure cleanup yet (`last_seq`/proxy maps grow; departed
  peers' proxies freeze — Phase 3's owner-drop reassignment + session lifecycle own this). Bevy-0.19 note: a
  long-lived `SystemState` outside schedules is not tick-clamped — recreate periodically on the Mode-3 server.
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
  `Str0mPeer::telemetry() -> Vec<(PeerId, PeerTelemetry)>`. **`FleetMetrics::aggregate(&[PeerTelemetry])`**
  turns many records into the numbers: **STUN-only success fraction = Connected / (Connected + Failed)**
  (Connecting excluded), the winning-candidate-kind breakdown (host/srflx/prflx/relay/unknown, the
  TURN-needed signal), and the **RTT/jitter distribution** (min / mean / p50 / p95 / max over per-peer
  mean RTTs, nearest-rank percentiles; mean per-peer jitter). Pure function — no network to compute or
  test; only the DATA needs real peers.
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
  three unit tests on the `Failed`/`Connected`-stays-`Connected` finalize transitions; **six unit tests on
  `FleetMetrics::aggregate`** (empty, success-fraction-excludes-Connecting, kind breakdown, RTT
  distribution over 1..=10 ms, all-failed → 0.0, percentile edges). Live str0m↔str0m:
  `[TELEMETRY] outcome=Connected local=Host rtt=0.6ms jitter=0.2ms samples=19` (the ICE RTT is tighter
  than the app-ping's ~4 ms, which is poll-bounded). Fresh reviewer on the per-peer diff (MERGE).
- **Residuals:** real-network NUMBERS need a deployed fleet (unchanged gate); browser-side candidate-pair
  classification via `getStats()` is a follow-up (matchbox-wasm doesn't surface it); srflx/relay local
  classification lights up with the deferred str0m gathering work. The telemetry map is retained per-peer
  (intentional — a fleet wants the historical outcome record); a long-lived Mode-3 hub accumulates one
  small `PeerTelemetry` per distinct remote, so bounded retention / snapshot-and-drain is a
  pre-Mode-3-production follow-up (reviewer NIT).
- **Status:** Accepted (2026-07-11).

## ADR-0019 — Reconnect / ICE-restart handling (Str0mPeer resilience)
- **Context:** `Str0mPeer` treated any loss as fatal — the first transient ICE `Disconnected` killed the
  connection, and any signaling WS drop `close_all`'d every live connection and never reconnected. Both are
  wrong: WebRTC `Disconnected` is documented transient/self-recovering, and WebRTC data paths are
  independent of signaling. Approved scope: the full mechanism.
- **Decision — five pieces (all native `str0m_peer.rs`):**
  1. **Transient tolerance + auto ICE restart.** `run_connection` tracks a `disconnected_since` episode
     instead of returning `Err`; returning to `Connected`/`Completed` clears it and counts a recovery. As
     the OFFERER, after a self-heal grace (`ICE_RESTART_GRACE` 2s) it initiates an in-place
     `sdp_api().ice_restart(true)` + `apply()` + re-offer — DTLS/SCTP and the channels SURVIVE (no data
     gap). A hard deadline (`DISCONNECT_HARD_DEADLINE` 10s) gives up → the full-reconnect fallback.
  2. **Re-offer channel guard.** The answerer's `apply_signal(Offer)` creates channels only on the FIRST
     offer (`chan_ids[0].is_none()`); a re-offer (ICE restart) reuses them (`accept_offer` handles the
     creds change) — recreating would break them.
  3. **Explicit `Str0mPeer::request_ice_restart(peer)`** → `Cmd::IceRestart` (ops recovery hook + the
     mechanism's hermetic test trigger); guarded to a connected peer so it can't clobber the initial offer.
  4. **Signaling reconnect + no-kill-on-blip.** `run_signaling` is an outer reconnect loop around
     `connect_and_serve` (→ `SignalingExit::{Closed,Dropped}`); a WS drop NO LONGER `close_all`s (live
     connections survive) — it backs off (500ms→x2→5s) and reconnects. The outbound queue lives across
     reconnects so per-peer threads' re-offers flush on the new WS.
  5. **Full-reconnect fallback.** On a hard `Err`, ONLY the offerer re-establishes (glare avoidance — the
     answerer recovers via the offerer's re-offer through the unknown-sender-Offer path), only while the
     peer is still in `Shared.present`, bounded by `MAX_RECONNECT_ATTEMPTS` (5) with exponential backoff.
  Telemetry (ADR-0018 extension): `PeerTelemetry` gains `reconnects` + `ice_restarts`; `FleetMetrics`
  sums them.
- **Glare rule:** only the offerer auto-restarts and re-establishes; role is fixed at spawn, so a pair can
  never both offer. The answerer tolerates `Disconnected` and heals via the re-offer.
- **str0m <-> BROWSER falls back to full reconnect** — matchbox-wasm is offer-once and won't accept a
  mid-session re-offer for an in-place ICE restart; str0m <-> str0m (server/native) gets the in-place restart.
- **Testability (honest):** real packet-loss recovery can't be simulated hermetically (`run_connection`
  owns a real `UdpSocket`). The restart MECHANISM is proven by `request_ice_restart` +
  `ice_restart_keeps_channels_and_counts` (both channels still exchange after; `ice_restarts` counted); the
  no-kill-on-blip fix by `connection_survives_signaling_drop` (kill the signaling server → data still flows,
  `poll_peers` stays open); the decision logic (`should_initiate_restart`, `reconnect_backoff`) by unit
  tests. The AUTOMATIC trigger (sustained-Disconnected → restart) shares the same code path behind a timer,
  validated by the decision unit test, not under simulated loss.
- **Fresh-reviewer verdict MERGE:** drain invariant preserved at both new mutation sites (`continue` after
  every `ice_restart`/`apply`), no busy-loop (`restart_sent` gates one restart/episode; socket wait floored),
  reconnect bounded (≤5, present-gated, no `pow` overflow), finalize completeness (only the reconnect path
  defers Failed-finalize; terminal attempts always finalize), no `unwrap`/`expect`/`unsafe`. Reviewer
  SHOULD-FIX folded in: reset `restart_sent` at each new episode so an ops restart can't suppress the next
  auto-restart.
- **Residuals (reviewer NITs, documented):** per-peer `reconnects`/`ice_restarts` counters reset on a FULL
  reconnect (a fresh `run_connection`), so `FleetMetrics` totals undercount full-reconnect recoveries
  (metrics fidelity only); a signaling reconnect changes our matchbox id (matchbox couples signaling id
  with peer identity), so a re-offer that must traverse signaling AFTER an id-changing reconnect is
  best-effort — the common case (connectivity blip, signaling UP) is fully handled.
- **Status:** Accepted (2026-07-11).

## ADR-0020 — Delta compression vs last-acked baseline; per-peer ack tracking
- **Context:** first Phase-3 replication-depth item. The slice sent changed-components + a full KEYFRAME
  (every owned entity's pos+vel) every 30 collects; the keyframe existed ONLY because the lossy state
  channel can lose a final value, so a stationary-then-quiet entity would freeze on a receiver that missed
  its last update. Replace the fixed keyframe with an ACKED-BASELINE delta: re-send a component only until
  every tracked peer has confirmed (acked) its current value, then go quiet. Approved model (via
  AskUserQuestion): **cumulative-ack + contiguous-run**; **drop the fixed keyframe**. HIGH-risk (netcode
  "compiles but subtly wrong"): plan-mode-first, TDD, fresh `netcode-auditor` (twice — see below).
- **Decision — the algorithm (contiguous-run cumulative-ack):**
  1. **Ack wire (receiver → sender, reliable channel).** New `NetEvent::Ack { seq }` = "I have applied up to
     state `seq` of YOUR stream." Reuses the `EventMsg` codec + apply routing (no new channel). Ephemeral
     bookkeeping — `sig` always `None`. **`WIRE_VERSION` → 2** (pre-release hard cutover; v1 peers cleanly
     reject v2).
  2. **Sender confirmation state (`Replication`).** `acked_seq[peer]` (highest seq of OUR stream a peer
     acked), a `peers` broadcast set, and per owned-entity per-component `CompSend { value, run_start,
     last_sent }` (the value we are confirming, the seq its current contiguous send-run began, and the last
     seq it was included in).
  3. **`decide_component` (pure).** At the provisional `seq = next_seq`: never-sent or value-changed ⇒
     include, fresh run at `seq`. Unchanged ⇒ `confirmed = peers.is_empty() || peers.all(acked[p] >=
     run_start)`; confirmed ⇒ skip; else re-send, resetting `run_start = seq` when `last_sent + 1 != seq`
     (a gap since the last inclusion). `seq` = SENT-message seq (only bumped when a message actually goes
     out), so contiguity in seq == contiguity across sent messages.
  4. **decide/commit split (the seq-consumption invariant).** `collect` decides all entities at the
     provisional seq WITHOUT mutating; only if ≥1 component is included is the message sent — THEN the
     `CompSend` updates commit and `next_seq += 1`. An empty tick consumes no seq and mutates no baseline.
  5. **The send trigger is a QUANTIZED-VALUE diff** against the committed baseline, NOT `Ref::is_changed()`
     — retiring the long-lived-server `check_change_ticks` hazard.
- **Soundness (the crux):** while a component is unconfirmed it is in EVERY sent message of its contiguous
  run `[run_start, last_sent]`, so a peer that acks any `seq >= run_start` demonstrably received a message
  carrying the current value ⇒ holds it. A value change resets `run_start` to the new value's first seq, so
  an ack of an OLD value can't confirm a NEW one. The gap-reset keeps the run contiguous when a component
  resumes after being all-confirmed (e.g. a new peer joins) — without it the run would span seqs the
  component was absent from, and an intermediate ack would falsely confirm.
- **The keyframe's job is now continuous:** a lost final value is re-sent every tick until acked; a new peer
  (acked nothing) has everything unconfirmed ⇒ gets a full targeted re-send. `on_peer_connected` tracks the
  peer (so `collect` MUST run with a populated peer set — empty peers degrades to plain changed-only, a
  documented `collect` precondition); `untrack_peer` prunes all per-peer state; `send_state` is pruned on
  despawn and transfer-away.
- **Ack basis = APPLIED, not SEEN (the auditor F1 fix, load-bearing).** The receiver keeps TWO per-sender
  high-waters: `last_seq` (LWW "seen" — drops reordered older messages, advances unconditionally) and
  `applied_seq` (advances only when a message's entries ALL applied — no dropped entry). `drain_acks` acks
  `applied_seq`. Without this split, a state message that RACED AHEAD of its reliable `Spawn` (entry dropped,
  no proxy yet) — or a handoff owner-mismatch — would still advance `last_seq`, get acked, and falsely
  confirm a value the receiver never held; with the keyframe gone that divergence is PERMANENT. Withholding
  the ack until a fully-applied message keeps the sender re-sending (bounded, for state-before-spawn, by the
  reliable Spawn's arrival). The ack is whole-message per-sender (a single unresolvable entry withholds the
  whole stream's ack → bandwidth-only over-send; per-entry acks are a future optimization, not a correctness
  need).
- **Audit (mandatory, HIGH-risk):** the FIRST `netcode-auditor` pass returned **FIX-FIRST** with F1 as a
  BLOCKER (the seen-vs-applied ack conflation above) plus should-fixes (untested gap-reset, no
  state-before-spawn ack test, empty-peers footgun, bandwidth-test honesty, an ADR mislabel). All addressed:
  the `applied_seq`/`last_seq` split; two new deterministic tests — `state_before_spawn_defers_ack` (T37,
  asserts a dropped-entry message is NOT acked; fails against the pre-fix code) and
  `gap_reset_keeps_run_contiguous` (T36, forces the `else { seq }` reset via a quiet entity while a second
  moving entity advances the stream, and distinguishes `run_start=reset` from `=1` with an injected stale
  ack); an honest T35 (measures the full-first-send keyframe-equivalent cost, asserts ZERO steady-state
  bytes); a `collect` empty-peers precondition doc. The SECOND (re-audit) pass returned **MERGE** — F1
  verifiably closed (confirmation invariant + liveness both hold), the new tests genuine guards, no
  regressions.
- **Evidence:** `two_world.rs` 28 deterministic tests green (T29–T37 the delta battery); full workspace
  suite green; clippy `-D warnings` native `--all-targets` + wasm32 (protocol/replication); fmt clean. T35
  proves the bandwidth win deterministically (a confirmed stationary scene sends 0 steady-state bytes vs the
  keyframe's full re-send every 30 ticks).
- **Residuals / fast-follows (auditor, non-blocking):** ~~the server's ack-ROUTING wiring is exercised only by
  the `two_world` `flush_acks` helper — dead in Mode 3 and untested in integration.~~ **CLOSED (2026-07-12)** by
  `crates/server/tests/headless_app.rs::ack_round_trip_confirms_and_goes_quiet` — a real-transport headless test
  that drives BOTH ack directions over the live `net_pump`: the client acks the server's stationary entity (the
  server's per-peer delta baseline confirms ⇒ goes quiet) AND the client OWNS a stationary entity it replicates
  to the server (the server's `net_pump` `drain_acks`→`send_event` routing confirms the client's baseline ⇒ goes
  quiet — the previously Mode-3-dead surface, now driven by a Mode-2-shaped client-owned entity). The test
  Client gained the missing client-side ack/collect pump wiring; both plateau assertions FAIL if either
  `drain_acks` send is removed (verified by disabling it: recv_delta = 40 vs the ≤2 threshold). netcode-audited
  → MERGE (non-vacuity proven via the confirmation-causality chain: a value can go quiet only after state
  flowed AND was acked). A real production client pump must adopt the same wiring the test Client demonstrates.
  Deeper desync (a peer that never sees a Spawn / a frozen wrong-owner proxy) remains owned by the separate
  anti-entropy-resync item.
- **Status:** Accepted (2026-07-11).

## ADR-0021 — Interest management (AOI, spatial grid); the sender goes PER-PEER
- **Context:** second Phase-3 replication-depth item. The ADR-0020 sender broadcast ONE `collect(world) ->
  Outbox` identically to every peer. Interest management makes replication **per-peer and visibility-gated**:
  each peer replicates only entities within its **area of interest** (a circle), computed by a **spatial
  grid**. Out-of-range entities are NOT replicated at all — neither per-tick state NOR existence — the
  structural **Mode-3 read-cheat defense** (a modified client can't read entities it never receives).
- **Decisions (user, via AskUserQuestion):** (1) **UNIFY** — per-peer collect REPLACES broadcast; the delta
  baseline becomes per-(peer,entity) with a per-peer seq stream. (2) **State + existence** — gate BOTH the
  state stream and lifecycle (spawn-on-AOI-enter / despawn-on-AOI-exit). The RECEIVER is UNCHANGED — each
  receiver still sees one continuous per-sender stream regardless of which entities enter/leave, so the
  audited ADR-0020 F1 receive-side soundness is preserved; only the SENDER generalizes.
- **The design:**
  - New `interest` module: `SpatialGrid` (cell-bucketed, `DEFAULT_CELL`=16, rebuilt each tick from owned+alive
    entities — remote proxies never enter it) + `Aoi{center,radius}`. `in_radius` scans the circle's cell
    bbox then EXACT-dist² filters (boundary inclusive `<=`); cells by `.floor()` (not `as i32` — truncation
    mis-cells negatives). A peer with NO `Aoi` set is UNBOUNDED (sees all owned) — a bandwidth default, and
    (auditor NIT) FAIL-OPEN: not a security guarantee; a read-cheat pump MUST `set_aoi` for every peer.
  - `collect_all(world) -> Vec<(PeerId, Outbox)>` replaces `collect`. Per-peer state: `send_state:
    HashMap<PeerId, HashMap<Entity, EntitySend>>`, `next_seq: HashMap<PeerId,u64>`, `known: HashMap<PeerId,
    HashSet<Entity>>` (each peer's proxy set), `aoi: HashMap<PeerId, Aoi>`, `pending_transfers:
    HashMap<Entity,PeerId>`. `decide_component` simplifies to a single `acked: u64` (`confirmed = acked >=
    run_start`).
  - **Per-peer order is load-bearing: dead → transfer → exit → enter → state.** DEAD wins over a pending
    transfer (dead is removed from `pending_transfers` first) so a corpse is never handed off (owned-ghost
    guard). AOI-EXIT drops `send_state[P][E]` so a re-enter re-baselines at a fresh seq (a climbing
    `acked_seq` can't false-confirm a re-entered entity). AOI-ENTER emits a Spawn only for entities in OUR
    namespace (`spawner==local`); an ADOPTED entity is stated to peers that already hold its proxy (no Spawn —
    we can't mint in another namespace). The id-map is pruned + `pending_transfers` cleared only AFTER the
    peer loop, so a two-peer despawn reaches both.
  - **Transfer:** a peer that KNOWS the entity gets a bare `OwnershipTransfer` (proxy kept under the new
    owner). A never-witnessed NEW OWNER gets Spawn+Transfer, but the Spawn only if `spawner==local`; the bare
    Transfer is ALWAYS emitted (harmless-if-no-proxy / load-bearing-if-witnessed), so a chained handoff
    O→A→q where q witnessed e via O completes even though A can't mint in O's namespace.
  - **Deterministic wire output:** every emitted collection (dead/transfer/exit/enter/state entries) is sorted
    by `NetEntityId` (which gained `Ord`) and peers by `PeerId`, so the bytes don't depend on HashSet/HashMap
    seed — reproducible captures, stable tests. (Found via a mode_proof M3 flake: the client's proxy-index
    pairing assumed Spawn order == server order, which HashSet iteration broke.)
  - `untrack_peer` clears ALL per-peer maps (`known`/`send_state`/`next_seq`/`aoi` + the receiver-side ones):
    a same-id peer reconnecting with a fresh world must NOT be seen as already-`known` (that would suppress
    Spawns forever) — and no leak. `on_peer_connected(peer)` is now just `track_peer` (no blanket Spawn
    replay — existence is gated; AOI-enter announces on the next collect). `transfer_ownership` records the
    intent, mints if uncollected, flips local Owner immediately (no double-authority window), drops
    `send_state[*][E]`.
- **Server pump + harnesses:** switched from one broadcast to per-peer routing (`for (target,out) in
  collect_all` → map target→transport peer). The server leaves AOI unset (Mode-3 clients see all; a per-client
  gameplay focus is future client work). e2e/mode_proof/slice_metrics/headless harnesses migrated with zero
  lost assertions.
- **Audit (mandatory, HIGH-risk; design also pre-validated by an independent Plan agent whose lifecycle-
  ordering + leak findings were folded into the plan):** the FIRST `netcode-auditor` pass returned **FIX-
  FIRST** — F1: the new-owner-notify branch emitted a foreign-namespace Spawn for adopted entities (receiver
  rejects it → silent orphan) + two missing tests (corpse-guard dangerous branch; adopted handoff). Fixed;
  the re-audit found my fix OVER-BROAD (it also dropped the load-bearing bare Transfer, orphaning a
  witnessing-q chained handoff — a NEW regression). Corrected to emit the Transfer unconditionally; a third
  pass returned **MERGE**. Everything else the auditor verified sound first time: exit/re-enter re-baseline,
  dead-over-transfer ordering, per-peer seq/ack independence, map-prune timing, determinism, `untrack`
  clearing, read-cheat completeness, and the grid math.
- **Evidence:** `two_world.rs` 46 deterministic tests green (Groups A–H: AOI gate, enter/exit/re-enter incl.
  the white-box `reenter_stale_ack_does_not_confirm`, per-peer independence, read-cheat existence-withholding,
  transfer/dead under AOI, the corpse-guard + both chained-handoff cases) + 5 `interest::SpatialGrid` unit
  tests; full workspace green; mode_proof M3 deterministic 8/8; clippy `-D warnings` native `--all-targets` +
  wasm32 (protocol/replication); fmt clean.
- **Documented accepted gaps:** boundary flicker (an entity oscillating across the AOI edge → Spawn/Despawn
  churn — correct; hysteresis is a later phase); `known[P]` is mutated before the caller confirms the reliable
  send (a dropped Outbox desyncs it — bounded by Phase-3 resync); the adopted-entity enter of a peer without
  the proxy re-sends every tick until resync (per-peer/bandwidth-only, documented); a chained handoff to a
  NEVER-witnessed new owner of an adopted entity is orphaned until resync (`in_radius` cost is O(bbox area) —
  no clamp, low-risk since AOI is server-controlled).
- **Status:** Accepted (2026-07-12).

## ADR-0022 — Prediction / reconciliation / interpolation buffers (predict-own, interpolate-others)
- **Context:** third Phase-3 replication-depth item. The receiver SNAP-applied remote `Position` (no
  smoothing) and owned entities had no input. This builds the client-prediction netcode stack:
  interpolate-others (remotes render ~100 ms behind, lerping buffered snapshots), predict-own (the
  locally-controlled avatar is simulated from local input immediately), and server reconciliation (the
  authority's snapshot re-anchors the client's prediction, which replays un-acked inputs). User chose the FULL
  input-prediction scope (AskUserQuestion). Landed in three audited stages (A→B→C) — the full stack is too
  large for one HIGH-risk diff (ADR-0020/0021 set the precedent).
- **The load-bearing idea — role = (authority × control), render is separate.** Two orthogonal axes plus a
  render-only output that never collide: **authority** (`Owner`/`authority_of` — unchanged: who computes
  authoritative `Position` and may put state on the wire; the `collect_all` gate is UNTOUCHED); **control**
  (a `Controlled` marker — which entity THIS instance drives with input; in Mode 3 the client's avatar is
  `Controlled` AND `authority==Remote`); **render** (a separate `RenderPos` component — the ONLY thing
  interpolation/prediction write). Prediction NEVER writes authoritative `Position`/`Velocity`, so a
  predicted avatar (`Remote`) is structurally excluded from `collect_all` (client emits inputs only, never
  state) — no new gate. Render role is derived: Local⇒copy Position; Remote+not-controlled⇒interpolate;
  Remote+controlled⇒predict.
- **Settled-invariant REFINEMENT (recorded, not worked around).** The literal invariant is "receivers never
  re-simulate others' entities; prediction only touches entities you own — so no two machines must agree on
  a float" (`docs/CONTEXT.md §28`). Mode-3 client prediction re-simulates the avatar the SERVER owns. The
  refinement: **prediction re-simulates ONLY the locally-CONTROLLED avatar, and re-anchors to the
  authoritative snapshot every message**, so cross-machine divergence is bounded by the un-acked-input window
  and self-corrects each snapshot — it is NOT a determinism *correctness requirement* (lockstep, which makes
  determinism a correctness requirement, stays rejected). Authoritative `Position` is written ONLY by
  `simulate`/`apply_state`; render output is the separate `RenderPos`. (The prediction/reconciliation half
  lands in Stage B.)
- **Stage A — interpolate-others (this commit).** New engine-core: `RenderPos` (render output — the only
  thing the render path writes; `spawn_owned` attaches it, seeded to the spawn pos), `Snapshot{tick,x,y}`,
  `InterpBuffer(VecDeque<Snapshot>)` (capped ring; its PRESENCE marks an entity interpolated), `RenderTick`
  (interp clock in sim-tick units; app-advanced, tests set it) + `Tick` (authoritative sim tick, advanced by
  `advance_tick`), `INTERP_DELAY_TICKS=6.4` (~100 ms @ 64 Hz = 2 net ticks); systems `interpolate` (lerp the
  buffer at `RenderTick − DELAY`, CLAMP out of range — NEVER extrapolate), `copy_owned_render` (Local ⇒
  `RenderPos=Position`; scheduled AFTER `interpolate` so a Local entity with a still-attached buffer wins),
  `push_snapshot` (cap-evicting + tick-monotonic — drops an out-of-order/duplicate tick). Wire: `StateMsg`
  gains `tick` (interp time axis — uniform, loss-immune, deterministic; not arrival time or the delta-warped
  `seq`) and `last_input` (reserved 0 until Stage B); `WIRE_VERSION 2→3` (one bump for both fields). The
  RECEIVER's snap-apply of authoritative `Position` is UNCHANGED — `apply_state` additionally pushes a
  snapshot (the post-apply Position at `msg.tick`) into the proxy's `InterpBuffer` (a pure side-record; a
  stationary entity goes delta-quiet ⇒ no snapshot ⇒ interpolation holds). The Spawn handler attaches an
  `InterpBuffer` to each new proxy; the server chains `advance_tick`.
- **Evidence (Stage A):** two_world 54 tests green (SA1–SA7 + the 3-snapshot interior-lerp: exact lerp,
  render-at-delay, underrun/overrun clamp with NO extrapolation, `Position` bit-untouched by interp, tick
  stamped, owned-render tracks Position) + protocol wire round-trips the new fields; full workspace green;
  clippy `-D warnings` native `--all-targets` + wasm32 (protocol/replication/engine-core); fmt clean.
  netcode-audited → **MERGE** (auditor NITs folded: the render-system order made explicit + documented, the
  push-snapshot tick-monotonicity guard, the 3-snapshot test). Documented gaps for later stages: a Mode-2
  sender must advance `Tick` to actually smooth (else tick=0 ⇒ clamp-to-latest); an entity adopted to/from
  Local keeps/lacks a buffer until the Stage-C role reset (RenderPos can go stale) — closed in Stage C.
- **Stage B — predict-own + input + server reconciliation.** New engine-core: `Intent{vx,vy}`,
  `Input{seq,intent}`, `Controlled{next_seq}` (CLIENT: I drive it + mint its input seqs), `ControlledBy(peer)`
  (AUTHORITY: peer P drives it — Stage B assumes ONE controlled entity per peer, the Mode-3 avatar model),
  `InputHistory(VecDeque<Input>)` (client, capped), `PendingInputs`/`ProcessedInput` resources (server).
  Systems: `predict` (client — `RenderPos = Position anchor + replay(InputHistory)`, one `intent*dt` per entry,
  recomputed from the anchor each tick so it never accumulates float error and NEVER writes authoritative
  `Position`/`Velocity`); `apply_input` (server, FixedUpdate BEFORE simulate — pops ONE fresh input per
  controlled entity, `Velocity=intent`, `ProcessedInput[peer]=seq`; skips `seq<=last` without consuming a
  tick; **ZEROS Velocity on underrun** so the server matches the client's replay — a held velocity would drift
  past it and pop, auditor F3). `record_input` stores the intent quantized→dequantized so the replay matches
  the server's applied Velocity bit-for-bit (auditor F1). Wire: `NetEvent::Input{seq,intent}` on the RELIABLE
  channel (each processed once, in order — `last_input` advances contiguously; a gap over-prunes with no
  recovery, so inputs MUST stay reliable, auditor F5). replication: `apply_events` Input arm (server queues
  into `PendingInputs[ControlledBy(from) entity]`); `drain_inputs` (client sends un-sent history entries,
  directed to the avatar's `Owner`); `apply_state` prunes `InputHistory` by `msg.last_input`; `collect_all`
  stamps `StateMsg.last_input` per-peer from `ProcessedInput`. server: chains `apply_input`; clears
  `ProcessedInput[peer]` on disconnect so a reconnecting fresh input-seq namespace isn't frozen (auditor F4).
  **The authority gate needs NO change** — a predicted avatar is `Remote`, so `collect_all` structurally never
  emits its state/Spawn (the client sends inputs only).
- **Reconciliation converges without float determinism:** `RenderPos` is re-pinned to the authoritative
  `Position` every snapshot and only extrapolates the un-acked `(last_input, next_seq]` window; the marker is
  monotonic (LWW drops stale snapshots) and prune is `seq<=last_input`, exactly aligned to "Position reflects
  inputs through last_input" — so a CORRECT prediction reconciles with no pop, and a WRONG one snaps to server
  truth + replays. Bit-exactness never required (the invariant refinement).
- **Evidence (Stage B):** two_world 64 tests green — SB1–SB10: no input lag, prediction leads by the un-acked
  window, prediction writes only RenderPos (Position+Velocity bit-unchanged), reconcile snaps+replays+converges,
  no pop on a correct prediction, a duplicate skipped once (marker monotonic), a stale LWW-dropped marker
  doesn't un-prune, the client never emits its predicted avatar's state, **reconcile corrects a WRONG
  prediction** (the headline), **underrun stops the server** (no drift) — plus the `NetEvent::Input` wire
  round-trip; full workspace green; clippy `-D warnings` native `--all-targets` + wasm32; fmt clean.
  netcode-audited → FIX-FIRST then MERGE (folded: F1 quantize-at-record, F3 zero-on-underrun + test, F4
  reconnect-marker reset, F6 wrong-prediction test; F2 one-avatar-per-peer scope + F5 reliable-channel
  dependence documented). Gap: `input_sent` isn't pruned on avatar despawn (slow leak — cleanup pass).
- **Stage C — handoff interplay.** New engine-core: a cached `RenderRole` (Owned/Interpolated/Predicted,
  derived from authority × `Controlled`) + `reset_render_role` (EXCLUSIVE system, first in the render step) —
  diffs the desired role vs the cached one and, on a TRANSITION, runs the flush/seed: → Owned drops
  `InterpBuffer` + clears `InputHistory` (now authoritative — stale inputs must NOT replay against the new
  anchor); → Predicted drops `InterpBuffer` + ensures `InputHistory`; → Interpolated drops `InputHistory` +
  ensures an `InterpBuffer` (kept if already present). It re-seeds `RenderPos` from the AUTHORITATIVE Position
  (belt-and-braces — the same-frame `copy_owned_render`/`predict`/`interpolate` overwrite it in their roles;
  the load-bearing effect is the component add/remove). replication: `apply_events` `OwnershipTransfer` flushes
  the proxy's `InterpBuffer` on ANY authority change (its snapshots came from the OLD owner — don't lerp across
  the A→B source discontinuity); `drain_inputs` now filters to `authority == Remote` so a self-owned
  controlled avatar can't self-direct inputs (auditor N3). This closes the Phase-1-flagged adoption bug
  (buffer-flush + prediction-seed on adoption).
- **Evidence (Stage C):** two_world 69 tests green — SC1 (adopt renders at the authoritative Position, buffer
  dropped), SC2 (relinquish non-controlled → Interpolated, buffer attached), SC3 (relinquish keeping control →
  Predicted, history ensured, no buffer), SC4 (observer flushes the buffer on an A→B owner change), SC5 (adopt
  a PREDICTED avatar → InputHistory cleared, transition fires exactly once — the unmasked assertion); full
  workspace green; clippy `-D warnings` native `--all-targets` + wasm32; fmt clean. netcode-audited → MERGE
  (auditor: no correctness bug; the seed block is defensive/over-guarded and docstrings + SC5 were tightened to
  reflect that `copy_owned_render` co-guarantees the adopt render). Documented gaps: an A→B observer's
  `RenderPos` freezes at the flushed mid-lerp value until B's snapshots arrive / resync (R6-class); `input_sent`
  / `PendingInputs` aren't pruned on avatar despawn (session-lifecycle follow-up).
- **Status:** Stages A + B + C ACCEPTED (2026-07-12) — the item is complete. Deferred fast-follows: a Mode-2
  sender advancing `Tick`; per-entity input markers for multi-avatar-per-peer; despawn-on-disconnect avatar
  cleanup; the actual in-browser render wiring (the separate Bevy client gameplay build).

## ADR-0023 — Interest-management follow-ups (snapshot hoist, hysteresis, per-client focus)
- **Context:** three ADR-0021 refinements, landed as audited stages a→b→c (TDD + a FRESH `netcode-auditor`
  each): (a) a shared per-tick snapshot so `collect_all` quantizes ONCE per owned entity, not per
  (peer,entity); (b) AOI-flicker hysteresis (two radii) so an entity oscillating across the boundary doesn't
  churn Spawn/Despawn; (c) a real per-client AOI focus (Mode 3 leaves AOI unset ⇒ clients see all).
- **Stage a — quantization hoist (Accepted 2026-07-12):** the grid + world query were already built once/tick;
  the residual per-(peer,entity) redundancy was quantization. The once-built `owned` snapshot now carries a
  precomputed `OwnedRow { id, qpos: QVec2, qvel: QVec2 }` (quantized once from the peer-invariant raw pos/vel);
  the per-peer loop READS `row.qpos`/`row.qvel` at enter-Spawn, `decide_component`, and the `StateEntry`
  entries. Raw pos/vel dropped from `owned` (the grid builds from `rows`; nothing else read them). The
  transfer-Spawn path is untouched (a transferred entity's Owner is flipped ⇒ it fails the authority gate ⇒
  it's never in `owned`; that path reads `world.get` directly). A PURE byte-identical refactor: no
  wire/receiver change; quantization is pure over a shared snapshot. Evidence: two_world 70 green (T35
  byte-exact 0 steady-state, A/B/C exact positions, SA/SB interp — the real regression proof) + a new
  `hoist_quantized_value_is_peer_invariant` (identical `StateEntry` across two peers, values pinned to the
  spawn coords); full workspace green; clippy `-D warnings` native + wasm32; fmt clean. netcode-audited →
  MERGE (byte-identity + invariants intact: authority-gate-first, no `Changed`, deterministic `NetEntityId`
  order, single per-tick quantization; the only delta is a release-invisible earlier `debug_assert` on a
  documented sim bug — arguably an improvement).
- **Stage b — AOI-flicker hysteresis (Accepted 2026-07-12):** the `Aoi` circle gained a two-radius band
  (`radius_inner`, `radius_outer`) — an entity ENTERS at `dist ≤ r_inner` and EXITS only at `dist > r_outer`;
  in the band a known entity stays and an unknown one is withheld, so one oscillating across the boundary no
  longer churns Spawn/Despawn. `collect_all` derives `visible_outer` (EXIT `!visible_outer` + STATE
  `visible_known` + the unbounded arm) and `visible_inner` (ENTER only) from two `in_radius` scans; the
  per-peer order (dead→transfer→exit→enter→state) is untouched. The band read-cheat holds: an entity in the
  band never inside `r_inner` is fully withheld (enter uses `visible_inner ∩ !known`, state uses `known ∩
  visible_outer`). `set_aoi(peer,center,radius)` is the degenerate band `inner==outer==radius` (the
  pre-hysteresis single boundary — keeps Groups A–H green); new `set_aoi_hysteresis(peer,center,r_inner,
  r_outer)` (`debug_assert r_inner≤r_outer`, and a release fail-safe `r_outer = r_outer.max(r_inner)` so an
  inverted band degrades to the safe single radius, not per-tick churn — auditor F1). Evidence: two_world 74
  green (new B6 no-churn-in-band, B7 baseline-survives-band-dip, B8 enter-at-inner/exit-at-outer + the band
  read-cheat, B9 degenerate==single; A–H the backward-compat guardrail); full workspace green; clippy `-D
  warnings` native + wasm32; fmt clean. netcode-audited → MERGE (wiring correct, band read-cheat sealed,
  baseline survival holds; auditor F1 actioned, F2/F3 test-strength gaps closed — B8 now asserts state
  continues for a known band entity, B7 asserts no Despawn on the dip).
- **Stage c — per-client AOI focus / avatar hook (Accepted 2026-07-12):** Mode 3 left every client's AOI unset
  (fail-open ⇒ sees all). Now an OPT-IN focused server (`build_server_app_focused`, `Net.focus_radius`) gives
  each connecting client a server-OWNED avatar it CONTROLS (`spawn_owned` + `ControlledBy(peer)` — Owner=server
  keeps Mode 3 authoritative; ControlledBy is the input/focus link, the audited ADR-0022 predicted-avatar
  model) at a distinct lane (`focus_radius * 4 * lane`, so foci are disjoint), and focuses that client's AOI on
  the avatar each net tick via `set_aoi_hysteresis(peer, avatar_pos, r, r*1.25)`. Borrow order: gather
  `(ControlledBy, Position)` (world read) → `set_aoi_*` (repl mutate) → `collect_all`. Disconnect despawns the
  avatar (via the `ControlledBy(peer)` scan) AND prunes its `PendingInputs` entry — that map was NEVER pruned
  anywhere (a per-reconnect leak the moment avatars exist; the old disconnect comment was aspirational —
  auditor). `build_server_app(_,_,N)` delegates unfocused (`None`) so M3/M4 are byte-unchanged. No echo/re-sim:
  the avatar is server-authoritative, the client emits inputs only (authority gate). Evidence: headless_app 5
  green over the real pump (`focused_server_withholds_out_of_focus_entities` — a client sees only its avatar +
  a near entity, an x=1e6 entity NEVER leaks in state OR existence; `two_focused_clients_see_disjoint_sets` —
  disjoint non-empty focus sets, neither sees the far entity; M3/M4 unchanged), 3× flake-clean; full workspace
  green; clippy `-D warnings` native; fmt clean. netcode-audited → MERGE (PendingInputs prune correct + keyed
  by avatar entity, no echo, borrow order sound, tests non-vacuous, M3/M4 intact; auditor F2 non-empty guard
  added).
- **Status:** Stages a + b + c ACCEPTED (2026-07-12) — the interest-management follow-ups item is complete.
  Deferred: a `slice_metrics` focused-bandwidth NUMBER (Instrumentation item — the win is already proven
  correct by A2/D1); the shared-per-tick snapshot's remaining perf (two `in_radius` scans + an unbounded-peer
  clone); AOI-focus for a REAL controllable client (this exercises the server hook with a stub client — a
  moving avatar's focus already follows it).

## ADR-0024 — Handoff depth + anti-entropy resync (heals the R6 cross-sender reordering gap)
- **Context:** the "handoff depth" item needed deterministic coverage for hand-back (A→B→A), repeated/cycle
  transfers, and handoff under packet loss (all worked already — just untested), PLUS the chained-transfer
  cross-sender **reordering** gap (R6): in A→B→C an observer D that receives T2(B→C) before T1(A→B) drops T2
  (`owner!=from`), then T1 flips D to owner B, leaving D FROZEN at B while the real owner is C — C's state
  rejected forever (F1 withholds C's whole ack stream). The ADRs reserved R6 for anti-entropy resync (no
  ad-hoc fix); resync was entirely unbuilt. Per the user's decision, this item BUILDS resync so R6 heals,
  absorbing most of the separate resync TODO item.
- **Stage 1 — positive-depth tests (Accepted 2026-07-12, tests-only):** Group R in `two_world.rs` — hand-back
  (A applies the new owner's state to its OWN original entity, then re-adopts via the persistent id-map and
  re-baselines full-mask), repeated/cycle (identity stable on the wire across every hop; a white-box guard for
  the previously-UNTESTED transfer-away re-baseline drop), packet-loss (a dropped/reordered state packet heals
  via the fresh owner's delta; stale old-owner state dropped by the owner gate — verified discriminating), and
  `r6_chained_reorder_freezes_observer_at_wrong_owner` which PINS the R6 gap. No production change; audited →
  MERGE (fixed a vacuous-loss-test blocker: the fresh-owner value coincided with the stale one).
- **Stage 2 — anti-entropy resync (Accepted 2026-07-12):** a **digest → request → ResyncSpawn** protocol, all
  on the RELIABLE channel (`WIRE_VERSION` 3→4). Divergence detection: the owner periodically sends a per-peer
  `NetEvent::Digest` (a `{id, state_hash: Option<u32>}` list over `known[peer] ∩ owned`; owner IMPLICIT =
  sender; `state_hash = Some(fnv32(qpos,qvel))` ONLY for a confirmed+UNCHANGED value so a moving entity never
  false-triggers, `None` otherwise). The receiver flags each id diverged (missing / `owner!=from` — the R6
  case, caught with no hash / same-owner hash-mismatch — a stale silent value; skips `owner==local`) and pulls
  with a directed `NetEvent::ResyncRequest{ids}`. The owner answers with a privileged
  `NetEvent::ResyncSpawn{id,pos,vel}` per id it STILL owns (authority Local) in the requester's AOI — the
  **responder-owns + AOI re-filter** (no ownership theft, no out-of-AOI leak, self-correcting under a
  concurrent handoff). The receiver's `ResyncSpawn` handler is the **healing primitive**: an own-authority
  guard (never overwrite an entity WE own), then create-or-correct — set `Owner:=from`, snap state, flush the
  `InterpBuffer` (bypassing the `owner!=from` and, for orphans, `spawner!=from` gates; sound because `from` IS
  the current authority in the Mode-2-coordinator / Mode-3-server envelope; identity ≠ authority, the id's
  spawner is unchanged; a CREATE never mints in OUR namespace — auditor F1). No new ack path: after the owner
  flip, C's next normal state passes the gate → `applied_seq` advances → the frozen ack unblocks. Test-drivable
  (`collect_resync`/`drain_resync_requests`/`drain_resync_responses`, no timers). `collect_all` byte-identical.
- **Boundary (honest, NOT healed here):** E4 — a never-witnessed adopted-owner orphan where NO peer holds a
  `Local` proxy for `e` (nobody can digest or answer for it) — needs the coordinator / host-migration item.
  The digest/refetch heals R6 (frozen wrong-owner) + the C-still-owns missing-proxy orphan + a stale silent
  value; it cannot heal E4.
- **SUPERSEDED for R6 by ADR-0025 A (2026-07-12):** the R6 cross-sender reorder is now resolved AT THE SOURCE by
  the per-entity `OwnerSeq` gate (the reorder lands on the true owner immediately — no freeze), so the resync's
  R6-freeze-heal role is retired. Resync's residual role is the STALE-SILENT-VALUE heal (unchanged), a LOST
  (not merely reordered) transfer's wrong-owner proxy (`resync_heals_lost_transfer_wrong_owner`), the orphan
  create/refetch, and E4. The R6 tests were reworked accordingly (`r6_chained_reorder_freezes_observer_at_wrong
  _owner` → `..._resolves_by_seq`; `r6_resync_heals_frozen_observer` → `resync_heals_lost_transfer_wrong_owner`).
- **Evidence:** two_world 86 green (R6-1 pins the gap, R6-2 heals it — one round corrects owner B→C, C's state
  applies, the ack unblocks; R6-3/R6-4 the responder-owns + own-authority adversarial guards, both fail with
  their guard removed; R6-5 the hash-mismatch path); protocol codec round-trip; full workspace green (the
  `WIRE_VERSION` bump breaks nothing); clippy `-D warnings` native + wasm32; fmt clean. netcode-audited →
  MERGE (sound within the trust envelope; the two bypassed gates correctly re-guarded; auditor F1 mint-guard +
  F2 peer-sort + F3 hash-mismatch test + F4 idempotency assertion all actioned).
- **Deviation from the design:** the `Digest` rides the RELIABLE channel (a `NetEvent` variant) rather than a
  new unreliable state-channel message — simpler (no channel disambiguation), the reliability cost is small
  (periodic, compact, on a slow cadence).
- **Stage 3 — production-pump wiring (Accepted 2026-07-12):** `server::net_pump` now DRIVES resync. A free
  `send_directed(transport, msgs)` (the extracted ack-routing loop) sends every directed batch; `drain_resync_
  requests` + `drain_resync_responses` fire EVERY FRAME (prompt, one-shot — rate-limited upstream by the digest
  cadence, next to `drain_acks`); `collect_resync` fires on a SLOW separate accumulator (`Net.resync_acc`,
  `RESYNC_INTERVAL` 500 ms, decoupled from the 50 ms net tick, anti-burst-clamped like `net.acc`). The RECEIVE
  side needed nothing (the `apply_events` Digest/ResyncRequest/ResyncSpawn arms already run in `recv_events`).
  Evidence: `server/tests/headless_app.rs::resync_heals_injected_desync_over_pump` — a stationary server entity
  is confirmed-quiet (delta stream silent + digest carries a hash — the load-bearing settle-to-quiet guard),
  the client's Remote proxy is corrupted, and the digest→request→ResyncSpawn round restores it over real
  hermetic WebRTC; resync is the ONLY heal path (verified: disabling the digest send HANGS the test), 3×
  flake-clean; M3/M4/ack/focused transparent (drains empty when nothing diverges); full workspace green;
  clippy `-D warnings` native (server is native-only) + wasm32 protocol/replication; fmt clean. netcode-audited
  → MERGE (cadence sound, routing byte-faithful, heal non-vacuous; auditor N1 post-settle re-baseline actioned).
  A real production CLIENT pump (future gameplay build) must adopt the same three resync sends the test Client
  now carries. **Deferred:** per-entry ack granularity; E4/coordinator healing.
- **Status:** Stages 1 + 2 + 3 ACCEPTED (2026-07-12) — handoff depth + anti-entropy resync are complete and
  wired into the production pump; the separate anti-entropy-resync TODO item is absorbed (only E4/coordinator
  healing remains, on the double-ownership item).

## ADR-0025 — Ownership-handoff failure modes (host-migration + coordinator-seq arbitration)
- **Context:** two unhandled failure modes — an owner DROP orphaned its entities (frozen at `Owner(departed)`
  on every survivor; `untrack_peer` never touched them), and DOUBLE-OWNERSHIP (conflicting ownership
  assertions — the R6 cross-sender reorder, or a Mode-2 claim) had no arbiter beyond "drop a non-current-owner"
  which merely FROZE. Decided (user): **lowest-peer-ID** election; **full claim/commit/reject** coordinator
  arbitration. Built as deterministic replication-layer primitives (test-drivable, no timers); the full Mode-2
  coordinator *service* is Phase 5 (reuses this rule). A shared pure `elect_owner(candidates) = candidates.min()`
  (`PeerId: Ord` is deliberately for this) serves both the host-migration election and the coordinator identity.
- **Stage B — host-migration reassignment (Accepted 2026-07-12):** `reassign_orphans(world, departed) ->
  Vec<Entity>` — each survivor computes `elected = elect_owner((self.peers \ {departed}) ∪ {local})` (the
  `∪ {local}` is load-bearing: `self.peers` excludes local, so a survivor that is ITSELF the minimum would else
  elect the lowest OTHER peer), scans the LIVE `Owner == departed` predicate (never a snapshot — idempotency),
  and for each sets `Owner:=elected`, `last_owner=elected`, flushes the `InterpBuffer` (old-owner snapshots
  must not lerp across the discontinuity; `reset_render_role` only drops it on →Owned/→Predicted, so a remote
  re-tag would keep them). NO wire event: the election is a pure function of the agreed surviving membership +
  the stable `NetEntityId→Owner` map, so the elected survivor sets `Owner:=local` and INDEPENDENTLY simulates
  (authority is DERIVED, never announced — like `reset_render_role`); others re-tag their proxy (freeze lifts
  when the elected owner's state arrives). Exactly-once = deterministic election ⇒ one Local owner; idempotent.
  **Closes the ADR-0024 E4 orphan:** reassignment gives EXACTLY ONE survivor a Local proxy (what E4 lacked) —
  a witness heals via the elected owner's state; a never-witnessed peer via the existing resync (the elected
  owner's `collect_all` AOI-enters the entity → `collect_resync` Digest → `ResyncSpawn` orphan-create).
  Evidence: two_world 94 green (HM1 the "reassigned exactly once" acceptance, HM4 the buffer flush white-box,
  HM7 all-survivors-agree, HM8 the E4 closure via reassign+resync). netcode-audited → MERGE (sound,
  deterministic, exactly-once, idempotent, single-ownership preserved; the E4 closure genuine; +a "re-tagged
  survivor doesn't simulate" guard). **Carry-forward (Stage A):** exactly-once relies on a CONSISTENT
  membership view — an inconsistent view could let two survivors self-elect; the deferred `net_pump`
  Disconnected wiring must guarantee consistent `track/untrack` or gate adoption on Stage A's ownership seq.
- **Stage A-kernel — the `OwnerSeq` gate (Accepted 2026-07-12):** a per-entity monotonic
  `OwnerSeq { seq: u64, coordinator: PeerId }` (lexicographic `Ord`: `seq` dominant, `coordinator` breaks
  equal-seq ties toward the higher id) is the arbiter for EVERY owner change. `WIRE_VERSION` 4→5; `seq` rides
  `OwnershipTransfer` AND `ResyncSpawn`. `NetIdRecord.owner_seq` is seeded `{0, id.spawner}` (a pure function of
  the id — every peer agrees on the baseline) and advanced only by an accepted change. `transfer_ownership`
  mints `{prev.seq + 1, coordinator: local}` (the current owner holds the system-max rank, so this strictly
  outranks every honest proxy). The **`OwnershipTransfer` apply gate REPLACES the old `owner!=from` check with a
  strict `seq > rec.owner_seq`** — authority is now proven by rank, not sender identity, so a cross-sender
  reordered transfer (lower rank) is dropped WITHOUT freezing: **the ADR-0024 R6 gap is now resolved AT THE
  SOURCE** (no freeze, no resync needed — the reorder lands on the true owner). Gate asymmetry (auditor-verified
  as exactly right): transfer/commit use strict `>` (a fresh mint always outranks); the `ResyncSpawn`
  owner-change heal uses `>=` (a resync re-affirms the CURRENT rank truth — e.g. an elected survivor at the
  rank-preserving migration rank correcting a non-witness — and a strictly-lower stale former-owner is still
  dropped). `ResyncSpawn` is a three-way apply: own-authority guard (owner==local → drop) FIRST; same-owner
  (`from==owner`) value-only heal (snap, no rank change); else owner-change `>=` heal; orphan-create adopts the
  asserted rank. **This closes the resync BACKDOOR** — a stale former-owner `ResyncSpawn` can no longer revert a
  committed owner (its rank is strictly lower). The STATE owner gate (`apply_state`, `owner!=from`) is
  UNCHANGED and independent. **Trust-envelope note (auditor MINOR):** dropping the `from` check on transfers
  widens the modified-client surface — any peer can now *seize* an entity by asserting `observed_max_seq + 1`
  (previously only its owner could give it away). This is within the documented free-tier anti-cheat envelope
  (a modified client is already out of scope; the Rhai sandbox is not anti-cheat); the handshake's
  coordinator-arbitrated commit is the stronger check for the pull path. Evidence: two_world 99 green (Group AK:
  seq-increments-along-chain, highest-rank-wins-on-reorder, equal-rank-dropped-strict-gate, resync-backdoor-
  dropped, value-heal-ignores-lower-rank; reworked R6: `r6_chained_reorder_resolves_by_seq` +
  `resync_heals_lost_transfer_wrong_owner`); protocol codec + `OwnerSeq` Ord pinned; full workspace green at
  WIRE 5. netcode-audited → MERGE (the seq gate is a sound total order along the single-ownership chain; the
  `>=`/`>` asymmetry, the pure seed, the backdoor closure, and single-ownership all verified; +the two
  defensive missing-`Owner` guards it recommended).
- **Stage A-handshake — claim/commit/reject (Accepted 2026-07-12):** the Mode-2 PULL path. WIRE 5→6 adds
  `NetEvent::{ClaimOwnership{id}, OwnershipCommit{id,new_owner,seq}, ClaimRejected{id}}`. `claim_ownership`
  resolves the id, computes the coordinator = `elect_owner(peers ∪ {local})` (lowest live id, reuses the
  host-migration election), and returns `(coordinator, ClaimOwnership bytes)` — flipping NO `Owner` (the
  no-pre-commit-authority guarantee is structural); if WE are the coordinator it records its own claim
  locally (no self-send). The coordinator's `ClaimOwnership` apply guards `coordinator==local` then records
  `pending_claims[id].insert(from)`. `drain_commits` (deterministic, no timers) arbitrates each claimed
  entity: `winner = elect_owner(claimants)` (lowest id), mint `{prev.seq + 1, coordinator: local}`, apply to
  its OWN proxy via the SHARED `apply_ranked_owner_change` (the strict-`>` kernel gate, now used by transfer,
  commit, and this self-apply — a single source of truth), emit `OwnershipCommit` to `claimants ∪ {prior
  owner}` (the prior owner is included so it DEMOTES — else double-authority) plus `ClaimRejected` to the
  losers. The commit apply arm has NO own-authority guard (unlike `ResyncSpawn`): a commit is MEANT to demote
  the current owner, and the strict-`>` rank gate drops stale/duplicate replays — coordinator-migration ties
  resolve toward the higher coordinator. Evidence: two_world 105 green (Group AK-H: two-claims→one-commit+one-
  reject+prior-owner-demotes [the 145/148 acceptance], no-pre-commit-authority, loser-re-claims-and-wins,
  claim-to-non-coordinator-ignored, newer-coordinator-wins-equal-seq-tie, unarbitrable-claim-rejected);
  protocol handshake codec + WIRE 6; full workspace green. netcode-audited → MERGE-with-follow-ups.
  - **Auditor liveness fix (actioned):** a claim the coordinator cannot arbitrate (no longer the coordinator,
    or an entity it does not track — e.g. outside its AOI) is now **rejected**, not silently black-holed, so
    the claimant re-routes/retries (test `unarbitrable_claim_is_rejected_not_blackholed`).
  - **Deferred carry-forwards (auditor MAJOR, documented not fixed — the "cross-owner interaction rules"
    thread):** (1) **push/pull mutual exclusion** — `transfer_ownership` (owner push) and the coordinator pull
    both mint the `OwnerSeq` independently and collide at equal `seq` (the tiebreak favors the higher-id
    minter, so a concurrent push can silently override a granted claim → a transient/again-permanent double-
    authority window). An entity must therefore use ONE mechanism at a time, or Mode 2 must route ALL ownership
    changes through the coordinator (sole minter). (2) **Persistent dual-coordinator split** — exactly-one-
    coordinator relies on a CONSISTENT `peers` view; the equal-seq tiebreak converges only a ONE-SHOT migration
    (AK-H5), NOT a persistent split where two peers each self-elect and oscillate. Both close with the deferred
    `net_pump` Disconnected / membership-consensus wiring (also the Stage-B exactly-once precondition).
- **Status:** Stage B + Stage A-kernel + Stage A-handshake Accepted (2026-07-12). The ADR-0025 item is
  COMPLETE; the cross-owner push/pull-exclusion + membership-consensus hardening is tracked as the deferred
  `net_pump` Disconnected / cross-owner-interaction thread.
