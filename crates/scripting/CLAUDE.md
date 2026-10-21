# CLAUDE.md — `scripting`  ⚠️ HIGH-RISK (sandbox)

**Purpose:** Rhai engine + compiled AST + Scope in a Bevy Resource; sandbox
limits; the thin custom ECS bridge.
**Risk tier:** **HIGH-RISK (security-critical).** Plan-mode-first,
`sandbox-auditor` + adversarial tests mandatory.

## Status
Implemented (initial bridge). `ScriptEngine` — a locked-down `Engine::new_raw()` + all `set_max_*` limits +
`disable_symbol("eval")` — held as a Bevy **NonSend** resource (ADR-0011), mutating a whitelisted `Health`
component per tick via `run_scripts`; in-memory + native file-mtime hot-reload. **ADR-0045:** the `Agent` value (`x`/`y`/`contacts`/`tick` read-only, `vx`/`vy`
read-write) and `update_agent`, which `engine_core::run_entity_scripts` calls for each owned `Scripted` entity;
unary minus for FLOAT registered (the raw engine lacks it). rhai 1.25 (non-sync,
`default-features=false`) + bevy_ecs 0.20. Sandbox-audited; compiles for wasm32.
**Phase-12 hardening (ADR-0046, awaiting the owner's review):** limits applied after any host registration
(`with_host_api` cannot loosen them), containers capped at 256 elements (that also caps nesting: deeper nesting
overflowed the native stack), `ScriptLimits` (source cap before compile, a 50 ms wall-clock watchdog per call,
a per-frame ops/time budget with `begin_frame`/`frame_stats`), the whitelist locked in `tests/api_whitelist.txt`,
and `scripts/check-rhai-features.sh` (in `just ci`) failing on `unchecked`/`internals`. `just script-cost`
measures the sample's cost. Known gap: map index-inserts skip the size check (bounded by the op cap).

## Crate-local invariants
- **Thin custom bridge, NOT `bevy_mod_scripting`** (BMS has no WASM support, issue #166).
- Build the engine with **`Engine::new_raw()`** (adds nothing by default) + explicit
  `register_type::<T>()` / `register_fn(...)`. Whitelisted surface only — no eval,
  no filesystem, no network.
- Apply **all** `set_max_*` limits (operations, call levels, string/array/map sizes,
  expr depths, modules). Keep Rhai's **`unchecked` feature OFF** (it voids every
  `set_max_*`) and **`internals` OFF**. A build/CI assertion must fail if either is enabled.
- The sandbox protects the player's **machine** from malicious **content**; it does
  **nothing** against a modified client. It is NOT anti-cheat.
- Keep scripts thin (high-level logic only); hot loops stay in Rust/Bevy systems.

## Rules
A sandbox escape is a machine-compromise, not a bug ticket. Adversarial TDD +
`sandbox-auditor` on every diff. Inherit all root invariants from `../../CLAUDE.md`.
