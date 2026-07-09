---
description: Dispatch the netcode-auditor subagent on the current replication/transport diff.
argument-hint: [extra focus, e.g. "handoff path"]
---

Use the **netcode-auditor** subagent to review the current working diff touching
`crates/replication`, `crates/transport` (and `crates/engine-core` if the authority
logic changed).

Extra focus for this review: $ARGUMENTS

Requirements:
- This MUST be a fresh, independent review — never the session that wrote the code.
- The auditor is read-only (no Write/Edit); it reports findings only.
- Summarize its findings ranked by severity and then STOP. Do not fix anything inline
  without explicit human direction — HIGH-RISK netcode changes go back through plan-mode.
