//! `client` — WASM/native client.
//!
//! Two WASM builds (WebGPU + WebGL2), single-threaded (no COOP/COEP). See
//! crates/client/CLAUDE.md and scripts/build-wasm.sh.
//!
//! The wasm build runs the minimal Bevy render (ADR-0017: Camera2d + one
//! asset-free sprite, canvas `#uniblox-canvas`) alongside the transport DEMO
//! (joins the local signaling room, exchanges greetings on both channels) and
//! the `[uniblox-metrics]` harness. Native main is still a stub — Bevy is a
//! wasm32-only dependency here; native parity is Phase 14.

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

    /// `performance.now()` in ms — the only monotonic clock on wasm
    /// (`std::time::Instant` panics there).
    fn now_ms() -> f64 {
        web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0)
    }

    /// In-browser ed25519 micro-bench — mirrors the native harness in
    /// `replication/examples/slice_metrics.rs` (fixed key, 64-byte message,
    /// 100 warmup, 1000 iters) so the numbers are directly comparable.
    /// NOTE: release wasm gets the same opt-level=3 crypto override as native
    /// (Cargo.toml profile overrides are target-independent) — the measured
    /// cost, and the wasm SIZE delta it buys, are the Phase-6 tradeoff inputs.
    fn bench_ed25519() {
        use ed25519_dalek::{Signer, SigningKey, Verifier};
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let vk = key.verifying_key();
        let msg = [0xABu8; 64]; // a typical event-sized payload

        for _ in 0..100 {
            let sig = key.sign(&msg);
            if vk.verify(&msg, &sig).is_err() {
                log("[uniblox-metrics] ed25519 self-check FAILED — bench skipped");
                return;
            }
        }

        const ITERS: u32 = 1000;
        let t0 = now_ms();
        let mut sig = key.sign(&msg);
        for _ in 1..ITERS {
            sig = key.sign(&msg);
        }
        let sign_us = (now_ms() - t0) * 1000.0 / f64::from(ITERS);

        let t0 = now_ms();
        let mut all_ok = true;
        for _ in 0..ITERS {
            all_ok &= vk.verify(&msg, &sig).is_ok();
        }
        let verify_us = (now_ms() - t0) * 1000.0 / f64::from(ITERS);

        log(&format!(
            "[uniblox-metrics] ed25519 wasm: sign {sign_us:.1} us/op, \
             verify {verify_us:.1} us/op ({ITERS} iters, verified={all_ok})"
        ));
    }

    pub fn start() {
        // Surface Rust panics as console.error with a message + backtrace
        // (panic=abort still traps afterwards, but the cause is visible), and
        // route `log` (matchbox internals included) to the console.
        console_error_panic_hook::set_once();
        let _ = console_log::init_with_level(log::Level::Debug);
        bench_ed25519();
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

/// The minimal Bevy render (wasm only): one camera, one moving sprite, and a
/// first-frame metric — the smallest thing that makes cold-load/TTI and the
/// two-build sizes REAL measurements (TODO §size-budget gate).
#[cfg(target_arch = "wasm32")]
mod render {
    // Bevy's derive macros detect the `bevy` facade by scanning [dependencies];
    // ours is target-scoped, so they emit `bevy_ecs::` paths — alias them back.
    use bevy::ecs as bevy_ecs;
    use bevy::prelude::*;

    /// Marker for the one demo sprite.
    #[derive(Component)]
    pub struct Bouncer;

    fn setup(mut commands: Commands) {
        commands.spawn(Camera2d);
        commands.spawn((
            Sprite::from_color(Color::srgb(0.9, 0.4, 0.1), Vec2::new(96.0, 96.0)),
            Transform::default(),
            Bouncer,
        ));
    }

    /// Frames demonstrably tick: slide the sprite on a sine.
    fn bounce(time: Res<Time>, mut sprites: Query<&mut Transform, With<Bouncer>>) {
        let x = (time.elapsed_secs() * 1.5).sin() * 160.0;
        for mut transform in &mut sprites {
            transform.translation.x = x;
        }
    }

    /// Logs the first rendered-frame time once ([uniblox-metrics] first-frame)
    /// — the TTI end-marker; navigation start is performance.now()'s origin.
    fn first_frame(mut done: Local<bool>) {
        if !*done {
            *done = true;
            let ms = web_sys::window()
                .and_then(|w| w.performance())
                .map(|p| p.now())
                .unwrap_or(0.0);
            web_sys::console::log_1(
                &format!("[uniblox-metrics] first-frame: {ms:.0} ms since navigation start").into(),
            );
        }
    }

    pub fn run() {
        App::new()
            .add_plugins(DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "uniblox".into(),
                    canvas: Some("#uniblox-canvas".into()),
                    fit_canvas_to_parent: true,
                    // Keep browser shortcuts (F5, devtools) working.
                    prevent_default_event_handling: false,
                    ..Default::default()
                }),
                ..Default::default()
            }))
            .add_systems(Startup, setup)
            .add_systems(Update, (bounce, first_frame))
            .run();
    }
}

fn main() {
    #[cfg(target_arch = "wasm32")]
    {
        // Transport demo + metrics first (spawn_local futures run alongside
        // winit's requestAnimationFrame loop), then the render app — run()
        // does not return on wasm.
        demo::start();
        render::run();
    }
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
