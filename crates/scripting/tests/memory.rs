//! The memory one hostile call can reach (ADR-0046; HIGH — sandbox). Rhai's size check covers each
//! value a script builds, not the container an index assignment writes into, so a script can grow an
//! array tree in place, one checked value per assignment, until the operation cap ends the call. This
//! binary counts every allocation (its own global allocator) and bounds the peak.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use scripting::{Health, ScriptEngine, ScriptLimits};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(bytes: usize) {
    let now = CURRENT.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` with the caller's arguments unchanged, so `System`'s
// guarantees carry over; the counters only observe sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which `System` shares.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc`/`realloc` above, i.e. from `System`, with this layout.
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: as for `dealloc`; `new_size` is the caller's, under `realloc`'s contract.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
            grew(new_size);
        }
        new
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A 1000-byte string, just under the 1 KiB string cap (room for a short prefix).
fn kib() -> String {
    format!("let s = \"{}\";", "y".repeat(1000))
}

/// The heaviest shapes found (2026-10-21): array trees grown in place by index assignment, each
/// assignment cloning a checked value of up to 127 elements and a 1 KiB string.
fn hostile_scripts() -> Vec<String> {
    let s = kib();
    let fill = "let v = []; while v.len() < 127 { v.push(0); }";
    vec![
        // Two levels: 127 x 127 slots, each a copy of a 127-element array.
        format!(
            "{s} {fill} let leaf = v; leaf[0] = s; let a = v; let i = 0; \
             while i < 127 {{ a[i] = v; i += 1; }} i = 0; \
             loop {{ let j = 0; while j < 127 {{ a[i][j] = leaf; j += 1; }} i += 1; }}"
        ),
        // Three levels.
        format!(
            "{s} {fill} let a = v; let i = 0; while i < 127 {{ a[i] = v; i += 1; }} \
             let b = v; i = 0; while i < 127 {{ b[i] = 0; i += 1; }} \
             i = 0; loop {{ let j = 0; while j < 127 {{ a[i][j] = v; let k = 0; \
             while k < 127 {{ a[i][j][k] = v; k += 1; }} j += 1; }} i += 1; }}"
        ),
        // Many top-level arrays, each slot reassigned.
        format!(
            "{s} {fill} let a = v; let b = v; let c = v; let d = v; let i = 0; \
             loop {{ a[i % 127] = v; b[i % 127] = v; c[i % 127] = v; d[i % 127] = v; i += 1; }}"
        ),
        // Strings: each slot a 1 KiB string (shared, so cheap), and fresh strings built per slot.
        format!(
            "{s} {fill} let a = v; let i = 0; while i < 127 {{ a[i] = v; i += 1; }} \
             i = 0; loop {{ let j = 0; while j < 127 {{ a[i][j] = `${{j}}${{s}}`; j += 1; }} i += 1; }}"
        ),
    ]
}

/// The bound ADR-0046 documents: one call stays under 64 MiB above where it started (measured
/// ~13 MiB for the heaviest shape), and everything is freed when the call returns.
#[test]
fn one_hostile_call_stays_under_the_documented_memory_bound() {
    const BOUND: usize = 64 * 1024 * 1024;
    let limits = ScriptLimits {
        call_time_budget: Duration::from_secs(60),
        frame_time_budget: Duration::from_secs(60),
        ..ScriptLimits::default()
    };
    for src in hostile_scripts() {
        let mut engine =
            ScriptEngine::from_source_with(&format!("fn update(h) {{ {src} h }}"), limits)
                .expect("it compiles");
        let before = CURRENT.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let failed = engine.update_component(Health { hp: 0 }).is_err();
        let peak = PEAK.load(Ordering::Relaxed) - before;
        let after = CURRENT.load(Ordering::Relaxed);
        assert!(failed, "the operation cap ends it: {src}");
        assert!(peak < BOUND, "peak {peak} bytes: {src}");
        assert!(
            after.saturating_sub(before) < 64 * 1024,
            "the call's memory is freed when it returns ({} bytes left): {src}",
            after.saturating_sub(before)
        );
        println!("peak {:.1} MiB", peak as f64 / (1024.0 * 1024.0));
    }
}
