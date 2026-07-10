//! `client` — WASM/native client (winit + wgpu, later in Phase 1).
//!
//! Two WASM builds (WebGPU + WebGL2), single-threaded (no COOP/COEP). See
//! crates/client/CLAUDE.md and scripts/build-wasm.sh.
//!
//! Until the Bevy client lands, the wasm build runs a transport DEMO: it joins
//! the local signaling room and exchanges greetings on both channels, logging
//! to the browser console — this is the "two browser tabs connect P2P" proof.

/// The interim two-tab transport demo (wasm only).
#[cfg(target_arch = "wasm32")]
mod demo {
    use gloo_timers::future::TimeoutFuture;
    use transport::{PeerState, Transport};
    use wasm_bindgen_futures::spawn_local;

    const ROOM_URL: &str = "ws://127.0.0.1:3536/uniblox-demo";
    const TICK_MS: u32 = 100;

    fn log(msg: &str) {
        web_sys::console::log_1(&msg.into());
    }

    pub fn start() {
        // Surface Rust panics as console.error with a message + backtrace
        // (panic=abort still traps afterwards, but the cause is visible), and
        // route `log` (matchbox internals included) to the console.
        console_error_panic_hook::set_once();
        let _ = console_log::init_with_level(log::Level::Debug);
        log(&format!("[uniblox-demo] connecting to {ROOM_URL}"));
        // Default ICE (STUN): browsers reject an empty ICE-server entry, so
        // the hermetic variant is native-only. Localhost tabs connect via
        // host candidates regardless; STUN is additional, not required.
        let (mut t, loop_fut) = Transport::connect(ROOM_URL);

        spawn_local(async move {
            // Resolves only on disconnect/error.
            if let Err(err) = loop_fut.await {
                log(&format!("[uniblox-demo] message loop ended: {err:?}"));
            }
        });

        spawn_local(async move {
            loop {
                let peers = match t.poll_peers() {
                    Ok(peers) => peers,
                    Err(err) => {
                        log(&format!("[uniblox-demo] transport closed, stopping: {err}"));
                        break;
                    }
                };
                for (peer, state) in peers {
                    log(&format!("[uniblox-demo] peer {peer}: {state:?}"));
                    if matches!(state, PeerState::Connected) {
                        let _ = t.send_state(peer, (*b"state-hello").into());
                        let _ = t.send_event(peer, (*b"event-hello").into());
                    }
                }
                for (peer, pkt) in t.recv_state() {
                    log(&format!(
                        "[uniblox-demo][STATE] from {peer}: {}",
                        String::from_utf8_lossy(&pkt)
                    ));
                }
                for (peer, pkt) in t.recv_events() {
                    log(&format!(
                        "[uniblox-demo][EVENT] from {peer}: {}",
                        String::from_utf8_lossy(&pkt)
                    ));
                }
                TimeoutFuture::new(TICK_MS).await;
            }
        });
    }
}

fn main() {
    #[cfg(target_arch = "wasm32")]
    demo::start();
    #[cfg(not(target_arch = "wasm32"))]
    println!("uniblox client (stub)");
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert_eq!(2 + 2, 4);
    }
}
