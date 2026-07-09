//! TDD battery for the Rhai ↔ Bevy-ECS bridge (HIGH — sandbox).
//!
//! Written FIRST and locked by the `tests/`-guard hook (`.claude/allow-test-edits`),
//! then the implementation is filled in until green. Assert error *variants*
//! (`matches!(*err, EvalAltResult::…)`), never message text.

use bevy_ecs::prelude::*;
use rhai::EvalAltResult;
use scripting::{Health, ScriptEngine, ScriptError, insert_scripting, run_scripts};

// ── Acceptance ─────────────────────────────────────────────────────────────

/// AC-1: a whitelisted component is mutated each tick through the Bevy schedule.
#[test]
fn whitelisted_mutation_per_tick() {
    let engine = ScriptEngine::from_source("fn update(h) { h.hp += 1; h }").unwrap();
    let mut world = World::new();
    insert_scripting(&mut world, engine);
    let e = world.spawn((Health { hp: 0 },)).id();

    let mut schedule = Schedule::default();
    schedule.add_systems(run_scripts);
    for _ in 0..5 {
        schedule.run(&mut world);
    }

    assert_eq!(world.get::<Health>(e).unwrap().hp, 5);
}

/// AC-1: a call to a function outside the whitelist fails (does not silently pass).
#[test]
fn unregistered_call_fails() {
    let mut engine = ScriptEngine::from_source("fn update(h) { missing_fn(h); h }").unwrap();
    let err = engine.update_component(Health { hp: 0 }).unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorFunctionNotFound(..)),
        "expected ErrorFunctionNotFound, got {err:?}"
    );
}

/// AC-2: an infinite loop TERMINATES WITH AN ERROR, not a hang. This is the
/// canary that proves rhai's `unchecked` feature is OFF — with `unchecked` on,
/// `set_max_operations` is a no-op and this test would hang forever.
#[test]
fn infinite_loop_terminates_not_hang() {
    let mut engine = ScriptEngine::from_source("fn update(h) { loop {} }").unwrap();
    let err = engine.update_component(Health { hp: 0 }).unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorTooManyOperations(..)),
        "expected ErrorTooManyOperations, got {err:?}"
    );
}

/// AC-3: editing the script (in-memory swap) changes behavior without restart.
#[test]
fn hot_reload_in_memory() {
    let mut engine = ScriptEngine::from_source("fn update(h) { h.hp += 1; h }").unwrap();
    assert_eq!(engine.update_component(Health { hp: 0 }).unwrap().hp, 1);

    engine
        .reload_from_str("fn update(h) { h.hp += 10; h }")
        .unwrap();
    assert_eq!(engine.update_component(Health { hp: 0 }).unwrap().hp, 10);
}

/// AC-3: editing the script FILE and reloading on mtime change swaps behavior;
/// an unchanged file does not reload.
#[test]
fn hot_reload_file_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.rhai");
    std::fs::write(&path, "fn update(h) { h.hp += 1; h }").unwrap();
    filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1_000_000_000, 0)).unwrap();

    let mut engine = ScriptEngine::from_file(&path).unwrap();
    assert_eq!(engine.update_component(Health { hp: 0 }).unwrap().hp, 1);
    assert!(
        !engine.reload_if_changed().unwrap(),
        "unchanged file must not reload"
    );

    std::fs::write(&path, "fn update(h) { h.hp += 10; h }").unwrap();
    filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(2_000_000_000, 0)).unwrap();
    assert!(
        engine.reload_if_changed().unwrap(),
        "changed file must reload"
    );
    assert_eq!(engine.update_component(Health { hp: 0 }).unwrap().hp, 10);
}

// ── Sandbox smoke (initial limits; full adversarial matrix is Phase 12) ─────

/// A script that grows a string past `set_max_string_size` is stopped, not OOM.
#[test]
fn huge_string_data_too_large() {
    let mut engine =
        ScriptEngine::from_source(r#"fn update(h) { let s = "x"; loop { s += s; } }"#).unwrap();
    let err = engine.update_component(Health { hp: 0 }).unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorDataTooLarge(..)),
        "expected ErrorDataTooLarge, got {err:?}"
    );
}

/// A script that grows an array past `set_max_array_size` is stopped, not OOM.
#[test]
fn large_array_data_too_large() {
    let mut engine =
        ScriptEngine::from_source("fn update(h) { let a = []; loop { a.push(0); } }").unwrap();
    let err = engine.update_component(Health { hp: 0 }).unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorDataTooLarge(..)),
        "expected ErrorDataTooLarge, got {err:?}"
    );
}

/// `eval(...)` is rejected at COMPILE time (disabled symbol) — a parse error,
/// not a runtime error.
#[test]
fn eval_rejected_at_compile() {
    let err = ScriptEngine::from_source(r#"fn update(h) { eval("1 + 1"); h }"#).unwrap_err();
    assert!(
        matches!(err, ScriptError::Parse(..)),
        "expected ScriptError::Parse, got {err:?}"
    );
}
