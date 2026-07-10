---
description: Print the slice instrumentation table (measured native metrics + labeled pendings).
allowed-tools: Bash
---

Print the instrumentation table through the WSL wrapper and report it verbatim:

`wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && ./scripts/slice-check.sh"`

If the measured rows say "no metrics yet", generate them first (a ~10 s native
run: bandwidth session + ping/echo + ed25519 micro-bench), then re-print:

`wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && direnv exec . cargo run --release -p replication --example slice_metrics"`

Notes when reporting:
- The WASM size rows are the STUB client until Bevy renders — never quote them
  as the size-budget measurement.
- Measured rows are native/loopback (annotated); the pending rows list their
  concrete environment blockers (browser client, desktop browser, real network).
- ed25519 verify depends on the crypto opt-level=3 override in the workspace
  Cargo.toml (the size profile is ~35x slower — a real Phase-6 consideration).
