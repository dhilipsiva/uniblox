//! What the sample game's script costs (ADR-0046): per `update_agent` call, and as a share of one
//! 64 Hz fixed tick for a range of scripted-entity counts. The escape-hatch trigger is script time
//! above ~10-20% of the frame budget; run in release (`just script-cost`).

use std::time::Instant;

use scripting::{Agent, ScriptEngine};

fn main() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../content/sample/arena.rhai"
    );
    let src = std::fs::read_to_string(path).expect("the sample script");
    let mut engine = ScriptEngine::from_source(&src).expect("the sample compiles");
    let agent = Agent {
        x: 11.0,
        y: 0.0,
        vx: 2.0,
        vy: 0.5,
        contacts: 0,
        tick: 0,
    };
    // Warm up, then measure in frames of 1000 calls each (the frame budget resets per frame).
    for _ in 0..10_000 {
        engine.begin_frame();
        let _ = engine.update_agent(agent);
    }
    let frames = 200;
    let per_frame = 1_000;
    let started = Instant::now();
    let mut ops = 0;
    for _ in 0..frames {
        engine.begin_frame();
        for _ in 0..per_frame {
            let _ = engine.update_agent(agent);
        }
        ops += engine.frame_stats().ops;
    }
    let calls = (frames * per_frame) as f64;
    let per_call_us = started.elapsed().as_secs_f64() * 1e6 / calls;
    let tick_us = 1e6 / 64.0;
    println!(
        "sample update_agent: {per_call_us:.2} us per call, {:.0} ops per call",
        ops as f64 / calls
    );
    for entities in [100, 1_000, 2_000, 5_000] {
        let share = per_call_us * entities as f64 / tick_us * 100.0;
        println!("  {entities:>5} scripted entities: {share:5.1}% of a 64 Hz tick");
    }
}
