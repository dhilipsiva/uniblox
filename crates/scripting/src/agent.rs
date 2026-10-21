//! The `Agent` script API (ADR-0045): the value a scripted entity's `update(agent)` receives and
//! returns. It is a plain data copy, never a handle into the ECS: position, contacts and tick are
//! read-only (the engine owns them), and velocity is the one thing a script steers. Floats are Rhai's
//! `FLOAT` (f64); the engine converts at the boundary.

use rhai::{Engine, FLOAT, INT};

/// One scripted entity's view for one tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Agent {
    /// Position (read-only to scripts).
    pub x: FLOAT,
    pub y: FLOAT,
    /// Velocity, in world units per second (read-write).
    pub vx: FLOAT,
    pub vy: FLOAT,
    /// The entity's owner-authoritative contact tally (read-only).
    pub contacts: INT,
    /// The authoritative sim tick (read-only).
    pub tick: INT,
}

/// Register `Agent`: getters for every field, setters for the velocity only. An assignment to a
/// read-only property is a script error (there is no setter to call).
pub fn register_agent(engine: &mut Engine) {
    engine
        .register_type_with_name::<Agent>("Agent")
        .register_get("x", |a: &mut Agent| a.x)
        .register_get("y", |a: &mut Agent| a.y)
        .register_get_set(
            "vx",
            |a: &mut Agent| a.vx,
            |a: &mut Agent, v: FLOAT| a.vx = v,
        )
        .register_get_set(
            "vy",
            |a: &mut Agent| a.vy,
            |a: &mut Agent, v: FLOAT| a.vy = v,
        )
        .register_get("contacts", |a: &mut Agent| a.contacts)
        .register_get("tick", |a: &mut Agent| a.tick);
    // The raw engine has no unary minus for FLOAT, and steering needs it (`a.vx = -a.vx`). A pure,
    // bounded operator: no state, no I/O.
    engine.register_fn("-", |v: FLOAT| -v);
}
