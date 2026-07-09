#!/usr/bin/env bash
# PreToolUse (Bash) hook: deny destructive commands. Exit 2 blocks the tool call
# and feeds stderr back to Claude. Reads the tool event JSON on stdin.
#
# Denied: rm -rf / -fr, `git push --force` / `-f` (but NOT --force-with-lease),
#         DROP TABLE/DATABASE, TRUNCATE TABLE, mkfs, redirect over /dev/sd*.
set -euo pipefail

cmd="$(cat \
  | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("tool_input",{}).get("command",""))' \
  2>/dev/null || true)"

if printf '%s' "$cmd" | grep -Eiq \
  'rm[[:space:]]+-[a-z]*r[a-z]*f|rm[[:space:]]+-[a-z]*f[a-z]*r|git[[:space:]]+push([[:space:]].*)?(--force([[:space:]]|$)|-f([[:space:]]|$))|drop[[:space:]]+(table|database)|truncate[[:space:]]+table|mkfs|>[[:space:]]*/dev/sd'; then
  echo "DENIED by deny-destructive hook: destructive command pattern detected: ${cmd}" >&2
  exit 2
fi
exit 0
