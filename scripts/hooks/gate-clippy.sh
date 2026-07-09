#!/usr/bin/env bash
# Stop hook: blocking clippy gate. Runs `cargo clippy --all-targets -- -D warnings`
# (incremental, fast) and blocks turn-end (exit 2) if it fails, so warnings cannot
# be left behind. Also clears the tests sentinel as a safety net so it never leaks
# past a turn. The full `cargo test` hard gate runs at commit (scripts/git-hooks/pre-commit).
set -euo pipefail

cd "$HOME/projects/dhilipsiva/uniblox"
# Self-activate the flake devShell (ADR-0010) so clippy runs under the pinned
# toolchain; falls back to ambient rustup if unavailable (graceful).
eval "$(direnv export bash 2>/dev/null)" 2>/dev/null || true
rm -f .claude/allow-test-edits   # safety net: never leave tests/ writable past a turn

if ! cargo clippy --all-targets --quiet -- -D warnings 2>/tmp/uniblox-clippy.err; then
  echo "Clippy gate FAILED (cargo clippy --all-targets -- -D warnings). Fix before finishing:" >&2
  tail -n 40 /tmp/uniblox-clippy.err >&2
  exit 2
fi
exit 0
