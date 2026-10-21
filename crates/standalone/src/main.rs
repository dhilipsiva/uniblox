//! Demo binary: `cargo run -p standalone` runs a live, net-free Mode-1 sim
//! (local authority over all entities). It runs forever — Ctrl-C to stop.
//!
//! Env: `UNIBLOX_BUNDLE` (optional): a content bundle's manifest, for example
//! `content/sample/manifest.json`; its script then drives the entities (ADR-0045).

use bevy_app::AppExit;
use protocol::PeerId;

fn main() -> AppExit {
    let mut app = standalone::build_standalone_app(PeerId(1), 4);
    if let Ok(manifest) = std::env::var("UNIBLOX_BUNDLE") {
        if let Err(err) = standalone::load_content(&mut app, std::path::Path::new(&manifest)) {
            eprintln!("[standalone] cannot load the bundle {manifest}: {err}");
            return AppExit::error();
        }
        println!("[standalone] content loaded from {manifest}");
    }
    app.run()
}
