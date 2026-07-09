//! `client` — WASM/native client (winit + wgpu).
//!
//! Stub for Phase 1.1. Two WASM builds (WebGPU + WebGL2), single-threaded (no
//! COOP/COEP). See crates/client/CLAUDE.md and scripts/build-wasm.sh.

fn main() {
    println!("uniblox client (stub)");
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert_eq!(2 + 2, 4);
    }
}
