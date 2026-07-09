//! `server` — headless authoritative Bevy sim (MinimalPlugins + fixed tick).
//!
//! Stub for Phase 1.1. Mode 3 authoritative hub — the SAME sim as the client,
//! with authority reassigned to the server (no logic fork).

fn main() {
    println!("uniblox server (stub)");
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert_eq!(2 + 2, 4);
    }
}
