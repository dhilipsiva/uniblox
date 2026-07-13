//! `client` — WASM/native client.
//!
//! Two WASM builds (WebGPU + WebGL2), single-threaded (no COOP/COEP). See
//! crates/client/CLAUDE.md and scripts/build-wasm.sh.
//!
//! The wasm build runs the Mode-1 (Standalone) playable view (ADR-0031: the
//! net-free `standalone` sim under `DefaultPlugins` — a camera, a keyboard-driven
//! avatar, and drifting NPCs, canvas `#uniblox-canvas`) alongside the transport
//! DEMO (joins the local signaling room, exchanges greetings on both channels)
//! and the `[uniblox-metrics]` harness. Native main is still a stub — Bevy is a
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

/// Map directional key states to a raw movement direction (`x`/`y` in
/// {-1,0,1}; opposing keys cancel). Pure + bevy-free so it unit-tests on native
/// (the render app is wasm-only). `up` → `+y`, `right` → `+x`. Diagonals are
/// un-normalized (the demo avatar moves ~√2× faster on a diagonal) — acceptable
/// for this Mode-1 view; normalize if the controller outlives the demo. The
/// `cfg(any(wasm32, test))` gate compiles it ONLY where used — the wasm render
/// module and the native test — so the native non-test build has no dead code.
#[cfg(any(target_arch = "wasm32", test))]
fn move_dir(up: bool, down: bool, left: bool, right: bool) -> (f32, f32) {
    let dx = i32::from(right) - i32::from(left);
    let dy = i32::from(up) - i32::from(down);
    (dx as f32, dy as f32)
}

/// The Mode-1 (Standalone) playable view (wasm only, ADR-0031): the net-free
/// `standalone` sim wired into the render app. Local authority over all entities
/// (`insert_sim`/`spawn_owned`); `standalone::add_sim_systems` runs the same
/// engine-core FixedUpdate sim the server runs; keyboard input sets the avatar's
/// `Velocity` and each frame `Position` is copied to the sprite `Transform`.
/// Plus the `first-frame` metric (cold-load/TTI + two-build sizes).
#[cfg(target_arch = "wasm32")]
mod render {
    // Bevy's derive macros detect the `bevy` facade by scanning [dependencies];
    // ours is target-scoped, so they emit `bevy_ecs::` paths — alias them back.
    use std::cell::RefCell;
    use std::rc::Rc;

    use bevy::ecs as bevy_ecs;
    use bevy::prelude::*;
    use engine_core::{Position, Velocity, insert_sim, spawn_owned};
    use persistence::{IdbStore, load_world_verified, save_world};
    use protocol::{ContentId, PeerId};
    use wasm_bindgen_futures::spawn_local;

    /// This instance's peer id. In Mode 1 every entity is owned by `LOCAL`, so
    /// `authority_of` is `Local` for all and the sim integrates everything.
    const LOCAL: PeerId = PeerId(1);
    /// Avatar movement speed, world units per second.
    const SPEED: f32 = 160.0;
    /// IndexedDB database + object-store names for the Mode-1 save.
    const DB: &str = "uniblox";
    const STORE: &str = "saves";
    /// localStorage key holding the hex of the latest save's content id (the
    /// mutable "latest save" pointer; the immutable blob lives in IndexedDB).
    const SLOT_KEY: &str = "uniblox.save.latest";

    fn log(msg: &str) {
        web_sys::console::log_1(&msg.into());
    }

    fn local_storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    /// A fetched save awaiting application: `(content id, blob)`.
    type PendingLoad = Rc<RefCell<Option<(ContentId, Vec<u8>)>>>;

    /// Async→ECS inbox (ADR-0036): a `spawn_local` load task deposits the fetched
    /// `(id, blob)`; the exclusive `apply_load` system drains + applies it next
    /// frame. `Rc<RefCell>` is correct here — wasm is single-threaded, so this is
    /// a NonSend resource.
    #[derive(Default)]
    struct LoadInbox(PendingLoad);

    /// Marks the player-controlled avatar (client-local, not a sim component).
    #[derive(Component)]
    pub struct Avatar;

