# uniblox

**Status:** pre-release (0.1.0, nothing published): the netcode core and the Mode-1 and Mode-3 runtimes work and are tested natively; scripting, the platform backend and the native client are not built yet. Details under [Status](#status).

uniblox is a browser-first Rust engine for multiplayer games, built around one idea, the *authority-swap*: a single per-entity, authoritative state-replication mechanism in which only the assignment of authority differs between modes. The same simulation therefore runs standalone, peer-to-peer and on a server with no fork in the game logic. It is being developed as a netcode library with a hosted demo.

The simulation uses Bevy 0.20's ECS. The transport is WebRTC DataChannels only (matchbox in the browser, str0m natively; no media, no SFU), and the browser build is single-threaded WebAssembly. Game logic is meant to run as sandboxed Rhai scripts.

## Status

Implemented and tested natively:
- the replication protocol, the two-channel transport and the scoped signaling service, with a shared Redis session registry;
- the Mode-1 (standalone) and Mode-3 (server) runtimes;
- content-addressed saves.

In the browser, the client renders the shared simulation in both WebGPU and WebGL2 builds (3.4 MB each, brotli) and saves and reloads the world. `just browser-e2e` checks this in headless Chromium on every CI run: the first frame, a save → reload → load round trip through IndexedDB, and two tabs exchanging packets on both channels.

Not built yet:
- `scripting`: the Rhai engine is sandbox-tested, but no runtime uses it, so games cannot be scripted yet.
- `platform`: an 11-line stub.
- The native client: a stub that prints its name.
- Mode 2 end to end in a browser.
- Real-network numbers, such as the STUN-only connection rate.
- Published crates.

## Measurements

Measured on 2026-10-21 with Bevy 0.20 and Rust 1.99.1 (`just wasm`, and the `slice_metrics` example of `replication`), on one Linux workstation:

| What | Result |
|---|---|
| WebGL2 build | 16,381,550 bytes after wasm-opt, 3,452,507 bytes brotli |
| WebGPU build | 16,452,790 bytes after wasm-opt, 3,473,869 bytes brotli |
| Size budget (8,000,000 bytes brotli per build) | pass, both builds |
| State replication, 2 entities at ~20 Hz, native loopback | 879 to 918 B/s per peer over three runs (the pre-update tree measures 881 B/s) |
| Round trip on the reliable channel, native loopback | 4.0 ms, 0.45 ms jitter (1 ms poll granularity) |
| ed25519 sign / verify, native | 9.3 / 18.9 µs |
| STUN-only connection success, real-network RTT | not measured yet: needs peers on real networks |

The browser figures in `scripts/slice-check.sh` (cold load, in-browser ed25519) were recorded on 2026-07-11 and have not been re-measured since.

## Modes

| Mode | Who is authoritative | Where it runs today |
|---|---|---|
| 1, Standalone | The local player owns every entity; the networking stack is absent. | `standalone` (headless) and the browser `client`, which saves and reloads the world by content ID in IndexedDB. |
| 2, P2P hybrid | Each entity has one owning peer. A coordinator, the lowest live peer, arbitrates ownership claims and transfers, and survivors re-assign a departed peer's entities. | `replication` and `transport` between peers, matched by the `services` signaling server. Tested natively; the browser flow is not built yet. |
| 3, Full server | A headless server owns every entity; clients predict their own avatar and interpolate the rest. | `server`, a headless authoritative runtime with a 64 Hz fixed tick. |

Modes 2 and 3 run identical systems and differ only in ownership data (ADR-0014).

## Crates

| Crate | Role |
|---|---|
| `protocol` | Wire types: peer and network-entity identity, quantization, replication messages, content IDs. |
| `replication` | The authority-gated replication protocol: acked-baseline deltas, interest management, prediction and reconciliation, ownership arbitration, resync. |
| `transport` | Two-channel WebRTC DataChannels (unreliable state, reliable events) over matchbox and str0m. |
| `services` | Signaling and the session registry: mode- and version-scoped rooms, session-size grouping, an optional shared Redis registry. |
| `engine-core` | The shared simulation: components, systems and `authority_of`, the single authority decision. |
| `standalone`, `server` | The Mode-1 and Mode-3 runtimes. |
| `client` | The browser client, built twice (WebGPU and WebGL2). |
| `persistence` | Content-addressed saves (blake3 IDs) with in-memory, file and IndexedDB stores. |
| `scripting` | A locked-down Rhai engine, sandbox-tested; no other crate depends on it yet. |
| `platform` | A placeholder for identity, billing, publishing and moderation. |

## Build, run and test

The toolchain (Rust 1.99.1 with the wasm32 target, wasm-bindgen, wasm-opt, brotli, node, coturn and redis) comes from the Nix flake:

```bash
nix develop                  # or `direnv allow` once; .envrc uses the flake
just ci                      # the gate CI runs: fmt, clippy and rustdoc with -D warnings, every test, cargo deny, the Mode-1 net-free check
just test                    # the tests that need no external service (not --release: that profile aborts on panic)
just wasm                    # both browser builds into dist/, ending with the 8 MB brotli size gate
git config core.hooksPath scripts/git-hooks   # once per clone: the pre-commit hook (fmt, clippy, tests)
```

The three TURN-relay and two shared-registry tests start `turnserver` and `redis-server`, so they are `#[ignore]`d: `just test` (or a plain `cargo test`) skips them, and `just test-all` and `just ci` run them with the binaries the dev shell provides. `just --list` shows every recipe. CI (`.github/workflows/ci.yml`) runs `just ci`, `just wasm` and `nix flake check` through `nix develop`.

Running it:

```bash
cargo run -p services        # signaling on port 3536 (UNIBLOX_SIGNALING_PORT; UNIBLOX_REDIS_URL for a shared registry)
cargo run -p server          # Mode 3; joins UNIBLOX_SIGNALING_URL (default ws://127.0.0.1:3536/uniblox-demo)
cargo run -p standalone      # Mode 1, headless
scripts/serve.sh             # serve dist/ (after `just wasm`) locally
```

## Records

- [DECISIONS.md](DECISIONS.md): the architecture decision log (40 ADRs).
- [TODO.md](TODO.md): the phase plan and its acceptance criteria.
- [PROJECT_STATE.md](PROJECT_STATE.md): current status.
- [docs/CONTEXT.md](docs/CONTEXT.md): background and the open naming question.
- [CLAUDE.md](CLAUDE.md): the operating context for coding agents.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
