#!/usr/bin/env bash
# Two-build WASM pipeline: WebGL2 (default) + WebGPU (--features webgpu +
# RUSTFLAGS=--cfg=web_sys_unstable_apis). Bevy cannot serve both from one binary
# (issue #13168), so we ship two artifacts + JS capability detection.
#
# Each variant is FULLY processed (bindgen -> wasm-opt -> brotli) before the next
# `cargo build` overwrites target/.../client.wasm.
#
# Phase 1.1 status: the toolchain (wasm-bindgen/wasm-opt/brotli) is not installed
# and the client is a stub (no Bevy), so this exits early with a clear message.
# It becomes meaningful once the toolchain is installed and the client renders
# (Phase 1.3-1.6). See TODO.md §1.1 (remaining) and DECISIONS.md ADR-0002.
set -euo pipefail
cd "$HOME/projects/dhilipsiva/uniblox"

for t in wasm-bindgen wasm-opt brotli; do
  if ! command -v "$t" >/dev/null 2>&1; then
    echo "MISSING tool: $t"
    echo "Install the WASM toolchain before running the pipeline, e.g.:"
    echo "  cargo install wasm-bindgen-cli wasm-opt"
    echo "  sudo apt-get install -y brotli        # or: cargo install brotli"
    echo "Aborting (Phase 1.1: expected — toolchain + Bevy client not yet present)."
    exit 1
  fi
done

RAW="target/wasm32-unknown-unknown/release/client.wasm"

emit () {
  local variant="$1" cargo_args="$2" rustflags="${3:-}"
  echo "== build ${variant} =="
  RUSTFLAGS="${rustflags}" cargo build -p client --release --target wasm32-unknown-unknown ${cargo_args}
  local out="dist/${variant}"
  mkdir -p "${out}"
  wasm-bindgen --target web --no-typescript --out-dir "${out}" --out-name client "${RAW}"
  wasm-opt -Oz --converge -o "${out}/client_bg.opt.wasm" "${out}/client_bg.wasm"
  brotli -f -q 11 "${out}/client_bg.opt.wasm"
  printf '  %-8s raw=%s  bindgen=%s  wasm-opt=%s  brotli=%s (bytes)\n' "${variant}" \
    "$(stat -c%s "${RAW}")" \
    "$(stat -c%s "${out}/client_bg.wasm")" \
    "$(stat -c%s "${out}/client_bg.opt.wasm")" \
    "$(stat -c%s "${out}/client_bg.opt.wasm.br")"
  if command -v twiggy >/dev/null 2>&1; then
    echo "  -- twiggy top (${variant}) --"
    twiggy top -n 15 "${out}/client_bg.opt.wasm" || true
  else
    echo "  (twiggy absent — per-function byte attribution skipped)"
  fi
}

echo "uniblox two-build WASM pipeline"
emit webgl2 ""                 ""                              # default build
emit webgpu "--features webgpu" "--cfg=web_sys_unstable_apis"  # WebGPU build
echo "Done. Artifacts: dist/webgl2/ and dist/webgpu/. Serve with scripts/serve.sh (no COOP/COEP)."
