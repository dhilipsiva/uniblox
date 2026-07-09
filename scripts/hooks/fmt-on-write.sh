#!/usr/bin/env bash
# PostToolUse (Edit|Write|MultiEdit) hook: run `cargo fmt` after a Rust file is
# written. Advisory only — never blocks a write. Reads the tool event JSON on stdin.
set -euo pipefail

payload="$(cat)"
fp="$(printf '%s' "$payload" \
  | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("tool_input",{}).get("file_path",""))' \
  2>/dev/null || true)"

# Only fmt after .rs edits (skip .md/.toml/.json writes).
case "$fp" in
  *.rs) : ;;
  *)    exit 0 ;;
esac

cd "$HOME/projects/dhilipsiva/uniblox"
# Self-activate the flake devShell (ADR-0010) so `cargo fmt` uses the pinned
# rustfmt; falls back to ambient rustup if unavailable (graceful).
eval "$(direnv export bash 2>/dev/null)" 2>/dev/null || true
cargo fmt >/dev/null 2>&1 || true   # advisory; never block
exit 0
