#!/usr/bin/env bash
# Print the slice instrumentation table (TODO.md §"Measurement gaps to instrument") and apply the
# WASM size-budget gate.
# Size columns come from the last dist/ build; runtime rows come from
# target/slice-metrics.json, written by the measurement harness:
#   direnv exec . cargo run --release -p replication --example slice_metrics
# Missing metrics are reported, not fatal.
#
# Exit status: non-zero when a built brotli artifact exceeds the budget. With --gate (what
# build-wasm.sh and CI run) only the gate is printed, and a missing artifact fails too.
# UNIBLOX_SIZE_BUDGET_BYTES (default 8000000, the ~8 MB budget) and UNIBLOX_DIST (default dist)
# override the budget and the directory.
set -euo pipefail
cd "$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"

GATE_ONLY=0
if [ "${1:-}" = "--gate" ]; then GATE_ONLY=1; fi
BUDGET="${UNIBLOX_SIZE_BUDGET_BYTES:-8000000}"
DIST="${UNIBLOX_DIST:-dist}"

gate () {
  echo "-- Size-budget gate (brotli, per build; budget ${BUDGET} bytes) --"
  local status=0 variant br size
  for variant in webgl2 webgpu; do
    br="${DIST}/${variant}/client_bg.wasm.br"
    if [ ! -f "${br}" ]; then
      if [ "${GATE_ONLY}" = 1 ]; then
        printf '  %-8s FAIL: %s is missing\n' "${variant}" "${br}"
        status=1
      else
        printf '  %-8s not built (run /build-wasm)\n' "${variant}"
      fi
      continue
    fi
    size="$(stat -c%s "${br}")"
    if [ "${size}" -le "${BUDGET}" ]; then
      printf '  %-8s PASS: %s bytes\n' "${variant}" "${size}"
    else
      printf '  %-8s FAIL: %s bytes is over the budget\n' "${variant}" "${size}"
      status=1
    fi
  done
  return "${status}"
}

if [ "${GATE_ONLY}" = 1 ]; then
  gate
  exit $?
fi

echo "=== uniblox slice-check — instrumentation table ==="
echo

size_row () {  # $1=label  $2=path
  if [ -f "$2" ]; then
    printf '  %-28s %s bytes\n' "$1" "$(stat -c%s "$2")"
  else
    printf '  %-28s %s\n' "$1" "not built (run /build-wasm)"
  fi
}

echo "-- WASM size (per build; served artifact = wasm-opt output; gate: <= ~8 MB brotli) --"
size_row "webgl2 wasm-opt"  "${DIST}/webgl2/client_bg.wasm"
size_row "webgl2 brotli"    "${DIST}/webgl2/client_bg.wasm.br"
size_row "webgpu wasm-opt"  "${DIST}/webgpu/client_bg.wasm"
size_row "webgpu brotli"    "${DIST}/webgpu/client_bg.wasm.br"
echo

echo "-- Measured (native slice, loopback) --"
METRICS=target/slice-metrics.json
if [ -f "$METRICS" ]; then
  python3 - "$METRICS" <<'PYEOF'
import json, sys
m = json.load(open(sys.argv[1]))
bw, rtt, ed = m["bandwidth"], m["rtt"], m["ed25519"]
rows = [
    (f"state channel / peer ({bw['entities']} entities)",
     f"{bw['state_bytes_per_sec']:.0f} B/s  ({bw['state_msgs_per_sec']:.1f} msg/s @ {m['net_tick_hz']:.0f} Hz net tick)"),
    ("events channel / peer (steady)", f"{bw['events_bytes_per_sec']:.0f} B/s"),
    ("peer RTT (loopback)", f"{rtt['mean_us']:.0f} us  ({rtt['note']})"),
    ("peer jitter (loopback)", f"{rtt['jitter_us']:.0f} us"),
    ("ed25519 sign (native)", f"{ed['sign_us']:.1f} us"),
    ("ed25519 verify (native)", f"{ed['verify_us']:.1f} us  (needs opt-level=3 crypto override — see Cargo.toml)"),
]
for label, val in rows:
    print(f"  {label:<36} {val}")
PYEOF
else
  echo "  (no metrics yet — run: direnv exec . cargo run --release -p replication --example slice_metrics)"
fi
echo

echo "-- Recorded in-browser measurements (dated; re-measure per release) --"
printf '  %-36s %s\n' "ed25519 sign (wasm, browser)"   "19.6-25.0 us/op  (2026-07-11, release + opt-level=3 crypto override)"
printf '  %-36s %s\n' "ed25519 verify (wasm, browser)" "44.0-45.6 us/op  (~1.7x native 25.7 us; 8-peer mesh @30Hz ~= 1% of a core)"
printf '  %-36s %s\n' "crypto wasm size cost"          "+106 KB wasm-opt / +55 KB brotli (dalek+override delta, pre-Bevy stub)"
printf '  %-36s %s\n' "cold-load: wasm instantiate"    "351 ms local (headless webgl2, optimized artifact, 2026-07-11)"
printf '  %-36s %s\n' "cold-load: first Bevy frame"    "381 ms local; + download: ~2.7 s @10 Mbps / ~5.4 s @5 Mbps (3.4 MB brotli)"
echo

echo "-- Pending (environment-gated) --"
printf '  %-36s %s\n' "STUN-only connection success"  "INSTRUMENT+AGGREGATION ready (ADR-0018): Str0mPeer::telemetry() per peer + FleetMetrics::aggregate → success fraction / kind breakdown / RTT distribution; real NUMBERS need a real-network fleet (loopback = 100% host, ~0.6ms)"
printf '  %-36s %s\n' "  └ browser-side classification" "getStats() candidate-pair type not yet surfaced by matchbox-wasm (follow-up)"
echo

gate
