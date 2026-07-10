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
- **Status:** Accepted (2026-07-10).

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
