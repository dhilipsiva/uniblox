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
cargo fmt >/dev/null 2>&1 || true   # advisory; never block
exit 0
