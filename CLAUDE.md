# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**uniblox** — a browser-first, native-secondary, Rust→WASM platform for user-generated multiplayer games. The one novel, load-bearing idea is the **authority-swap**: a *single* per-entity authoritative state-replication mechanism where **only the authority assignment varies by mode**, so the same authored simulation runs Standalone (Mode 1), P2P Hybrid (Mode 2), and Full-Server (Mode 3) with **no logic fork**.

Engine: **Bevy 0.20** (ECS) on wgpu · UGC logic in sandboxed **Rhai** · transport is **WebRTC DataChannels only** (matchbox in-browser, str0m native/server).

## Current state (read this first)

Live status is in `PROJECT_STATE.md` and the reason behind every design choice is in `DECISIONS.md` (ADR-0001 to ADR-0047); read both before non-trivial work. In short:

- **Done:** Phase 1 (the vertical slice; the authority-swap gate passed, ADR-0014; the Bevy client renders in the browser, ADR-0017), Phase 2 bar the deploy-gated real-network numbers, the Phase-3 replication core (acked-baseline deltas, interest management, prediction and reconciliation, ownership arbitration, host migration, resync; ADR-0020 to ADR-0029), and Phase 4 (the net-free Mode-1 runtime and content-addressed saves, ADR-0030 to ADR-0036).
- **Underway:** Phase 5, central services: scoped signaling, the asymmetric version filter, `?next=N` session grouping and a shared Redis registry are done (ADR-0037 to ADR-0040), and so are join tokens and rate limits (ADR-0047, proposed); the Mode-2 coordinator peer service, atomic registry updates, TTL crash cleanup and autoscaling are not.
- **Scripting (ADR-0045, hardened by ADR-0046, proposed):** content bundles (`protocol::bundle`: manifest + per-asset blake3 ids) drive `Scripted` entities through `engine_core::run_entity_scripts`, owner-only, velocity-only; the sample game `content/sample` runs unchanged in all three modes (`server/tests/scripted_modes.rs`). The browser client links Rhai but loads no content yet.
- **Not built:** `platform` is a stub; the native client is a stub. CI checks the browser paths (first frame, save/reload, two-tab exchange) with `just browser-e2e`.
- **Measured** (2026-10-21, README "Measurements"): ~3.45 MB brotli per WASM build (8 MB gate), ~880–920 B/s per peer at 2 entities and 20 Hz, native ed25519 sign/verify 9.3/18.9 µs (with the opt-level=3 crypto override in `Cargo.toml`).

Design docs (read before non-trivial work — do not relitigate decisions they mark settled):
- `docs/final-buildspec.md` — **the what/how**: resolved technical verdicts, stack table, risk register, phased build sequence.
- `docs/final-todo.md` — **the build plan**: phase-by-phase backlog with risk tiers and acceptance criteria. Phase 1 (the vertical slice) is built first.
- `docs/CONTEXT.md` — **the why**: rationale, rejected alternatives, the anti-cheat trust envelope, commercial reality. Read before changing any "fixed" decision.
- `docs/research/` — the raw source research reports, indexed in its README; **stale in places** (Bevy 0.15, lightyear 0.17). Prefer the two `final-*` docs; the buildspec's "Corrections" section lists what the raw reports got wrong.

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
- `standalone` — the Mode-1 (Standalone) runtime (ADR-0030): local authority over all entities, **net-free by construction** (no transport/replication in its crate graph).
- `persistence` — the Mode-1 content-addressed save (ADR-0033/0034/0035): `save_world`/`load_world` (DTO-mirror + blake3 `ContentId`) + stores — in-memory `MemoryStore`, native durable `FileStore` (`std::fs`), browser durable `IdbStore` (IndexedDB, async).
- `services` — the scoped signaling / session-registry / matchmaking WebSocket service (ADR-0037/0038/0039/0040: a custom `NextTopology` + `{mode, version}` path-scoping + the asymmetric `engine >= min` version filter + `?next=N` session-SIZE grouping + a `RegistryStore` split with a shared `RedisRegistryStore` for horizontal scale).
- `platform` — Postgres / identity / billing / publish / moderation backend.

## Commands

