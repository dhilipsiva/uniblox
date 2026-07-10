# CLAUDE.md — `client`

**Purpose:** the WASM/native client (winit + wgpu).
**Risk tier:** standard.

## Status
**Bevy 0.19 renders (ADR-0017)** — wasm32-ONLY dependency (native Bevy would
drag alsa/udev/X11 into the devShell; native parity is Phase 14), pruned to
`["2d", "bevy_winit", "webgl2"]` (+`bevy/webgpu` via the crate's `webgpu`
feature for the second build). Minimal scene: `Camera2d` + one asset-free
bouncing sprite into canvas `#uniblox-canvas`, plus a `first-frame` metric.
Alongside it the wasm build runs the **transport two-tab demo**
(`[uniblox-demo][STATE]/[EVENT]` markers; re-verified with Bevy in-binary
2026-07-11) and the **metrics harness** (`[uniblox-metrics]`: ed25519 sign
~20–25 µs / verify ~45 µs; cold-load 351 ms instantiate / 381 ms first frame,
local headless — see `/slice-check`). Native main is still the stub.

## Gotchas (learned here)
- **Bevy's derive macros miss target-scoped deps** — they scan `[dependencies]`
  for the `bevy` facade and emit `bevy_ecs::` paths; `use bevy::ecs as
  bevy_ecs;` in the module fixes it.
- **Hidden tabs never tick**: winit's web loop runs on requestAnimationFrame,
  which browsers suspend for hidden tabs — the app pauses (transport keeps
  running on setTimeout). Expected behavior, not a bug.
- The transport demo + metrics run BEFORE `render::run()` — Bevy's `run()`
  never returns on wasm.

## Crate-local invariants
- **Two WASM builds, not one.** WebGL2 = default build; WebGPU = `--features webgpu`
  with `RUSTFLAGS=--cfg=web_sys_unstable_apis`. Bevy cannot serve both from one
  binary (issue #13168). JS capability detection selects the build — see
  `web/index.html` and `../../scripts/build-wasm.sh`.
- **Single-threaded at launch — do NOT enable SharedArrayBuffer/threads or set
  COOP/COEP headers.** Cross-origin isolation breaks the OAuth/payment popups Mode 3 needs.
- The `webgpu` cargo feature is a no-op until Bevy lands; it exists so the
  two-build invocation is valid today.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
