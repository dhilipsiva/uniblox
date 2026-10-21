---
description: Run the two-build (WebGPU + WebGL2) WASM pipeline and print the size table.
allowed-tools: Bash
---

Run the two-build WASM pipeline from the repository root and report its output verbatim:

`./scripts/build-wasm.sh`

The script builds WebGL2 (default) and WebGPU (`--features webgpu` +
`RUSTFLAGS=--cfg=web_sys_unstable_apis`), each followed by wasm-bindgen →
`wasm-opt -Oz --converge` → brotli, and prints raw→bindgen→opt→brotli sizes.

If `wasm-bindgen`, `wasm-opt`, or `brotli` is not installed, the script prints
`MISSING tool: <name>` and exits non-zero — report that plainly; the flake dev shell (`nix develop`,
or `direnv allow` once) provides them. The run ends with the size gate (`scripts/slice-check.sh --gate`),
which fails if either brotli artifact exceeds the 8 MB budget.
