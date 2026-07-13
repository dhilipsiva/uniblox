//! Demo binary: `cargo run -p standalone` runs a live, net-free Mode-1 sim
//! (local authority over all entities). It runs forever — Ctrl-C to stop.

use bevy_app::AppExit;
use protocol::PeerId;

fn main() -> AppExit {
    standalone::build_standalone_app(PeerId(1), 4).run()
}
