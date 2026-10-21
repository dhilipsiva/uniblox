#!/usr/bin/env bash
# Two-build WASM pipeline: WebGL2 (default) + WebGPU (--features webgpu +
# RUSTFLAGS=--cfg=web_sys_unstable_apis). Bevy cannot serve both from one binary
# (issue #13168), so we ship two artifacts + JS capability detection.
#
# Each variant is FULLY processed (bindgen -> wasm-opt -> brotli) before the next
# `cargo build` overwrites target/.../client.wasm.
#
# The toolchain (wasm-bindgen/wasm-opt/brotli/twiggy) comes from the flake devShell (ADR-0010). The
# Bevy client renders (ADR-0017), so the two builds differ and their sizes are real; the run ends
# with the brotli size gate (scripts/slice-check.sh --gate). See DECISIONS.md ADR-0002.
set -euo pipefail
cd "$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"

# Run inside the flake devShell (DECISIONS.md ADR-0010) so wasm-bindgen/wasm-opt/brotli/twiggy and the
# pinned cargo resolve however this script is invoked.
source scripts/lib/flake-env.sh

for t in wasm-bindgen wasm-opt brotli; do
  if ! command -v "$t" >/dev/null 2>&1; then
    echo "MISSING tool: $t — the flake devShell should provide it."
    echo "Run inside the flake env: direnv exec . ./scripts/build-wasm.sh"
    echo "(run 'direnv allow' once if you have not — see DECISIONS.md ADR-0010)."
    echo "Aborting."
    exit 1
  fi
done

RAW="${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release/client.wasm"
# An 8 MiB stack instead of the 1 MiB wasm32 default: the worst native recursion a script can reach
# needs ~0.45 MiB in a native release build (ADR-0046, `just stack-check`); this keeps a wide margin
# for wasm frame sizes, extrapolated from native rather than measured in wasm. A stack overflow in
# wasm traps and kills the client tab.
STACK="-C link-arg=-zstack-size=8388608"

emit () {
  local variant="$1" cargo_args="$2" rustflags="${3:-}"
  echo "== build ${variant} =="
  RUSTFLAGS="${rustflags}" cargo build -p client --release --target wasm32-unknown-unknown ${cargo_args}
  local out="dist/${variant}"
  mkdir -p "${out}"
  wasm-bindgen --target web --no-typescript --out-dir "${out}" --out-name client "${RAW}"
  local bindgen_bytes
  bindgen_bytes="$(stat -c%s "${out}/client_bg.wasm")"
  # Enable exactly the BASELINE wasm features rustc emits (wasm-bindgen strips
  # the target_features section, so wasm-opt cannot auto-detect them). Do NOT
  # use `-all`: it also enables experimental proposals and wasm-opt then emits
  # encodings stable browsers reject (found live: 'invalid heap type exact,
  # enable with --experimental-wasm-custom-descriptors' — the optimized
  # artifact was unloadable in Chrome).
  local wasm_features=(
    --enable-bulk-memory --enable-bulk-memory-opt --enable-sign-ext
    --enable-mutable-globals --enable-nontrapping-float-to-int
    --enable-reference-types --enable-multivalue
  )
  wasm-opt "${wasm_features[@]}" -Oz --converge -o "${out}/client_bg.opt.wasm" "${out}/client_bg.wasm"
  # The optimized artifact MUST take the client_bg.wasm name — that is what
  # wasm-bindgen's client.js fetches. (Found live: the page was serving the
  # unoptimized bindgen output while the optimized one sat unused.)
  mv "${out}/client_bg.opt.wasm" "${out}/client_bg.wasm"
  brotli -f -q 11 "${out}/client_bg.wasm"   # -> client_bg.wasm.br (input kept)
  printf '  %-8s raw=%s  bindgen=%s  wasm-opt=%s  brotli=%s (bytes)\n' "${variant}" \
    "$(stat -c%s "${RAW}")" \
    "${bindgen_bytes}" \
    "$(stat -c%s "${out}/client_bg.wasm")" \
    "$(stat -c%s "${out}/client_bg.wasm.br")"
  if command -v twiggy >/dev/null 2>&1; then
    echo "  -- twiggy top (${variant}) --"
    twiggy top -n 15 "${out}/client_bg.wasm" 2>/dev/null \
      || echo "  (twiggy could not parse this wasm — newer feature set; per-function sizes skipped)"
  else
    echo "  (twiggy absent — per-function byte attribution skipped)"
  fi
}

echo "uniblox two-build WASM pipeline"
emit webgl2 ""                 "${STACK}"                               # default build
emit webgpu "--features webgpu" "${STACK} --cfg=web_sys_unstable_apis"   # WebGPU build
# Stage the capability-detection page at the served root.
cp crates/client/web/index.html dist/index.html
echo "Done. Artifacts: dist/webgl2/ and dist/webgpu/ (+ index.html). Serve with scripts/serve.sh (no COOP/COEP)."
# The size budget: fail the build when either brotli artifact is over it.
scripts/slice-check.sh --gate
