//! The Phase-12 adversarial matrix (ADR-0046; HIGH — sandbox). Each hostile script must end with an
//! error, never hang, crash or reach the machine. Written before the implementation; assert error
//! *variants*, never message text.

use std::time::{Duration, Instant};

use rhai::EvalAltResult;
use scripting::{Health, ScriptEngine, ScriptError, ScriptLimits, sandbox_engine};

/// Run one call with generous TIME budgets, so the resource limit under test is the one that fires
/// (the watchdog has its own test below; in a debug build it would otherwise stop the slow map cases
/// first, which is also a correct outcome).
fn run(src: &str) -> Result<Health, Box<EvalAltResult>> {
    let limits = ScriptLimits {
        call_time_budget: Duration::from_secs(60),
        frame_time_budget: Duration::from_secs(60),
        ..ScriptLimits::default()
    };
    ScriptEngine::from_source_with(src, limits)
        .unwrap()
        .update_component(Health { hp: 0 })
}

// ── Resource limits ──────────────────────────────────────────────────────────

#[test]
fn deep_recursion_hits_the_call_depth_limit() {
    let err = run("fn f(n) { f(n + 1) } fn update(h) { f(0); h }").unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorStackOverflow(..)),
        "{err:?}"
    );
}

#[test]
fn huge_strings_arrays_and_maps_are_refused() {
    for src in [
        r#"fn update(h) { let s = "x"; loop { s += s; } }"#,
        "fn update(h) { let a = []; loop { a.push(0); } }",
        // Copying a container into itself doubles it each step: the size check must stop it.
        "fn update(h) { let m = #{ a: 1 }; let i = 0; loop { m[`k${i}`] = m; i += 1; } }",
        "fn update(h) { let a = [1]; loop { a.push(a); } }",
    ] {
        let err = run(src).unwrap_err();
        assert!(
            matches!(*err, EvalAltResult::ErrorDataTooLarge(..)),
            "{src}: {err:?}"
        );
    }
}

/// A known Rhai gap (ADR-0046): inserting NEW keys into a map by index is not checked against the
/// map-size limit. The operation cap still ends the call (measured: under 10,000 keys, under 1 MB,
/// freed when the call returns, because the scope is rewound and scripts keep no state).
#[test]
fn map_index_inserts_are_bounded_by_the_operation_cap() {
    let err = run("fn update(h) { let m = #{}; let i = 0; loop { m[`k${i}`] = i; i += 1; } }")
        .unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorTooManyOperations(..)),
        "{err:?}"
    );
}

#[test]
fn deeply_nested_maps_terminate() {
    let err = run("fn update(h) { let m = #{}; loop { m = #{ inner: m }; } }").unwrap_err();
    assert!(
        matches!(
            *err,
            EvalAltResult::ErrorTooManyOperations(..) | EvalAltResult::ErrorDataTooLarge(..)
        ),
        "{err:?}"
    );
}

#[test]
fn tight_loops_hit_the_operation_limit() {
    let err = run("fn update(h) { let i = 0; while true { i += 1; } h }").unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorTooManyOperations(..)),
        "{err:?}"
    );
}

#[test]
fn deeply_nested_expressions_are_rejected_at_compile_time() {
    let expr = format!("{}1{}", "(".repeat(200), ")".repeat(200));
    let err =
        ScriptEngine::from_source(&format!("fn update(h) {{ let x = {expr}; h }}")).unwrap_err();
    assert!(matches!(err, ScriptError::Parse(..)), "{err:?}");
}

#[test]
fn an_import_is_refused() {
    let compiled = ScriptEngine::from_source(r#"import "evil" as e; fn update(h) { h }"#);
    let refused = match compiled {
        Err(_) => true,
        Ok(mut engine) => engine.update_component(Health { hp: 0 }).is_err(),
    };
    assert!(
        refused,
        "an import must fail at compile time or at run time"
    );
}

// ── Source size, before compile ──────────────────────────────────────────────

#[test]
fn an_oversized_source_is_rejected_before_compile() {
    let limit = ScriptLimits::default().max_source_bytes;
    let src = format!("fn update(h) {{ h }} // {}", "x".repeat(limit));
    let err = ScriptEngine::from_source(&src).unwrap_err();
    assert!(matches!(err, ScriptError::TooLarge { .. }), "{err:?}");
    let mut ok = ScriptEngine::from_source("fn update(h) { h }").unwrap();
    assert!(matches!(
        ok.reload_from_str(&src),
        Err(ScriptError::TooLarge { .. })
    ));
}

#[test]
fn an_oversized_file_is_rejected_without_reading_it_all() {
    let limit = ScriptLimits::default().max_source_bytes;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.rhai");
    std::fs::write(
        &path,
        format!("fn update(h) {{ h }} // {}", "x".repeat(limit)),
    )
    .unwrap();
    let err = ScriptEngine::from_file(&path).unwrap_err();
    assert!(matches!(err, ScriptError::TooLarge { .. }), "{err:?}");
}

// ── The wall-clock watchdog ──────────────────────────────────────────────────

/// A host function that blocks is not counted by the operation limit; the time budget aborts the
/// script at its next operation.
#[test]
fn the_watchdog_aborts_a_script_that_blocks_inside_a_host_call() {
    let limits = ScriptLimits {
        call_time_budget: Duration::from_millis(20),
        ..ScriptLimits::default()
    };
    let mut engine = ScriptEngine::with_host_api(
        "fn update(h) { block_ms(60); let i = 0; while i < 1000 { i += 1; } h }",
        limits,
        |engine| {
            engine.register_fn("block_ms", |ms: i64| {
                std::thread::sleep(Duration::from_millis(ms as u64));
            });
        },
    )
    .unwrap();
    let started = Instant::now();
    let err = engine.update_component(Health { hp: 0 }).unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorTerminated(..)),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// The host callback cannot loosen the sandbox: the limits are applied after it.
#[test]
fn a_host_api_cannot_loosen_the_limits() {
    let mut engine = ScriptEngine::with_host_api(
        "fn update(h) { loop {} }",
        ScriptLimits::default(),
        |engine| {
            engine.set_max_operations(0);
        },
    )
    .unwrap();
    let err = engine.update_component(Health { hp: 0 }).unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorTooManyOperations(..)),
        "{err:?}"
    );
}

