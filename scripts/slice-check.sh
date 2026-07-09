#!/usr/bin/env bash
# Print the slice instrumentation table (TODO.md §"Measurement gaps to instrument").
# Size columns come from the last dist/ build; runtime rows are populated once the
# running slice emits metrics (the instrumentation step). Fails gracefully if nothing is built yet.
set -euo pipefail
cd "$HOME/projects/dhilipsiva/uniblox"

echo "=== uniblox slice-check — instrumentation table ==="
echo

size_row () {  # $1=label  $2=path
  if [ -f "$2" ]; then
    printf '  %-28s %s bytes\n' "$1" "$(stat -c%s "$2")"
  else
    printf '  %-28s %s\n' "$1" "not built (run /build-wasm)"
  fi
}

echo "-- WASM size (per build) --"
size_row "webgl2 wasm-opt"  "dist/webgl2/client_bg.opt.wasm"
size_row "webgl2 brotli"    "dist/webgl2/client_bg.opt.wasm.br"
size_row "webgpu wasm-opt"  "dist/webgpu/client_bg.opt.wasm"
size_row "webgpu brotli"    "dist/webgpu/client_bg.opt.wasm.br"
echo

echo "-- Runtime metrics (from the running slice) --"
for m in \
  "cold-load time (TTI)" \
  "replication bandwidth / peer" \
  "ed25519 sign cost (in-browser)" \
  "ed25519 verify cost (in-browser)" \
  "STUN-only connection success" \
  "peer RTT" \
  "peer jitter"; do
  printf '  %-32s %s\n' "$m" "pending (instrumentation)"
done
echo
echo "Note: real numbers require a rendering Bevy client (built later in Phase 1)."
