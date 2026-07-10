# CLAUDE.md — `client`

**Purpose:** the WASM/native client (winit + wgpu).
**Risk tier:** standard.

## Status
No Bevy yet. The wasm build runs the interim **transport two-tab demo**: connects
to `ws://127.0.0.1:3536/uniblox-demo`, exchanges greetings on both channels, logs
to the console (`[uniblox-demo][STATE]/[EVENT]` markers — asserted by
`scripts/e2e-two-tab.mjs`). Panics and matchbox internals surface in the console
(`console_error_panic_hook` + `console_log`). Native main is still the stub.

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
