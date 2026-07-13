# CLAUDE.md — `client`

**Purpose:** the WASM/native client (winit + wgpu).
**Risk tier:** standard.

## Status
**Bevy 0.19 renders (ADR-0017)** — wasm32-ONLY dependency (native Bevy would
drag alsa/udev/X11 into the devShell; native parity is Phase 14), pruned to
`["2d", "bevy_winit", "webgl2"]` (+`bevy/webgpu` via the crate's `webgpu`
feature for the second build). **Mode-1 (Standalone) playable view (ADR-0031):**
the net-free `standalone` sim is wired into the `DefaultPlugins` render app —
`Camera2d` + a keyboard-driven `Avatar` + a few drifting NPCs (all locally owned
via `engine_core::{insert_sim, spawn_owned}`), `standalone::add_sim_systems` runs
the engine-core FixedUpdate sim, `drive_avatar` maps held keys → the avatar's
`Velocity` (pure native-tested `move_dir`), `sync_render` copies `Position` →
sprite `Transform` (direct read — Mode 1 is local-authority, no smoothing; Modes
2/3 will read the interpolated `RenderPos`/`copy_owned_render`). No prediction/
interpolation is used. The ADR-0017 `Bouncer` sine demo is replaced; the
`first-frame` metric is kept. Size gate re-checked → PASS (3.39/3.41 MB brotli,
~+10 KB). Alongside it the wasm build runs the **transport two-tab demo**
(`[uniblox-demo][STATE]/[EVENT]` markers; re-verified with Bevy in-binary
2026-07-11) and the **metrics harness** (`[uniblox-metrics]`: ed25519 sign
~20–25 µs / verify ~45 µs; cold-load 351 ms instantiate / 381 ms first frame,
local headless — see `/slice-check`) and the **IndexedDB self-test** (ADR-0035, B4: `idb_selftest()` via
`spawn_local` opens `persistence::IdbStore`, get→put→get a fixed blob → `[uniblox-idb] first session` / `durable:
prior-session blob present` / `roundtrip ok` — reload the tab and the first marker flips to "durable", proving the
browser save persists across reloads; C1 replaces it with the real save UI). Native main is still the stub.

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
