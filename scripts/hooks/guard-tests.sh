#!/usr/bin/env bash
# PreToolUse (Edit|Write|MultiEdit) hook: block edits to any tests/ directory
# during an implementation turn. A test-writing turn lifts the guard by creating
# the sentinel .claude/allow-test-edits (see /write-tests and the test-writer
# subagent). Exit 2 blocks the edit. Reads the tool event JSON on stdin.
set -euo pipefail

fp="$(cat \
  | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("tool_input",{}).get("file_path",""))' \
  2>/dev/null || true)"

# Match a tests/ directory in POSIX (/tests/) or Windows-UNC (\tests\) form.
if printf '%s' "$fp" | grep -Eq '(^|[\/])tests[\/]'; then
  if [ ! -f "$HOME/projects/dhilipsiva/uniblox/.claude/allow-test-edits" ]; then
    echo "DENIED by guard-tests hook: tests/ edits are gated during implementation turns." >&2
    echo "Use /write-tests (or the test-writer subagent), which sets .claude/allow-test-edits." >&2
    exit 2
  fi
fi
exit 0
