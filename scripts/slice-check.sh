#!/usr/bin/env bash
# Print the slice instrumentation table (TODO.md §"Measurement gaps to instrument").
# Size columns come from the last dist/ build; runtime rows come from
# target/slice-metrics.json, written by the measurement harness:
#   direnv exec . cargo run --release -p replication --example slice_metrics
# Fails gracefully when artifacts/metrics are missing.
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

echo "-- WASM size (per build; served artifact = wasm-opt output; gate: <= ~8 MB brotli) --"
size_row "webgl2 wasm-opt"  "dist/webgl2/client_bg.wasm"
size_row "webgl2 brotli"    "dist/webgl2/client_bg.wasm.br"
size_row "webgpu wasm-opt"  "dist/webgpu/client_bg.wasm"
size_row "webgpu brotli"    "dist/webgpu/client_bg.wasm.br"
echo

echo "-- Measured (native slice, loopback) --"
METRICS=target/slice-metrics.json
if [ -f "$METRICS" ]; then
  /usr/bin/python3 - "$METRICS" <<'PYEOF'
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

echo "-- Measured (in-browser; re-measure per release) --"
printf '  %-36s %s\n' "ed25519 sign (wasm, browser)"   "19.6-25.0 us/op  (2026-07-11, release + opt-level=3 crypto override)"
printf '  %-36s %s\n' "ed25519 verify (wasm, browser)" "44.0-45.6 us/op  (~1.7x native 25.7 us; 8-peer mesh @30Hz ~= 1% of a core)"
printf '  %-36s %s\n' "crypto wasm size cost"          "+106 KB wasm-opt / +55 KB brotli (dalek+override delta, pre-Bevy stub)"
printf '  %-36s %s\n' "cold-load: wasm instantiate"    "351 ms local (headless webgl2, optimized artifact, 2026-07-11)"
printf '  %-36s %s\n' "cold-load: first Bevy frame"    "381 ms local; + download: ~2.7 s @10 Mbps / ~5.4 s @5 Mbps (3.4 MB brotli)"
printf '  %-36s %s\n' "size-budget gate"               "PASS: 3.38/3.40 MB brotli <= ~8 MB; first frame ~3.1 s @10 Mbps (in target), ~5.8 s @5 Mbps (marginal)"
echo

echo "-- Pending (environment-gated) --"
printf '  %-36s %s\n' "STUN-only connection success"  "INSTRUMENT+AGGREGATION ready (ADR-0018): Str0mPeer::telemetry() per peer + FleetMetrics::aggregate → success fraction / kind breakdown / RTT distribution; real NUMBERS need a real-network fleet (loopback = 100% host, ~0.6ms)"
printf '  %-36s %s\n' "  └ browser-side classification" "getStats() candidate-pair type not yet surfaced by matchbox-wasm (follow-up)"
