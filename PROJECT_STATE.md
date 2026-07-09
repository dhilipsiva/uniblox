# PROJECT_STATE.md

Living snapshot of where uniblox is. Update it when a phase's status changes.
The **why** behind decisions lives in `DECISIONS.md`; the **what/how** lives in
`docs/final-buildspec.md`; the backlog lives in `TODO.md`.

**Current phase: 1.1 — scaffolding (landing).**

## Done
- **Cargo workspace** — virtual manifest, 9 crates under `crates/*` (glob members),
  size-optimized `[profile.release]`. `cargo build` + `cargo test` green (9 smoke tests).
- **Single-threaded stance** — no COOP/COEP anywhere (serve script + capability page + ADR-0003).
- **AI-workflow scaffolding** — per-crate `CLAUDE.md`, `DECISIONS.md`, four subagents
  (`test-writer`, `netcode-auditor`, `sandbox-auditor`, `reviewer`), five slash commands
  (`/build-wasm`, `/slice-check`, `/review-netcode`, `/new-crate`, `/write-tests`),
  four hooks (`.claude/settings.json` + `scripts/hooks/`), git pre-commit gate.
- **Build-pipeline scaffolding** — `scripts/build-wasm.sh`, `scripts/slice-check.sh`,
  `scripts/serve.sh`, `crates/client/web/index.html` (capability detection). Fail
  gracefully until the WASM toolchain + a rendering Bevy client exist.
- **`.mcp.json`** scaffold (github, read-only postgres, docs/Context7, playwright).

## Blocked / deferred (prerequisites do not exist yet)
- **Real two-build WASM artifacts + size table** — needs `wasm-bindgen`, `wasm-opt`,
  `brotli`, `twiggy` (all ABSENT) installed, AND a Bevy client that renders (Phase 1.3–1.6).
- **Bevy feature-prune + `wasm-opt --converge` size deltas** — needs Bevy added (Phase 1.3+).
- **MCP reachability** (github / read-only postgres / docs / playwright) — needs `node`/`npx`
  in WSL (ABSENT), a running read-only Postgres role, a GitHub PAT (in `settings.local.json`),
  and Playwright browsers.
- **Web Audio worklet** investigation — needs a running WASM client with audio.

## Next
- **1.2 Rhai ↔ Bevy ECS bridge** (HIGH-RISK — plan-mode-first, `sandbox-auditor`, adversarial TDD).
- Then **1.3–1.7**: the mode-agnostic mini-game, matchbox two-channel transport, the custom
  replication protocol, and the authority-swap to Mode 3 + one A→B handoff. **1.5–1.7 is the
  architecture go/no-go gate** — do not build services until the authority-swap and a clean
  handoff are proven.

## Toolchain notes
WSL2 Ubuntu; cargo/rustc 1.92 direct on PATH (edition 2024 OK). **No nix dev-shell, no `just`** —
call `cargo` directly. Run everything through:
`wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && <CMD>"`. Hook/build scripts parse
event JSON with `/usr/bin/python3` (the rye shim fails non-interactively; `jq` is absent).
