---
description: Scaffold a new workspace crate under crates/<name> and verify it builds.
argument-hint: <crate-name> [--bin]
allowed-tools: Read, Write, Edit, Bash
---

Scaffold a new workspace crate named `$1` (pass `--bin` for a binary crate; default is a library).

Steps:
1. Create `crates/$1/Cargo.toml`:
   ```toml
   [package]
   name = "$1"
   version.workspace = true
   edition.workspace = true

   [dependencies]
   ```
2. Create the source with a smoke test:
   - library → `crates/$1/src/lib.rs`
   - `--bin` → `crates/$1/src/main.rs` with a `fn main()`
   both containing `#[cfg(test)] mod tests { #[test] fn smoke() { assert_eq!(2 + 2, 4); } }`
   and a `//!` doc comment stating the crate's purpose and "Stub — no functional code yet."
3. Create `crates/$1/CLAUDE.md` from the per-crate template (purpose, risk tier, status=stub,
   crate-local invariants, "inherit root invariants from ../../CLAUDE.md").

The root `members = ["crates/*"]` glob includes it automatically — do NOT edit the root Cargo.toml.

Add **no** third-party dependencies unless the task explicitly requires them; new version pins go
in the root `[workspace.dependencies]`, not the member crate. Then verify:

`wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && cargo build -p $1 && cargo test -p $1"`