    /// Seed the Mode-1 world: a camera, a keyboard-driven avatar, and a few
    /// locally-owned drifting NPCs. Exclusive — `insert_sim`/`spawn_owned` need
    /// `&mut World`.
    fn setup(world: &mut World) {
        world.spawn(Camera2d);
        insert_sim(world, LOCAL, 1.0 / 64.0);

        let avatar = spawn_owned(
            world,
            LOCAL,
            Position { x: 0.0, y: 0.0 },
            Velocity { x: 0.0, y: 0.0 },
        );
        world.entity_mut(avatar).insert((
            Sprite::from_color(Color::srgb(0.9, 0.4, 0.1), Vec2::splat(48.0)),
            Transform::default(),
            Avatar,
        ));

        // A few locally-owned NPCs drifting in +x — visible evidence the sim runs.
        for i in 0..4 {
            let npc = spawn_owned(
                world,
                LOCAL,
                Position {
                    x: -240.0 + 120.0 * i as f32,
                    y: 140.0,
                },
                Velocity { x: 40.0, y: 0.0 },
            );
            world.entity_mut(npc).insert((
                Sprite::from_color(Color::srgb(0.3, 0.6, 0.9), Vec2::splat(32.0)),
                Transform::default(),
            ));
        }

        log("[uniblox-save] press K to save, L to load");
    }

    /// Read the movement keys and set the avatar's authoritative `Velocity`
    /// directly — in Mode 1 the avatar is locally owned, so `simulate`
    /// integrates it (no prediction/reconciliation needed).
    fn drive_avatar(keys: Res<ButtonInput<KeyCode>>, mut q: Query<&mut Velocity, With<Avatar>>) {
        let (dx, dy) = crate::move_dir(
            keys.pressed(KeyCode::ArrowUp) || keys.pressed(KeyCode::KeyW),
            keys.pressed(KeyCode::ArrowDown) || keys.pressed(KeyCode::KeyS),
            keys.pressed(KeyCode::ArrowLeft) || keys.pressed(KeyCode::KeyA),
            keys.pressed(KeyCode::ArrowRight) || keys.pressed(KeyCode::KeyD),
        );
        for mut vel in &mut q {
            vel.x = dx * SPEED;
            vel.y = dy * SPEED;
        }
    }

    /// Copy each sim entity's authoritative `Position` into its render
    /// `Transform`. Correct for Mode 1 (local authority IS the truth — no
    /// smoothing); Modes 2/3 will read the interpolated `RenderPos` instead
    /// (engine-core `copy_owned_render`). The camera has no `Position`, so the
    /// query skips it.
    fn sync_render(mut q: Query<(&Position, &mut Transform)>) {
        for (pos, mut transform) in &mut q {
            transform.translation.x = pos.x;
            transform.translation.y = pos.y;
        }
    }

    /// Save the live Mode-1 world to IndexedDB on `K`, remembering its content id
    /// in the localStorage pointer. Exclusive — `save_world` reads the whole
    /// `&World`; the async IndexedDB write is handed to `spawn_local`.
    fn save_on_key(world: &mut World) {
        if !world
            .resource::<ButtonInput<KeyCode>>()
            .just_pressed(KeyCode::KeyK)
        {
            return;
        }
        match save_world(world) {
            Ok((id, blob)) => {
                let hex = id.to_hex();
                spawn_local(async move {
                    match IdbStore::open(DB, STORE).await {
                        Ok(store) => match store.put(&blob).await {
                            Ok(_) => {
                                // Only claim "saved" once the pointer persists —
                                // otherwise a later load can't find the blob.
                                let pointer_ok = local_storage()
                                    .map(|ls| ls.set_item(SLOT_KEY, &hex).is_ok())
                                    .unwrap_or(false);
                                if pointer_ok {
                                    log(&format!("[uniblox-save] saved {hex}"));
                                } else {
                                    log(&format!(
                                        "[uniblox-save] blob stored ({hex}) but the save pointer could not be written"
                                    ));
                                }
                            }
                            Err(e) => log(&format!("[uniblox-save] put failed: {e}")),
                        },
                        Err(e) => log(&format!("[uniblox-save] open failed: {e}")),
                    }
                });
            }
            Err(e) => log(&format!("[uniblox-save] serialize failed: {e}")),
        }
    }

