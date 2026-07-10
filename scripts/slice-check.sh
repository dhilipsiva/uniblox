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

echo "-- WASM size (per build) -- (STUB CLIENT — not the size-budget measurement until Bevy renders)"
size_row "webgl2 wasm-opt"  "dist/webgl2/client_bg.opt.wasm"
size_row "webgl2 brotli"    "dist/webgl2/client_bg.opt.wasm.br"
size_row "webgpu wasm-opt"  "dist/webgpu/client_bg.opt.wasm"
size_row "webgpu brotli"    "dist/webgpu/client_bg.opt.wasm.br"
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

echo "-- Pending (environment-gated) --"
printf '  %-36s %s\n' "cold-load time (TTI)"          "needs the rendering Bevy browser client"
printf '  %-36s %s\n' "ed25519 sign/verify (browser)" "needs a desktop browser (WSL2 headless blocked, ADR-0012)"
printf '  %-36s %s\n' "STUN-only connection success"  "needs real-network peers (loopback is meaningless)"
printf '  %-36s %s\n' "real WASM size / feature-prune" "needs the Bevy client (later in Phase 1)"