// ── The per-frame budget across entities ─────────────────────────────────────

/// Each call stays under the per-call cap, but together they would burn the frame: once the frame
/// budget is spent, later calls in the same frame are refused, and the next frame starts fresh.
#[test]
fn the_frame_budget_caps_the_cost_across_entities() {
    let limits = ScriptLimits {
        frame_ops_budget: 200_000,
        ..ScriptLimits::default()
    };
    let burn = "fn update(h) { let i = 0; while i < 15000 { i += 1; } h }";
    let mut engine = ScriptEngine::from_source_with(burn, limits).unwrap();
    engine.begin_frame();
    let results: Vec<bool> = (0..40)
        .map(|_| engine.update_component(Health { hp: 0 }).is_ok())
        .collect();
    let ran = results.iter().filter(|ok| **ok).count();
    assert!(
        (1..40).contains(&ran),
        "the budget stopped part of the frame (ran {ran})"
    );
    assert!(
        results[ran..].iter().all(|ok| !ok),
        "after the budget is spent, nothing runs"
    );
    let stats = engine.frame_stats();
    assert_eq!(stats.calls as usize, ran);
    assert_eq!(stats.skipped as usize, 40 - ran);
    assert!(stats.ops >= 200_000);
    engine.begin_frame();
    assert!(
        engine.update_component(Health { hp: 0 }).is_ok(),
        "a new frame starts fresh"
    );
}

// ── The whitelist ─────────────────────────────────────────────────────────────

/// The exact set of functions a script can call. A change here is a sandbox change: review it, then
/// update `tests/api_whitelist.txt`.
#[test]
fn the_whitelist_is_exactly_the_reviewed_set() {
    let mut actual = sandbox_engine().gen_fn_signatures(true);
    actual.sort();
    actual.dedup();
    let expected: Vec<String> = include_str!("api_whitelist.txt")
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn calls_outside_the_whitelist_are_rejected() {
    for call in [
        r#"print("x")"#,
        r#"debug("x")"#,
        "exit()",
        "sleep(1)",
        r#"open("/etc/passwd")"#,
        r#"read_file("/etc/passwd")"#,
        r#"system("ls")"#,
        "timestamp()",
        "parse_json(\"{}\")",
    ] {
        let src = format!("fn update(h) {{ {call}; h }}");
        let failed = match ScriptEngine::from_source(&src) {
            Err(_) => true,
            Ok(mut engine) => engine.update_component(Health { hp: 0 }).is_err(),
        };
        assert!(failed, "{call} must not be callable");
    }
}

// ── No state bleed ────────────────────────────────────────────────────────────

#[test]
fn no_state_bleeds_between_engines_or_across_a_reload() {
    let mut a = ScriptEngine::from_source("fn update(h) { h.hp += 1; h }").unwrap();
    let mut b = ScriptEngine::from_source("fn update(h) { h.hp += 1; h }").unwrap();
    assert_eq!(a.update_component(Health { hp: 0 }).unwrap().hp, 1);
    assert_eq!(a.update_component(Health { hp: 0 }).unwrap().hp, 1);
    assert_eq!(b.update_component(Health { hp: 0 }).unwrap().hp, 1);
    a.reload_from_str("fn other(h) { h }").unwrap();
    let err = a.update_component(Health { hp: 0 }).unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorFunctionNotFound(..)),
        "{err:?}"
    );
}

/// Regression (found by this matrix, ADR-0046): nesting maps or arrays up to the size cap must not
/// overflow the native stack, even on a 1 MiB thread (the wasm32 default) in a debug build. An
/// overflow aborts the whole process, so this test cannot pass by accident.
#[test]
fn nesting_to_the_cap_does_not_overflow_a_small_stack() {
    for src in [
        "fn update(h) { let m = #{}; loop { m = #{ inner: m }; } }",
        "fn update(h) { let a = []; loop { a = [a]; } }",
    ] {
        let src = src.to_string();
        let joined = std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || run(&src).is_err())
            .unwrap()
            .join();
        assert_eq!(
            joined.ok(),
            Some(true),
            "the nested script must end with an error"
        );
    }
}