    /// Request a load on `L`: read the localStorage pointer and fetch the blob
    /// into the [`LoadInbox`] (applied next frame by `apply_load`). The async
    /// fetch can't touch the `World`, so it deposits into the inbox instead.
    fn load_on_key(keys: Res<ButtonInput<KeyCode>>, inbox: NonSend<LoadInbox>) {
        if !keys.just_pressed(KeyCode::KeyL) {
            return;
        }
        let Some(hex) = local_storage().and_then(|ls| ls.get_item(SLOT_KEY).ok().flatten()) else {
            log("[uniblox-save] no save to load");
            return;
        };
        let Ok(id) = ContentId::from_hex(&hex) else {
            log("[uniblox-save] stored save id is invalid");
            return;
        };
        let cell = inbox.0.clone();
        spawn_local(async move {
            match IdbStore::open(DB, STORE).await {
                Ok(store) => match store.get(id).await {
                    Ok(Some(blob)) => *cell.borrow_mut() = Some((id, blob)),
                    Ok(None) => log("[uniblox-save] save blob missing from store"),
                    Err(e) => log(&format!("[uniblox-save] get failed: {e}")),
                },
                Err(e) => log(&format!("[uniblox-save] open failed: {e}")),
            }
        });
    }

    /// Apply a pending loaded blob to the `World` and RE-ESTABLISH the render
    /// layer. The save records authoritative sim state only (`Position`/
    /// `Velocity`/`Owner`), so `load_world_verified` rebuilds bare entities — this
    /// re-attaches `Sprite`+`Transform` and re-designates the first as the
    /// controllable `Avatar` (the save doesn't record client render/control roles).
    fn apply_load(world: &mut World) {
        let pending = world.non_send::<LoadInbox>().0.borrow_mut().take();
        let Some((id, blob)) = pending else {
            return;
        };
        if let Err(e) = load_world_verified(world, id, &blob, 1.0 / 64.0) {
            log(&format!("[uniblox-save] load failed: {e}"));
            return;
        }
        // Reconstructed entities have `Position` but no `Transform` — re-clothe.
        let naked: Vec<Entity> = world
            .iter_entities()
            .filter(|e| e.get::<Position>().is_some() && e.get::<Transform>().is_none())
            .map(|e| e.id())
            .collect();
        for (i, &e) in naked.iter().enumerate() {
            let (color, size, is_avatar) = if i == 0 {
                (Color::srgb(0.9, 0.4, 0.1), 48.0, true)
            } else {
                (Color::srgb(0.3, 0.6, 0.9), 32.0, false)
            };
            world.entity_mut(e).insert((
                Sprite::from_color(color, Vec2::splat(size)),
                Transform::default(),
            ));
            if is_avatar {
                world.entity_mut(e).insert(Avatar);
            }
        }
        log(&format!("[uniblox-save] loaded {}", id.to_hex()));
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
        let mut app = App::new();
        app.add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "uniblox".into(),
                canvas: Some("#uniblox-canvas".into()),
                fit_canvas_to_parent: true,
                // Keep browser shortcuts (F5, devtools) working.
                prevent_default_event_handling: false,
                ..Default::default()
            }),
            ..Default::default()
        }));
        // Mode-1 sim at 64 Hz (matches the server/standalone tick).
        app.insert_resource(Time::<Fixed>::from_hz(64.0));
        app.insert_non_send(LoadInbox::default());
        standalone::add_sim_systems(&mut app);
        app.add_systems(Startup, setup);
        app.add_systems(
            Update,
            (
                drive_avatar,
                sync_render,
                save_on_key,
                load_on_key,
                apply_load,
                first_frame,
            ),
        );
        app.run();
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
    use super::move_dir;

    #[test]
    fn smoke() {
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn move_dir_maps_axes() {
        assert_eq!(move_dir(false, false, false, false), (0.0, 0.0));
        assert_eq!(move_dir(true, false, false, false), (0.0, 1.0)); // up = +y
        assert_eq!(move_dir(false, true, false, false), (0.0, -1.0)); // down = -y
        assert_eq!(move_dir(false, false, true, false), (-1.0, 0.0)); // left = -x
        assert_eq!(move_dir(false, false, false, true), (1.0, 0.0)); // right = +x
        assert_eq!(move_dir(true, true, true, true), (0.0, 0.0)); // opposing cancels
        assert_eq!(move_dir(true, false, false, true), (1.0, 1.0)); // up-right
    }
}
