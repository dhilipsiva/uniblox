#!/usr/bin/env bash
# Serve dist/ locally for capability-detection testing.
#
# IMPORTANT: this sets NO Cross-Origin-Opener-Policy / Cross-Origin-Embedder-Policy
# headers, and it must stay that way. Cross-origin isolation (needed only for
# SharedArrayBuffer threads, which we do NOT use at launch) severs window.opener and
# breaks the OAuth sign-in and payment-checkout popups Mode 3 requires. Single-threaded
# WASM at launch — see DECISIONS.md ADR-0003. python3's http.server sets no such
# headers by default, which is exactly what we want.
set -euo pipefail
cd "$HOME/projects/dhilipsiva/uniblox"

if [ ! -d dist ]; then
  echo "dist/ not found — run scripts/build-wasm.sh first."
  exit 1
fi

PORT="${1:-8080}"
echo "Serving dist/ on http://localhost:${PORT}/ with NO COOP/COEP (single-threaded)."
exec /usr/bin/python3 -m http.server "${PORT}" --directory dist
