---
description: Enter test-writing mode (lifts the tests/ edit guard), dispatch test-writer, then re-lock.
argument-hint: <what to test, e.g. "replication quantization round-trip">
allowed-tools: Read, Write, Edit, Bash
---

Write tests for: $ARGUMENTS

The `guard-tests` PreToolUse hook blocks edits to any `tests/` directory during
implementation turns. This command lifts that guard, writes tests, then re-locks:

1. Create the sentinel:
   `wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && touch .claude/allow-test-edits"`
2. Use the **test-writer** subagent to author/modify the tests for the target above.
   For HIGH-RISK crates (`replication`, `scripting`, crypto/billing) the human must have
   specified the cases — implement exactly those, no more.
3. Remove the sentinel:
   `wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && rm -f .claude/allow-test-edits"`

The `Stop` hook also clears the sentinel as a safety net, so it never leaks past a turn.
Commit the tests before writing the implementation so `git diff tests/` proves they were not tampered with.
