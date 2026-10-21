#!/usr/bin/env bash
# Mode 1 (standalone) must run with the networking stack ABSENT (ADR-0030): its crate graph carries
# no transport/replication/matchbox/str0m/webrtc. The Cargo.toml dependency list is the
# human-reviewable proof; this is the automated backstop, run by `just ci` and the pre-commit hook.
set -euo pipefail
cd "$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"

tree="$(cargo tree --locked -p standalone --edges normal)"
if printf '%s\n' "${tree}" \
  | grep -Eq '(^|[^[:alnum:]_-])(transport|replication|matchbox[a-z_]*|str0m|webrtc) v[0-9]'; then
  echo "FAILED: standalone must stay net-free (no transport/replication/matchbox/str0m/webrtc)." >&2
  exit 1
fi
echo "standalone is net-free"
