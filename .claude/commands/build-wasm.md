---
description: Run the two-build (WebGPU + WebGL2) WASM pipeline and print the size table.
allowed-tools: Bash
---

Run the two-build WASM pipeline through the WSL wrapper and report its output verbatim:

`wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && ./scripts/build-wasm.sh"`

The script builds WebGL2 (default) and WebGPU (`--features webgpu` +
`RUSTFLAGS=--cfg=web_sys_unstable_apis`), each followed by wasm-bindgen →
`wasm-opt -Oz --converge` → brotli, and prints raw→bindgen→opt→brotli sizes.

If `wasm-bindgen`, `wasm-opt`, or `brotli` is not installed, the script prints
`MISSING tool: <name>` and exits non-zero — report that plainly (at Phase 1.1 the
toolchain and a rendering Bevy client do not exist yet, so this is the expected result).
