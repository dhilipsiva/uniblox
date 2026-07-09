//! The whitelisted example component + its Rhai registration.
//!
//! This is the REFERENCE pattern for exposing a gameplay component to scripts:
//! `register_type_with_name` + `register_get_set` per field, plus explicit host
//! helpers via `register_fn`. Real gameplay components (engine-core, later) register
//! the same way. Nothing here grants eval / filesystem / network access.

use bevy_ecs::prelude::Component;
use rhai::Engine;

/// A tiny whitelisted component that scripts may read and modify (`h.hp`).
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct Health {
    pub hp: i64,
}

/// Register the whitelisted script API on a raw engine: the `Health` type with a
/// `hp` get/set, plus one example host function.
pub fn register_api(engine: &mut Engine) {
    engine
        .register_type_with_name::<Health>("Health")
        .register_get_set(
            "hp",
            |h: &mut Health| h.hp,
            |h: &mut Health, v: i64| h.hp = v,
        );

    // Example explicit host function (`register_fn`) — a pure, bounded helper.
    engine.register_fn("clampi", |v: i64, lo: i64, hi: i64| v.max(lo).min(hi));
}
