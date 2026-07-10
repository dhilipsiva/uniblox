# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**uniblox** — a browser-first, native-secondary, Rust→WASM platform for user-generated multiplayer games. The one novel, load-bearing idea is the **authority-swap**: a *single* per-entity authoritative state-replication mechanism where **only the authority assignment varies by mode**, so the same authored simulation runs Standalone (Mode 1), P2P Hybrid (Mode 2), and Full-Server (Mode 3) with **no logic fork**.

Engine: **Bevy 0.19** (ECS) on wgpu · UGC logic in sandboxed **Rhai** · transport is **WebRTC DataChannels only** (matchbox in-browser, str0m native/server).

## Current state (read this first)

**THE AUTHORITY-SWAP GATE PASSED (ADR-0014).** Phase 1 scaffolding, the Rhai↔Bevy bridge, the mini-game, transport, the custom replication protocol, and the Mode-3 proof are done. The Cargo workspace exists (9 crates under `crates/*`), with the size-optimized `[profile.release]`, the AI-workflow scaffolding (`.claude/agents/`, `.claude/commands/`, `.claude/settings.json` hooks, per-crate `CLAUDE.md`, `PROJECT_STATE.md`, `DECISIONS.md`), the build-pipeline scripts, and the Nix flake devShell (ADR-0010). Implemented: **`scripting`** (locked-down Rhai engine, ADR-0011, sandbox-audited); **`engine-core`** (`authority_of` — the single decision point) + **`protocol`** (the wire format); **`transport`** (matchbox 0.14 two-channel, ADR-0012) + **`services`** (signaling binary); **`replication`** (ADR-0013 — authority-gated sender, newest-seq LWW receiver, handoff; netcode-audited); **`server`** (the Mode-3 headless runtime, ADR-0014 — standalone bevy_app+bevy_time, 64 Hz FixedUpdate, ~20 Hz net pump; the M1–M4 side-by-side proof shows Mode 2 vs Mode 3 differ ONLY in ownership data). Deps pinned in `[workspace.dependencies]`: rhai 1.25, bevy_ecs/app/time 0.19, matchbox 0.14, postcard/serde, tokio, wasm glue. See `PROJECT_STATE.md` for live status; `DECISIONS.md` for the ADR log. The ownership-handoff item is closed (auditor-verified) and **the slice's native core is COMPLETE**: instrumentation measures bandwidth/peer (742 B/s @ 2 entities, 20 Hz), RTT/jitter, and native ed25519 (13.4/25.7 µs — note the opt-level=3 crypto override in Cargo.toml; the size profile was ~35× slower on verify). Remaining Phase-1 work is client-gated (real WASM sizes, cold-load, browser metrics). Phase 2+ fan-out is unblocked per the staged plan.

Design docs (read before non-trivial work — do not relitigate decisions they mark settled):
- `docs/final-buildspec.md` — **the what/how**: resolved technical verdicts, stack table, risk register, phased build sequence.
- `docs/final-todo.md` — **the build plan**: phase-by-phase backlog with risk tiers and acceptance criteria. Phase 1 (the vertical slice) is built first.
- `docs/CONTEXT.md` — **the why**: rationale, rejected alternatives, the anti-cheat trust envelope, commercial reality. Read before changing any "fixed" decision.
- `docs/*.txt`, `docs/gemini.md` — raw source research reports; **stale in places** (Bevy 0.15, lightyear 0.17). Prefer the two `final-*` docs; the buildspec's "Corrections" section lists what the raw reports got wrong.

## Settled architecture invariants — do NOT "helpfully" break these

Each is load-bearing; the rationale is in `docs/CONTEXT.md §2`. If you think one is wrong, surface it — don't silently work around it.

