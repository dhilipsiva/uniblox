#!/usr/bin/env bash
# Stop hook: blocking clippy gate. Runs `cargo clippy --all-targets -- -D warnings`
# (incremental, fast) and blocks turn-end (exit 2) if it fails, so warnings cannot
# be left behind. Also clears the tests sentinel as a safety net so it never leaks
# past a turn. The full `cargo test` hard gate runs at commit (scripts/git-hooks/pre-commit).
set -euo pipefail

cd "$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
# Run clippy under the pinned flake toolchain (ADR-0010) however the hook is invoked.
source scripts/lib/flake-env.sh
rm -f .claude/allow-test-edits   # safety net: never leave tests/ writable past a turn

if ! cargo clippy --all-targets --quiet -- -D warnings 2>/tmp/uniblox-clippy.err; then
  echo "Clippy gate FAILED (cargo clippy --all-targets -- -D warnings). Fix before finishing:" >&2
  tail -n 40 /tmp/uniblox-clippy.err >&2
  exit 2
fi
exit 0
