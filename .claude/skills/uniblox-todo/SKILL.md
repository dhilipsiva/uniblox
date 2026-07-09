---
name: uniblox-todo
description: Work a single uniblox TODO item end-to-end — plan (risk-tiered), implement, test/verify, then delete it from TODO.md (or rewrite it if only partially done), then commit and push to the uniblox repo. Use for uniblox platform tasks, one at a time; Phase 1 (the vertical slice) is built first.
---

# Work one uniblox TODO item

Drive a single task from "named" to "committed and pushed." Do exactly one item per invocation — never batch. The
user hands you the next item when this one lands.

The user names or quotes the item. `TODO.md` at the repo root is the source of truth — it is a phased
backlog built top-to-bottom with **Phase 1 (the vertical slice) first**. Each task carries a **risk tier**
(`[HIGH]` / `[LOW]` / `[MIXED]`); obey it (see step 1). If the reference is ambiguous, ask before doing
anything.

> The settled architecture, invariants, workspace map, and build commands live in
> [`CLAUDE.md`](../../../CLAUDE.md); the full backlog and acceptance criteria live in
> [`TODO.md`](../../../TODO.md). Read both before touching non-trivial code — the architecture is settled,
> so record decisions rather than relitigate them.

## Environment

This project lives on WSL2 (Ubuntu); the Windows working dir is a UNC view of the Linux filesystem, and
git run from the Windows side fails with "dubious ownership." Run shell commands through the wrapper:

```
# general (git, file, python3) commands:
wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && <CMD>"

# cargo / WASM-tool / npx commands — prefix with `direnv exec .` to enter the flake env:
wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && direnv exec . <CMD>"
# compound cargo chains: direnv exec . bash -lc '<a && b>'  (bare `direnv exec . a && b` runs b OUTSIDE the env)
```