- **Single-ownership per entity ⇒ no CRDT in the runtime.** No concurrent writes means nothing to merge; last authoritative snapshot wins. (CRDT is permitted *only* in a future collaborative-editing tool, never gameplay sync.)
- **No cross-platform float determinism.** Receivers never re-simulate others' entities — they apply replicated state and interpolate; prediction only touches entities you own. Never introduce a mechanism requiring browser/x86/ARM peers to compute identical floats (this is why lockstep was rejected).
- **WebRTC DataChannels only. No media, no SFU, anywhere.** Social is emoji-only ⇒ no voice/video path. Mode 3 is an authoritative *hub*, not a relay/SFU.
- **Two WASM builds, not one.** Bevy cannot serve WebGPU and WebGL2 from one binary (issue #13168 open; `webgpu` feature overrides `webgl2`). Ship two builds + JS capability detection.
- **Single-threaded WASM at launch.** Do NOT enable SharedArrayBuffer/threads (COOP/COEP) — cross-origin isolation breaks the OAuth sign-in and payment-checkout popups Mode 3 needs.
- **Custom replication protocol, not an off-the-shelf netcode crate.** No existing crate backs all three modes over WebRTC DataChannels by varying only authority (lightyear defers IO to aeronet, which has no WebRTC-DC layer and untested distributed authority; bevy_replicon is server→client-only). Do not adopt lightyear/replicon as the cross-mode backbone.
- **Rhai is a thin custom bridge, NOT `bevy_mod_scripting`** (BMS lacks WASM support, issue #166 — disqualifying for browser-first).
- **The Rhai sandbox is not anti-cheat.** It protects a player's *machine* from malicious *content*; it does nothing against a *modified client*. Orthogonal problems.

## Planned workspace layout (`docs/final-todo.md §4`)

Cargo workspace, multi-crate. HIGH-RISK crates get plan-mode-first, TDD, and a dedicated auditor before merge:
- `protocol` — shared types (versions, messages, content IDs).
- `replication` — the custom protocol: wire format, quantization, delta/baseline, authority, handoff, resync. **HIGH-RISK.**
- `transport` — matchbox (browser) + str0m (native/server) abstraction; two-channel config (unreliable state + reliable events).
- `scripting` — Rhai engine, sandbox limits, ECS bridge. **HIGH-RISK (sandbox).**
- `engine-core` — Bevy setup, shared systems, ECS components.
- `client` — WASM/native client (winit + wgpu).
- `server` — headless authoritative Bevy sim (`MinimalPlugins` + fixed tick).
- `services` — signaling / session-registry / matchmaking WebSocket service.
- `platform` — Postgres / identity / billing / publish / moderation backend.

## Commands

**Dev environment:** the toolchain comes from the **Nix flake devShell** (`flake.nix`, `DECISIONS.md` ADR-0010) — pinned Rust (cargo/rustc/clippy/rustfmt, wasm32 target) + `wasm-bindgen`/`wasm-opt`/`brotli`/`twiggy`/`node`, all pinned by `flake.lock`. Run `direnv allow` once per clone. Interactive `cd` auto-activates the env (direnv + nix-direnv). For the WSL wrapper, **prefix cargo/WASM-tool/npx commands with `direnv exec .`** so they use the flake toolchain; pure git/file commands use the plain wrapper. Ambient rustup is a benign fallback for un-routed commands. The cargo/tool-bearing scripts (`build-wasm.sh`, the `gate-clippy`/`fmt-on-write` hooks, `pre-commit`) self-activate the flake.

Standard cargo across the workspace (run from repo root, inside the flake env):

```bash
direnv exec . cargo build
direnv exec . cargo test                       # single test: ... cargo test <name> -- --exact
direnv exec . cargo clippy --all-targets -- -D warnings
direnv exec . cargo fmt
```

Do **not** run `cargo test --release` — `[profile.release]` sets `panic="abort"`, which the test harness cannot use; plain `cargo test` builds under the dev/test profile (unwind). Toolchain is edition-2024-capable (flake pins Rust ≥ 1.85; currently 1.96).

The **two-WASM-build pipeline** is `scripts/build-wasm.sh` (with `scripts/slice-check.sh`, `scripts/serve.sh`, and the capability-detection page `crates/client/web/index.html`), invoked by the `/build-wasm` and `/slice-check` slash commands. It runs two separate `cargo build --target wasm32-unknown-unknown` invocations — the WebGPU build enables the `webgpu` feature and needs `RUSTFLAGS=--cfg=web_sys_unstable_apis`; the WebGL2 build is the default — each followed by `wasm-bindgen`, then `wasm-opt -Oz --converge` on the *final* file, then brotli. The WASM tools are now present (via the flake), so the pipeline **runs end-to-end** — but on the current stub client the two builds are byte-identical, KB-sized output, **meaningless** for the size budget until the Bevy client renders (later in Phase 1). Do NOT claim stub sizes as the size-budget measurement. The size-optimized release profile is `opt-level="z"`, `lto=true`, `codegen-units=1`, `strip=true`, `panic="abort"`.

**AI workflow:** four subagents (`.claude/agents/`), five slash commands (`.claude/commands/`: `/build-wasm`, `/slice-check`, `/review-netcode`, `/new-crate`, `/write-tests`), and four hooks (`.claude/settings.json` → `scripts/hooks/`: fmt-on-write, destructive-command deny, `tests/`-edit guard, clippy Stop gate) plus the `scripts/git-hooks/pre-commit` clippy+test gate. `/new-crate <name> [--bin]` scaffolds a workspace crate (the `members = ["crates/*"]` glob picks it up automatically).

## Working rules

- **Instrument, don't assume.** Several load-bearing numbers are measurement gaps (WASM size, cold-load, replication bandwidth/peer, in-browser ed25519 cost, STUN-only failure rate). Measure them in the slice; see `docs/final-todo.md §3`.
- No `unwrap()`/`expect()` in non-test code; no new `unsafe` without a `// SAFETY:` comment; never silence a compiler error with a stray `.clone()` or `unsafe`.
- "Compiles but subtly wrong" is the dominant risk in netcode/concurrency — neither the compiler nor clippy catch it. Use TDD and a fresh auditor (never let the session that wrote the code audit itself) for `replication` and `scripting` work.
- The anti-cheat design is sound only inside a specific envelope (low-stakes, casual/co-op, no hidden information, no real-money economy). Free-tier anti-cheat is cost-imposition, not prevention. Don't build features that rely on it as if it were prevention — see `docs/CONTEXT.md §4`.
