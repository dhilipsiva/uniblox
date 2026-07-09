---
description: Print the slice instrumentation table (WASM sizes now; runtime metrics once the slice runs).
allowed-tools: Bash
---

Print the instrumentation table through the WSL wrapper and report it verbatim:

`wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && ./scripts/slice-check.sh"`

The table covers the measurement gaps from `TODO.md` §"Measurement gaps": per-build
WASM size (before/after wasm-opt + brotli), cold-load time, replication bandwidth/peer,
in-browser ed25519 sign/verify cost, STUN-only connection-success rate, and peer RTT/jitter.

At Phase 1.1 only the size columns can be populated (and only once `/build-wasm` has run);
the runtime rows print `pending (Phase 1.8)` until the running slice emits them.
