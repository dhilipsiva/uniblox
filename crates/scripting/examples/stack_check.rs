//! The worst-case native recursion a script can reach (`scripting::worst_case_stack_scripts`,
//! ADR-0046) must finish on a 1 MiB stack, the wasm32 default, in the release profile the browser
//! ships. A stack overflow aborts the process, so a non-zero exit is the failure. `just stack-check`
//! runs it; pass a stack size in KiB to probe the margin
//! (`cargo run --release -p scripting --example stack_check 512`).

use std::time::Duration;

use scripting::{Health, ScriptEngine, ScriptLimits, worst_case_stack_scripts};

fn main() {
    let kib: usize = std::env::args()
        .nth(1)
        .map(|arg| arg.parse().expect("a stack size in KiB"))
        .unwrap_or(1024);
    for (name, src) in worst_case_stack_scripts() {
        let outcome = std::thread::Builder::new()
            .stack_size(kib * 1024)
            .spawn(move || {
                // Generous time, so the run reaches the bottom of the recursion instead of timing out.
                let limits = ScriptLimits {
                    call_time_budget: Duration::from_secs(60),
                    frame_time_budget: Duration::from_secs(60),
                    ..ScriptLimits::default()
                };
                let mut engine = ScriptEngine::from_source_with(&src, limits).expect("it compiles");
                engine
                    .update_component(Health { hp: 0 })
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .expect("spawn the thread")
            .join()
            .expect("the thread finished");
        match outcome {
            Ok(()) => println!("{name}: finished on a {kib} KiB stack"),
            Err(e) => {
                eprintln!("{name}: failed on a {kib} KiB stack: {e}");
                std::process::exit(1);
            }
        }
    }
}
