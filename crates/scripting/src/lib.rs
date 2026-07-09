//! `scripting` — Rhai engine, sandbox limits, ECS bridge. **HIGH-RISK (sandbox).**
//!
//! Stub for Phase 1 scaffolding. No functional code yet. See crates/scripting/CLAUDE.md:
//! thin custom bridge (NOT bevy_mod_scripting); `new_raw` + explicit `register_*`;
//! all `set_max_*` limits; `unchecked`/`internals` features OFF.

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert_eq!(2 + 2, 4);
    }
}
