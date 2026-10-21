//! TDD battery for the `Agent` script API (ADR-0045; HIGH — sandbox): the value a scripted entity's
//! `update(agent)` receives. Position, contacts and tick are read-only; velocity is the one thing a
//! script steers. Written before the implementation. Assert error *variants* or plain failure, never
//! message text.

use rhai::EvalAltResult;
use scripting::{Agent, ScriptEngine};

fn agent() -> Agent {
    Agent {
        x: 11.0,
        y: 0.0,
        vx: 2.0,
        vy: 0.5,
        contacts: 0,
        tick: 7,
    }
}

/// A script steers: it reads the position and rewrites the velocity.
#[test]
fn velocity_is_scriptable() {
    let mut engine =
        ScriptEngine::from_source("fn update(a) { if a.x > 10.0 && a.vx > 0.0 { a.vx = -a.vx; } a }")
            .unwrap();
    let out = engine.update_agent(agent()).unwrap();
    assert_eq!(out.vx, -2.0);
    assert_eq!(out.vy, 0.5);
    assert_eq!((out.x, out.y, out.contacts, out.tick), (11.0, 0.0, 0, 7));
}

/// Contacts and the sim tick are visible to the script.
#[test]
fn contacts_and_tick_are_readable() {
    let mut engine = ScriptEngine::from_source(
        "fn update(a) { if a.contacts > 0 && a.tick == 7 { a.vy = 3.0; } a }",
    )
    .unwrap();
    let out = engine
        .update_agent(Agent {
            contacts: 2,
            ..agent()
        })
        .unwrap();
    assert_eq!(out.vy, 3.0);
}

/// Position is the engine's: a script cannot write it (it would bypass `simulate`).
#[test]
fn position_is_read_only() {
    let mut engine = ScriptEngine::from_source("fn update(a) { a.x = 0.0; a }").unwrap();
    assert!(engine.update_agent(agent()).is_err());
}

/// Contacts are owner-authoritative engine state: read-only to scripts.
#[test]
fn contacts_and_tick_are_read_only() {
    for src in [
        "fn update(a) { a.contacts = 9; a }",
        "fn update(a) { a.tick = 0; a }",
        "fn update(a) { a.y = 1.0; a }",
    ] {
        let mut engine = ScriptEngine::from_source(src).unwrap();
        assert!(engine.update_agent(agent()).is_err(), "{src} should fail");
    }
}

/// The sandbox limits apply to agent scripts too: an infinite loop terminates.
#[test]
fn infinite_loop_terminates() {
    let mut engine = ScriptEngine::from_source("fn update(a) { loop { } a }").unwrap();
    let err = engine.update_agent(agent()).unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorTooManyOperations(..)),
        "expected ErrorTooManyOperations, got {err:?}"
    );
}

/// A script that returns something other than an agent is an error, not a silent pass.
#[test]
fn wrong_return_type_fails() {
    let mut engine = ScriptEngine::from_source("fn update(a) { 42 }").unwrap();
    assert!(engine.update_agent(agent()).is_err());
}

/// No state leaks between calls: a script-level variable does not survive the call.
#[test]
fn no_state_leaks_between_calls() {
    let mut engine = ScriptEngine::from_source("fn update(a) { a.vy = a.vy + 1.0; a }").unwrap();
    let first = engine.update_agent(agent()).unwrap();
    let second = engine.update_agent(agent()).unwrap();
    assert_eq!(first, second);
}
