#!/usr/bin/env bash
# Two-build WASM pipeline: WebGL2 (default) + WebGPU (--features webgpu +
# RUSTFLAGS=--cfg=web_sys_unstable_apis). Bevy cannot serve both from one binary
# (issue #13168), so we ship two artifacts + JS capability detection.
#
# Each variant is FULLY processed (bindgen -> wasm-opt -> brotli) before the next
# `cargo build` overwrites target/.../client.wasm.
#
# Status: the toolchain (wasm-bindgen/wasm-opt/brotli/twiggy) is provided by the flake devShell
# (ADR-0010), so this runs end-to-end. On the current stub client (no Bevy) the two builds are
# byte-identical and the sizes are meaningless; it becomes meaningful once the client renders
# (built later in Phase 1). See TODO.md Phase 1 (Instrumentation) and DECISIONS.md ADR-0002.
set -euo pipefail
cd "$HOME/projects/dhilipsiva/uniblox"

# Self-activate the flake devShell (DECISIONS.md ADR-0010) so wasm-bindgen/wasm-opt/
# brotli/twiggy + the pinned cargo resolve however this script is invoked; falls back
# to ambient rustup if the env is unavailable (graceful).
eval "$(direnv export bash 2>/dev/null)" 2>/dev/null || true

for t in wasm-bindgen wasm-opt brotli; do
  if ! command -v "$t" >/dev/null 2>&1; then
    echo "MISSING tool: $t — the flake devShell should provide it."
    echo "Run inside the flake env: direnv exec . ./scripts/build-wasm.sh"
    echo "(run 'direnv allow' once if you have not — see DECISIONS.md ADR-0010)."
    echo "Aborting."
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
  # `-all` enables all wasm features so validation accepts modern rustc output
  # (bulk-memory/memory.copy, sign-ext, etc.); wasm-bindgen strips the
  # target_features section wasm-opt would otherwise auto-detect. All these
  # features are baseline in modern browsers.
  wasm-opt -all -Oz --converge -o "${out}/client_bg.opt.wasm" "${out}/client_bg.wasm"
  brotli -f -q 11 "${out}/client_bg.opt.wasm"
  printf '  %-8s raw=%s  bindgen=%s  wasm-opt=%s  brotli=%s (bytes)\n' "${variant}" \
    "$(stat -c%s "${RAW}")" \
    "$(stat -c%s "${out}/client_bg.wasm")" \
    "$(stat -c%s "${out}/client_bg.opt.wasm")" \
    "$(stat -c%s "${out}/client_bg.opt.wasm.br")"
  if command -v twiggy >/dev/null 2>&1; then
    echo "  -- twiggy top (${variant}) --"
    twiggy top -n 15 "${out}/client_bg.opt.wasm" 2>/dev/null \
      || echo "  (twiggy could not parse this wasm — newer feature set; per-function sizes skipped)"
  else
    echo "  (twiggy absent — per-function byte attribution skipped)"
  fi
}

echo "uniblox two-build WASM pipeline"
emit webgl2 ""                 ""                              # default build
emit webgpu "--features webgpu" "--cfg=web_sys_unstable_apis"  # WebGPU build
# Stage the capability-detection page at the served root.
cp crates/client/web/index.html dist/index.html
echo "Done. Artifacts: dist/webgl2/ and dist/webgpu/ (+ index.html). Serve with scripts/serve.sh (no COOP/COEP)."