// ── Audit follow-ups (2026-10-21, ADR-0046) ──────────────────────────────────

/// The deepest native recursion content can reach: recursion at the call-depth cap, each body at
/// the function-body depth cap with the call innermost, and at the bottom an `==` of two arrays
/// nested to the container cap. It must end inside the documented stack (debug build, 4 MiB; release
/// is ~4x smaller and `just stack-check` runs it on 1 MiB).
#[test]
fn the_worst_case_recursion_and_nesting_fit_the_documented_stack() {
    let src = scripting::worst_case_stack_script();
    let joined = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let limits = ScriptLimits {
                call_time_budget: Duration::from_secs(60),
                frame_time_budget: Duration::from_secs(60),
                ..ScriptLimits::default()
            };
            let mut engine = ScriptEngine::from_source_with(&src, limits).expect("it compiles");
            engine.update_component(Health { hp: 0 }).map(|h| h.hp)
        })
        .unwrap()
        .join();
    let outcome = joined.expect("no stack overflow");
    assert!(outcome.is_ok(), "the worst case runs within the limits: {outcome:?}");
}

/// Array-package callbacks (`map`, `filter`, `sort`, ...) run in a cloned state whose operations
/// Rhai does not add to the caller's; they must still count against the per-call cap.
#[test]
fn callbacks_count_against_the_operation_cap() {
    let src = "fn burn(x) { let i = 0; while i < 30000 { i += 1; } x }
               fn update(h) { let a = []; let i = 0; while i < 100 { a.push(i); i += 1; } a.map(\"burn\"); h }";
    let err = run(src).unwrap_err();
    assert!(
        matches!(
            *err,
            EvalAltResult::ErrorTerminated(..) | EvalAltResult::ErrorTooManyOperations(..)
        ),
        "{err:?}"
    );
}

/// ... and against the frame budget, which `frame_stats` reports truthfully.
#[test]
fn callbacks_count_against_the_frame_budget() {
    let limits = ScriptLimits {
        call_time_budget: Duration::from_secs(60),
        frame_time_budget: Duration::from_secs(60),
        frame_ops_budget: 50_000,
        ..ScriptLimits::default()
    };
    let src = "fn burn(x) { let i = 0; while i < 2000 { i += 1; } x }
               fn update(h) { let a = []; let i = 0; while i < 100 { a.push(i); i += 1; } a.map(\"burn\"); h }";
    let mut engine = ScriptEngine::from_source_with(src, limits).unwrap();
    engine.begin_frame();
    assert!(engine.update_component(Health { hp: 0 }).is_err(), "the frame budget stops it");
    let ops = engine.frame_stats().ops;
    assert!((50_000..60_000).contains(&ops), "counted ops {ops}");
}

/// Compile time grows with the number of functions, and parsing is not op-counted: the number of
/// functions in a script is capped.
#[test]
fn too_many_functions_are_rejected_at_compile_time() {
    let many: String = (0..500).map(|i| format!("fn f{i}() {{ {i} }}\n")).collect();
    let err = ScriptEngine::from_source(&format!("{many}fn update(h) {{ h }}")).unwrap_err();
    assert!(matches!(err, ScriptError::Parse(..)), "{err:?}");
}

/// The language intrinsics a script can use (not shown by `gen_fn_signatures`): reviewed. A Rhai
/// upgrade that changes one of them breaks this list.
#[test]
fn the_reviewed_intrinsics_work() {
    for body in [
        "let t = type_of(1);",
        "let f = Fn(\"update\");",
        "let f = Fn(\"double\"); let y = f.call(2);",
        "let f = Fn(\"add\"); let g = f.curry(1); let y = g.call(2);",
        "let x = 1; let d = is_def_var(\"x\");",
        "let d = is_def_fn(\"double\", 1);",
        "try { throw 1; } catch (e) { }",
        "for i in 0..3 { }",
        "let s = `${[1, 2]}`;",
    ] {
        let src = format!(
            "fn double(x) {{ x * 2 }} fn add(a, b) {{ a + b }} fn update(h) {{ {body} h }}"
        );
        assert!(run(&src).is_ok(), "{body} should work");
    }
}

/// Strings are capped at 1 KiB (it bounds what each operation can allocate, so the memory one call
/// can reach through the map index-insert gap).
#[test]
fn the_string_cap_is_one_kibibyte() {
    assert!(run(r#"fn update(h) { let s = "x"; let i = 0; while i < 10 { s += s; i += 1; } h }"#).is_ok());
    let err = run(r#"fn update(h) { let s = "x"; let i = 0; while i < 11 { s += s; i += 1; } h }"#)
        .unwrap_err();
    assert!(matches!(*err, EvalAltResult::ErrorDataTooLarge(..)), "{err:?}");
}
