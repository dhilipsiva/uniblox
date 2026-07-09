# DECISIONS.md — Architecture Decision Record (ADR) log

Append-only record of **why**. `CLAUDE.md` is the operating summary of the settled
invariants; this file is the durable rationale so future sessions record new
decisions instead of relitigating old ones. Full reasoning: `docs/CONTEXT.md`.

Each ADR: **Context / Decision / Consequences / Status.** Never edit an accepted
ADR's decision — supersede it with a new, higher-numbered ADR.

---

## ADR-0001 — Cargo workspace, multi-crate, `crates/*` glob layout
- **Context:** the repo began as a single hello-world crate; the settled design is a
  9-crate workspace (`docs/final-todo.md` §Workspace layout).
- **Decision:** virtual workspace manifest at the root; members nested under `crates/`
  via the glob `members = ["crates/*"]`. Flat root-level crates rejected (clutters the
  root control plane; glob avoids editing `members` on every new crate).
- **Consequences:** `/new-crate` just drops a directory under `crates/`; the root stays
  a clean control plane (docs, .claude, scripts, trackers).
- **Status:** Accepted (Phase 1.1).

## ADR-0002 — Two WASM builds, not one
- **Context:** Bevy cannot serve WebGPU and WebGL2 from one binary (issue #13168 open;
  the `webgpu` feature overrides `webgl2`). Prior reports claimed a single runtime-selecting
  binary — refuted.
- **Decision:** ship two `cargo build --target wasm32-unknown-unknown` artifacts (WebGPU with
  `--features webgpu` + `RUSTFLAGS=--cfg=web_sys_unstable_apis`; WebGL2 default) + JS capability detection.
- **Consequences:** 2× build/CI; a capability-detection page (`crates/client/web/index.html`) picks the build.
- **Status:** Accepted.

## ADR-0003 — Single-threaded WASM at launch; no COOP/COEP
- **Context:** SharedArrayBuffer/threads require cross-origin isolation (COOP/COEP), which
  severs `window.opener` and breaks the OAuth sign-in and payment-checkout popups Mode 3 needs.
- **Decision:** single-threaded at launch; set no cross-origin-isolation headers anywhere.
- **Consequences:** single-thread stutter accepted; native is the performance tier. Audio may
  later move to a Web Audio worklet (its own thread, no COOP/COEP) — deferred investigation.
- **Status:** Accepted.

## ADR-0004 — Custom replication protocol (not lightyear / replicon / renet)
- **Context:** no existing crate backs all three modes over WebRTC DataChannels by varying only
  authority. lightyear defers IO to aeronet (no WebRTC-DataChannel layer); bevy_replicon is
  server→client-only.
- **Decision:** a first-party per-entity replication protocol in `crates/replication` (HIGH-RISK).
- **Consequences:** more work + the dominant "compiles but subtly wrong" risk ⇒ TDD + `netcode-auditor`.
- **Status:** Accepted.

## ADR-0005 — Thin custom Rhai bridge (not `bevy_mod_scripting`)
- **Context:** BMS lists WASM as unsupported (issue #166) — disqualifying for a browser-first platform.
- **Decision:** a thin first-party bridge in `crates/scripting` — `Engine::new_raw()` + explicit
  `register_*`, all `set_max_*` limits, `unchecked`/`internals` features OFF.
- **Consequences:** every capability is explicit; the sandbox protects the machine, not the game.
- **Status:** Accepted.

## ADR-0006 — Size-optimized release profile; tests keep unwind
- **Context:** WASM size is a load-bearing risk; the ship build wants aggressive size optimization.
- **Decision:** `[profile.release]` = `opt-level="z"`, `lto=true`, `codegen-units=1`, `strip=true`,
  `panic="abort"`. Scoped to release ONLY — `cargo test` builds under the dev/test profile (unwind),
  so the harness is unaffected.
- **Consequences:** slow release builds (lto + 1 codegen unit) accepted. **Do not run `cargo test
  --release`** (it would build the harness under the abort profile).
- **Status:** Accepted.

## ADR-0007 — Version pins centralized in `[workspace.dependencies]`
- **Context:** the Bevy ecosystem moves fast (pre-1.0, ~3-month cadence); per-crate pins drift.
- **Decision:** every third-party version pin lives once in the root `[workspace.dependencies]`;
  member crates use `dep.workspace = true`.
- **Consequences:** one place to bump per Bevy cycle; no cross-crate version skew.
- **Status:** Accepted (table empty at 1.1 — zero deps).

## ADR-0008 — `tests/` edits gated by a `.claude/allow-test-edits` sentinel
- **Context:** the AI workflow commits tests before implementation so `git diff tests/` proves the
  tests were not tampered with to make implementation pass. A hook must block `tests/` edits during
  implementation turns but allow them during test-writing turns.
- **Decision:** a PreToolUse hook (`scripts/hooks/guard-tests.sh`) blocks edits to any `tests/`
  path unless the sentinel file `.claude/allow-test-edits` exists. `/write-tests` (and the
  `test-writer` subagent) create the sentinel; the Stop hook clears it so it never leaks past a turn.
- **Consequences:** implementation turns cannot silently rewrite tests; the sentinel is gitignored.
- **Status:** Accepted.

## ADR-0009 — Blocking gate split: clippy on Stop, full tests on pre-commit
- **Context:** the always-do gate is `cargo clippy --all-targets -- -D warnings` + `cargo test`.
  Running the full suite on every turn-end through the WSL bridge is too slow/disruptive.
- **Decision:** the `Stop` hook runs clippy only (incremental, fast, blocking on warnings); the full
  `cargo test` hard gate runs at the commit boundary via a tracked git `pre-commit` hook
  (`scripts/git-hooks/pre-commit`, wired with `git config core.hooksPath scripts/git-hooks`).
- **Consequences:** clippy blocks turn-end; both clippy and tests block a commit. Least-disruptive
  design that still satisfies "blocking clippy + test gate."
- **Status:** Accepted.
