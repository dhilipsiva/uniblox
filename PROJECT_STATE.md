# PROJECT_STATE.md

Living snapshot of where uniblox is. Update it when a phase's status changes.
The **why** behind decisions lives in `DECISIONS.md`; the **what/how** lives in
`docs/final-buildspec.md`; the backlog lives in `TODO.md`.

**Current phase: Phase 1 (the vertical slice) — scaffolding, the Rhai↔Bevy bridge, and the mode-agnostic mini-game done; next is matchbox two-channel transport.**

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
- **Matchbox two-channel transport** (unreliable state + reliable events; two browser tabs connect P2P;
  evaluate `nibli` before hand-rolling plumbing).
- Then the custom replication protocol and the authority-swap to Mode 3 + one A→B handoff. **The
  replication → authority-swap → handoff items are the architecture go/no-go gate** — do not build services
  until the authority-swap and a clean handoff are proven.

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
