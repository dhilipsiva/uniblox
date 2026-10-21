//! `scripting` — the thin custom Rhai ↔ Bevy-ECS bridge. **HIGH-RISK (sandbox).**
//!
//! A locked-down raw Rhai engine (`Engine::new_raw()` + an explicit whitelist + all
//! `set_max_*` limits + `eval` disabled) is held as a Bevy `NonSend` resource
//! (ADR-0011). It mutates a whitelisted component each tick via [`run_scripts`], and steers
//! scripted entities through the [`Agent`] value (ADR-0045; the system lives in `engine-core`).
//! This is NOT `bevy_mod_scripting` (no WASM support).
//!
//! The sandbox protects the player's **machine** from malicious **content**; it does
//! nothing against a modified client and is NOT anti-cheat. These are the *initial*
//! limits only — full hardening (wall-clock watchdog, adversarial matrix, the
//! `unchecked`/`internals` CI assertion) is Phase 12.

mod agent;
mod component;
mod engine;
mod error;
mod system;

pub use agent::{Agent, register_agent};
pub use component::{Health, register_api};
pub use engine::{
    FRAME_BUDGET_TOKEN, FrameStats, MAX_CONTAINER_ELEMENTS, MAX_OPERATIONS_PER_CALL, ScriptEngine,
    ScriptLimits, TIME_BUDGET_TOKEN, sandbox_engine,
};
pub use error::ScriptError;
pub use system::{hot_reload_system, insert_scripting, run_scripts};
