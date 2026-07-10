# PROJECT_STATE.md

Living snapshot of where uniblox is. Update it when a phase's status changes.
The **why** behind decisions lives in `DECISIONS.md`; the **what/how** lives in
`docs/final-buildspec.md`; the backlog lives in `TODO.md`.

**Current phase: Phase 1 (the vertical slice) — THE AUTHORITY-SWAP GATE PASSED (ADR-0014) and the ownership-handoff item is closed (auditor-verified). Only Instrumentation remains in the slice.**

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
  was resolved obsolete (repo repurposed). Residual: the browser-tab run — blocked in WSL2 headless (ICE
  gathering never completes; matchbox wasm waits on it), verifiable on a desktop browser (see TODO).
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

## Blocked / deferred (prerequisites do not exist yet)
- **Real two-build WASM artifacts + size table** — WASM toolchain is now provided by the flake;
  the remaining blocker is a Bevy client that renders (built later in Phase 1). (Do NOT force artifacts from
  the stub — the two builds are byte-identical and the sizes are meaningless until Bevy is in.)
- **Bevy feature-prune + `wasm-opt --converge` size deltas** — needs Bevy added (later in Phase 1).
- **MCP reachability** (github / read-only postgres / docs / playwright) — `node`/`npx` now provided by
  the flake; still needs a running read-only Postgres role, a GitHub PAT (in `settings.local.json`), and
  Playwright browsers. (`docs` should be reachable on the flake alone.)
- **Web Audio worklet** investigation — needs a running WASM client with audio.

## Next
- **Instrumentation** [LOW]: emit the slice metrics (`/slice-check` table) — the last Phase-1 item
  (meaningful WASM artifacts + Bevy feature-prune remain gated on the Bevy client rendering).

## Toolchain notes
WSL2 Ubuntu. **The toolchain comes from the Nix flake devShell** (ADR-0010): pinned Rust 1.96.1
(edition 2024, wasm32 target) + `wasm-bindgen`/`wasm-opt`/`brotli`/`twiggy`/`node`. Run `direnv allow`
once per clone. Interactive `cd` auto-activates; for the WSL wrapper, prefix cargo/WASM-tool/npx
commands with `direnv exec .`:
`wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && direnv exec . <CMD>"` (compound chains:
`direnv exec . bash -lc '<a && b>'`). Pure git/file commands use the plain wrapper. Ambient rustup
(cargo 1.92) still exists as a fallback for un-routed commands. Hook/build scripts self-activate the
flake and parse event JSON with `/usr/bin/python3` (the rye shim fails non-interactively; `jq` is absent).
No `just` here.
