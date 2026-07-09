//! Bevy systems + world wiring for the script host.

use bevy_ecs::prelude::*;
use bevy_ecs::system::NonSendMut;

use crate::component::Health;
use crate::engine::ScriptEngine;

/// Per-tick: run `update(Health)` for every entity with a `Health` component. A
/// script error logs a warning and leaves the component unchanged (never panics).
pub fn run_scripts(mut engine: NonSendMut<ScriptEngine>, mut q: Query<&mut Health>) {
    for mut health in &mut q {
        match engine.update_component(health.clone()) {
            Ok(next) => *health = next,
            Err(err) => log::warn!("script update skipped: {err}"),
        }
    }
}

/// Per-tick (native/dev): reload the script if its source file changed.
pub fn hot_reload_system(mut engine: NonSendMut<ScriptEngine>) {
    if let Err(err) = engine.reload_if_changed() {
        log::warn!("hot-reload failed, keeping last-good script: {err}");
    }
}

/// Insert the script host as a `NonSend` resource (it is not `Send + Sync`).
pub fn insert_scripting(world: &mut World, engine: ScriptEngine) {
    world.insert_non_send(engine);
}