**Dev environment:** the toolchain comes from the **Nix flake devShell** (`flake.nix`, `DECISIONS.md` ADR-0010) — pinned Rust (cargo/rustc/clippy/rustfmt, wasm32 target) + `wasm-bindgen`/`wasm-opt`/`brotli`/`twiggy`/`node`/`coturn` (turnserver for the hermetic TURN tests, ADR-0016) + `redis` (redis-server for the hermetic shared-registry tests, ADR-0040), all pinned by `flake.lock`. Run `direnv allow` once per clone. Interactive `cd` auto-activates the env (direnv + nix-direnv). In non-interactive shells, **prefix cargo/WASM-tool/npx commands with `direnv exec .`** so they use the flake toolchain (or work inside `nix develop`); git and file commands need no prefix. The cargo/tool-bearing scripts (`build-wasm.sh`, the `gate-clippy`/`fmt-on-write` hooks, `pre-commit`) self-activate the flake.

Standard cargo across the workspace (run from repo root, inside the flake env):

```bash
direnv exec . cargo build
direnv exec . cargo test                       # single test: ... cargo test <name> -- --exact
direnv exec . cargo clippy --all-targets -- -D warnings
direnv exec . cargo fmt
```

`just ci` is the gate CI runs (fmt, clippy, every test including the `#[ignore]`d coturn/redis ones, rustdoc with `-D warnings`, `cargo deny`, the Mode-1 net-free check); `just --list` shows the rest.

Do **not** run `cargo test --release` — `[profile.release]` sets `panic="abort"`, which the test harness cannot use; plain `cargo test` builds under the dev/test profile (unwind). The flake pins Rust 1.99.1 exactly, equal to `rust-version` (Bevy 0.20 needs ≥ 1.97.1).

The **two-WASM-build pipeline** is `scripts/build-wasm.sh` (with `scripts/slice-check.sh`, `scripts/serve.sh`, and the capability-detection page `crates/client/web/index.html`), invoked by the `/build-wasm` and `/slice-check` slash commands. It runs two separate `cargo build --target wasm32-unknown-unknown` invocations — the WebGPU build enables the `webgpu` feature and needs `RUSTFLAGS=--cfg=web_sys_unstable_apis`; the WebGL2 build is the default — each followed by `wasm-bindgen`, then `wasm-opt -Oz --converge` (with the explicit BASELINE wasm-feature flags — never `-all`, which emits encodings stable browsers reject, ADR-0017), then brotli; the optimized artifact takes the `client_bg.wasm` name `client.js` fetches. **The Bevy client renders (ADR-0017)** and the sizes are REAL: ~15.6 MB wasm-opt → **~3.45 MB brotli (README "Measurements")** per build (2d-pruned, no assets) — size-budget gate PASS (≤ ~8 MB). The size-optimized release profile is `opt-level="z"`, `lto=true`, `codegen-units=1`, `strip=true`, `panic="abort"`.

**AI workflow:** four subagents (`.claude/agents/`), five slash commands (`.claude/commands/`: `/build-wasm`, `/slice-check`, `/review-netcode`, `/new-crate`, `/write-tests`), and four hooks (`.claude/settings.json` → `scripts/hooks/`: fmt-on-write, destructive-command deny, `tests/`-edit guard, clippy Stop gate) plus the `scripts/git-hooks/pre-commit` clippy+test gate. `/new-crate <name> [--bin]` scaffolds a workspace crate (the `members = ["crates/*"]` glob picks it up automatically).

## Working rules

- **Instrument, don't assume.** Several load-bearing numbers are measurement gaps (WASM size, cold-load, replication bandwidth/peer, in-browser ed25519 cost, STUN-only failure rate). Measure them in the slice; see `docs/final-todo.md §3`.
- No `unwrap()`/`expect()` in non-test code; no new `unsafe` without a `// SAFETY:` comment; never silence a compiler error with a stray `.clone()` or `unsafe`.
- "Compiles but subtly wrong" is the dominant risk in netcode/concurrency — neither the compiler nor clippy catch it. Use TDD and a fresh auditor (never let the session that wrote the code audit itself) for `replication` and `scripting` work.
- The anti-cheat design is sound only inside a specific envelope (low-stakes, casual/co-op, no hidden information, no real-money economy). Free-tier anti-cheat is cost-imposition, not prevention. Don't build features that rely on it as if it were prevention — see `docs/CONTEXT.md §4`.
