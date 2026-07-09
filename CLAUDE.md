# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**uniblox** — a browser-first, native-secondary, Rust→WASM platform for user-generated multiplayer games. The one novel, load-bearing idea is the **authority-swap**: a *single* per-entity authoritative state-replication mechanism where **only the authority assignment varies by mode**, so the same authored simulation runs Standalone (Mode 1), P2P Hybrid (Mode 2), and Full-Server (Mode 3) with **no logic fork**.

Engine: **Bevy 0.19** (ECS) on wgpu · UGC logic in sandboxed **Rhai** · transport is **WebRTC DataChannels only** (matchbox in-browser, str0m native/server).

## Current state (read this first)

The repo is **greenfield**: `src/main.rs` is a hello-world, `Cargo.toml` has no dependencies, and none of the architecture below exists in code yet. The design is fully specified in `docs/` and is the source of truth for *what* to build. When you scaffold real work, the first step is converting this single-crate package into the Cargo workspace described below.

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

Standard cargo (works today on the hello-world; run from repo root):

```bash
cargo build
cargo test                       # single test: cargo test <name> -- --exact
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Requires a Rust toolchain new enough for **edition 2024** (Rust ≥ 1.85 / recent stable).

**WASM builds are not scaffolded yet** — when you build the two-build pipeline (TODO Phase 1.1), the intended invocations are two separate `cargo build --target wasm32-unknown-unknown` runs: the WebGPU build enables the `webgpu` feature and needs `RUSTFLAGS=--cfg=web_sys_unstable_apis`; the WebGL2 build is the default. Each is followed by `wasm-bindgen`, then `wasm-opt -Oz` on the *final* file, then brotli. The size-optimized release profile is `opt-level="z"`, `lto=true`, `codegen-units=1`, `strip=true`, `panic="abort"`.

## Working rules

- **Instrument, don't assume.** Several load-bearing numbers are measurement gaps (WASM size, cold-load, replication bandwidth/peer, in-browser ed25519 cost, STUN-only failure rate). Measure them in the slice; see `docs/final-todo.md §3`.
- No `unwrap()`/`expect()` in non-test code; no new `unsafe` without a `// SAFETY:` comment; never silence a compiler error with a stray `.clone()` or `unsafe`.
- "Compiles but subtly wrong" is the dominant risk in netcode/concurrency — neither the compiler nor clippy catch it. Use TDD and a fresh auditor (never let the session that wrote the code audit itself) for `replication` and `scripting` work.
- The anti-cheat design is sound only inside a specific envelope (low-stakes, casual/co-op, no hidden information, no real-money economy). Free-tier anti-cheat is cost-imposition, not prevention. Don't build features that rely on it as if it were prevention — see `docs/CONTEXT.md §4`.