Toolchain notes (these differ from nibli — do not copy nibli's habits):

- **The toolchain comes from a Nix flake devShell** (`flake.nix`, `DECISIONS.md` ADR-0010): pinned Rust
  (cargo/rustc/clippy/rustfmt, wasm32 target, currently 1.96) + `wasm-bindgen`/`wasm-opt`/`brotli`/`twiggy`/
  `node`, all pinned by `flake.lock`. Run **`direnv allow`** once per clone. Interactive `cd` auto-activates
  (direnv + nix-direnv, already installed); the WSL wrapper enters it via the `direnv exec .` prefix above.
  Ambient rustup (cargo 1.92) is a benign fallback for un-routed commands. No `just` here.
- The **two-WASM-build** pipeline (`scripts/build-wasm.sh`) + `/build-wasm` and `/slice-check` are
  **scaffolded**; the WASM tools are present (via the flake), so `build-wasm.sh` **runs end-to-end**; on the
  current stub the output is byte-identical, KB-sized wasm — **meaningless** for the size budget until the Bevy
  client renders (later in Phase 1). Do NOT claim stub sizes as the size-budget measurement.
- `.claude/` is tracked (this skill file is committed); `/target`, `/dist`, `/result`, `.direnv/`, and the
  local `.claude/settings.local.json` are gitignored — no force-add needed.

## The loop

1. **Locate & plan — respect the risk tier.** Read the relevant crate source/tests for the item.
   Architecture, invariants, and the workspace map are in [`CLAUDE.md`](../../../CLAUDE.md).
   - `[HIGH]` items (the custom replication / authority-swap netcode, the Rhai sandbox, anything touching
     crypto/signing, billing/entitlement, or anti-cheat validation): **plan-mode first**, then TDD with the
     **human specifying the test cases**. State a one-paragraph approach and get it right before writing code.
   - `[LOW]`/`[MIXED]` items: proceed with the acceptance criteria as the contract; state a brief approach
     for anything non-trivial.
   - Pause and ask **only** if the item is genuinely ambiguous or needs a design decision — otherwise run
     the whole loop.

2. **Implement — honor the settled invariants.** Follow [`CLAUDE.md`](../../../CLAUDE.md). Never break a
   settled invariant: single-ownership per entity / no CRDT in the runtime; no cross-platform float
   determinism; WebRTC DataChannels only (no media/SFU); two WASM builds not one; single-threaded WASM
   (no COOP/COEP); custom replication (not lightyear/replicon); thin custom Rhai bridge (not
   `bevy_mod_scripting`). Also: no `unwrap()`/`expect()` in non-test code; no new `unsafe` without a
   `// SAFETY:` comment; never paper over a compiler error with a stray `.clone()` or `unsafe`.

3. **Test / verify.** Report output faithfully — never claim success without running it:
   - `cargo clippy --all-targets -- -D warnings` and `cargo test` (single test: `cargo test <name>`);
     `cargo fmt` before committing.
   - For `[HIGH]` netcode/sandbox items: write the tests **first**, then — if the auditor subagents are set
     up (`TODO.md` Phase 1 scaffolding) — dispatch **netcode-auditor** / **sandbox-auditor** on the diff; otherwise
     do a fresh-context review. "Compiles but subtly wrong" is the dominant netcode risk and neither the
     compiler nor clippy catch it.
   - `/build-wasm` + `/slice-check` for WASM size / cold-load / instrumentation, once scaffolded.
   - Re-run to confirm the change does what the item asked. If verification fails, fix and re-verify.
     **Do not commit broken work.**

4. **Update the tracker (`TODO.md`).** Items are plain `-` bullets — **never `- [ ]` checkboxes** (the
   workflow deletes or rewrites items, it never checks them off): fully done → **delete the bullet
   entirely** (no `~~strikethrough~~`, no "DONE" marker); partially done → **rewrite** the item (and its
   `*Acceptance:*` clause) to state exactly what remains; stale/obsolete → remove it and say so in the
   commit. Preserve the acceptance criteria on any item you keep.

5. **Sync docs.** Update [`CLAUDE.md`](../../../CLAUDE.md) if invariants/commands/workspace changed, the
   relevant per-crate `CLAUDE.md`, and `PROJECT_STATE.md` / `DECISIONS.md` if those exist. Commit code +
   doc changes together.

6. **Commit & push (uniblox repo, branch `main`; remote `origin` = `git@github.com:dhilipsiva/uniblox.git`).**
   - Write the commit message to a temp file and `git commit -F /tmp/msg.txt`. Heredocs and `-m` mangle
     backticks / `?` / quotes inside the double-quoted `bash -lc` wrapper — the file avoids that. (Use the
     Write tool to author the message at `\\wsl.localhost\Ubuntu\tmp\msg.txt`, i.e. `/tmp/msg.txt`.)
   - End the message with the `Co-Authored-By: Claude … <noreply@anthropic.com>` trailer the harness
     specifies for the model actually doing the work — do **not** hardcode a model name here (it goes stale).
   - Scope the commit to the files this item touched (`git add <those files>`). Never sweep in unrelated
     working-tree changes.
   - Before pushing: `git fetch` and check `main` isn't behind; rebase (not merge) if it moved. Then push.
     The **SSH remote can hang** in this environment — if so, push over gh-authed HTTPS:
     `git -c credential.helper='!gh auth git-credential' push https://github.com/dhilipsiva/uniblox.git HEAD:main`.

7. **Stop.** Report what changed + the verification result in a few lines, then wait for the next item.

## Guardrails

- One item per invocation. Don't pick up adjacent items "while you're here."
- Respect risk tiers: `[HIGH]` items are plan-mode-first + TDD + a fresh auditor — never merge them on a
  single self-review.
- Never run long build/test work in background agents — they have died silently mid-run; work in the main
  loop with checkpoint commits.
- If blocked on a decision, ask before implementing — don't guess on irreversible or design-shaped calls.
- The architecture is settled (`CLAUDE.md` invariants). Don't relitigate it mid-task; if reality forces a
  change, record the new decision and why.
