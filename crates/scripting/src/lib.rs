//! `scripting` — the thin custom Rhai ↔ Bevy-ECS bridge. **HIGH-RISK (sandbox).**
//!
//! A locked-down raw Rhai engine (`Engine::new_raw()` + an explicit whitelist + all
//! `set_max_*` limits + `eval` disabled) is held as a Bevy `NonSend` resource
//! (ADR-0011) and mutates a whitelisted component each tick via [`run_scripts`].
//! This is NOT `bevy_mod_scripting` (no WASM support).
//!
//! The sandbox protects the player's **machine** from malicious **content**; it does
//! nothing against a modified client and is NOT anti-cheat. These are the *initial*
//! limits only — full hardening (wall-clock watchdog, adversarial matrix, the
//! `unchecked`/`internals` CI assertion) is Phase 12.

mod component;
mod engine;
mod error;
mod system;

pub use component::{Health, register_api};
pub use engine::ScriptEngine;
pub use error::ScriptError;
pub use system::{hot_reload_system, insert_scripting, run_scripts};
