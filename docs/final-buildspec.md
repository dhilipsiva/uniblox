# BUILD SPEC — Browser-First Rust/WASM UGC Game Platform with Tri-Mode WebRTC Networking

## TL;DR
- **Build the per-entity replication protocol custom on top of a raw WebRTC-DataChannel transport (matchbox in browser, str0m native).** No existing Rust crate can back all three modes over WebRTC DataChannels by varying only authority: lightyear (0.26.4) defers IO to aeronet, which has NO WebRTC-DataChannel layer, and its distributed/peer authority is explicitly "still in flux" and untested; bevy_replicon is server→client-only by design.
- **Ship TWO WASM builds (WebGPU + WebGL2) with JS capability detection, NOT one runtime binary**, and do NOT enable COOP/COEP cross-origin isolation at launch (it breaks the OAuth + payment popups Mode 3 requires). Bevy issue #13168 is still OPEN.
- **The unified-sync thesis holds**: a single authoritative per-entity replication mechanism, varying only authority assignment, cleanly yields Modes 1/2/3 with no logic fork — but you own the netcode. Plan for ~15-25% of Mode-2 STUN-only peer connections silently failing.

## Key Findings (contested claims resolved, as of July 2026)

1. **D.1 PIVOTAL — state replication build-vs-reuse: BUILD custom.** *Confidence: established.* Claim C (stale) is refuted — lightyear is at 0.26.4, not 0.17. But Claim B (Grok: "one crate backs all three modes over WebRTC-DC") is **false today**, and Claim A (Gemini/Claude) is **substantially correct**. lightyear now defers all IO to the `aeronet` crate; confirmed at lightyear 0.26.4 (docs.rs, latest), with maintainer cBournhonesque's release notes stating verbatim: *"Instead of maintaining my own steam, webtransport, websocket io layers, I now defer to the excellent aeronet crate!"* aeronet's own docs list only channel, websocket, webtransport, and steam IO layers — **there is no WebRTC-DataChannel IO layer for aeronet or lightyear**. lightyear's README states authority handling "is still somewhat in flux… hasn't been properly tested, and the distributed_authority example is still outdated." bevy_replicon is documented as server-authoritative with components "only replicated from the server to the clients, and never the other way around." Therefore no existing crate meets the requirement; the per-entity protocol must be custom-built over matchbox/str0m.

2. **Single WASM WebGPU/WebGL2 fallback binary: NO — two builds.** *Confidence: established.* wgpu did add runtime backend detection (gfx-rs/wgpu #5044), which misled DeepSeek/ChatGPT/Gemini. But **Bevy has not integrated it**: Bevy issue #13168 ("Support WebGL2 and WebGPU in the same WASM file") remains OPEN, with the maintainer noting progress "just increase[d] slightly." Bevy's own docs state enabling the `webgpu` feature "will override the webgl2 feature, and builds with the webgpu feature enabled won't be able to run on browsers that don't support WebGPU." Claude/Grok correct.

3. **Bevy version & maturity.** *Confidence: established.* Current stable is **Bevy 0.19.0 (released 2026-06-19)**. Cadence is a breaking release roughly every 3 months; still pre-1.0 ("A new version of Bevy containing breaking changes to the API is released approximately once every 3 months"). WebGL2 is the safe default; WebGPU is opt-in and gated. Headless server uses MinimalPlugins + a fixed-tick run loop. BSN scene format has no first-party .bsn file loader yet (Claude/Gemini correct) — the content-bundle loader must be custom.

4. **WASM binary size.** *Confidence: inferred; flagged as measurement gap.* Optimized non-trivial Bevy WASM apps land in the low-single-digit-MB to high-single-digit-MB compressed range after wasm-opt + brotli; Bevy's fox demo was ~22 MB uncompressed. Gemini's "1.8-2.5 MB" is optimistic for a full app. Gemini's "100 MB+" figure applies specifically to unoptimized dev builds — corroborated by the Extreme Bevy tutorial (Johan Helsing Studio), which reports its wasm-server-runner dev build verbatim as "uncompressed wasm output is 160.01mb in size." A reliable optimized+brotli figure for a comparable app was not independently confirmed in this pass, so **treat final size as a measurement gap to instrument in the slice**; budget conservatively.

5. **Threads / COOP-COEP: NO-GO at launch.** *Confidence: established.* SharedArrayBuffer/WASM threads require `COOP: same-origin` + `COEP: require-corp`, which severs `window.opener` and breaks OAuth sign-in and payment-checkout popups — both mandatory for Mode 3. `COOP: restrict-properties` (preserves postMessage/closed) exists but is Chrome-only. Gemini's "enable threads" recommendation is rejected.

6. **Symmetric-NAT / STUN-only failure: plan 15-25%.** *Confidence: inferred.* callstats.io telemetry ("WebRTC – an analytics perspective") reports a TURN split of "No Relay 78% / TURN/UDP 13% / TURN/TCP 7% / TURN/TLS 2%" — i.e. **~22% of calls need relay infrastructure** — and "Call Setup Failures: ~10% of calls fail to setup," with "85% due to NAT/FW." CelloIP telemetry: 15-20% consumer, 40-60% enterprise. Gemini's ">30%" was region-specific; region-neutral planning range is **15-25% of Mode-2 peer connections silently failing STUN-only**.

7. **matchbox dual channels: YES, native.** *Confidence: established.* matchbox_socket "supports both unreliable and reliable data channels, with configurable ordering guarantees and variable packet retransmits," via `add_channel(ChannelConfig::reliable())` / `ChannelConfig::unreliable()`. Works browser + native, ships matchbox_server signaling. DeepSeek's single-channel/fork claim is **false**.

8. **str0m vs webrtc-rs: str0m for native/server.** *Confidence: established.* str0m is sans-IO, lock-free ("no Rc, Mutex, mpsc, Arc"), supports DataChannels, used in production (Lookback SFU). webrtc-rs is async/callback/lock-heavy; a sans-IO `rtc` rewrite exists. Browser(web-sys)↔native(str0m) DataChannel interop is standard WebRTC.

9. **Physics: Avian, low-stakes.** *Confidence: inferred.* Both work on WASM; determinism isn't required here. Avian (successor to bevy_xpbd) is ECS-native and near feature parity; rapier is more mature. Recommend Avian for ECS integration but note the choice is low-stakes.

10. **ed25519 signing cost.** *Confidence: established (native numbers); inferred (WASM).* Per the ed25519-dalek README/docs.rs benchmark on "an Intel Skylake i9-7900X running at 3.30 GHz, without TurboBoost": "Ed25519 signing time: [15.617 us 15.630 us 15.647 us]" and "Ed25519 signature verification time: [45.930 us 45.968 us 46.011 us]" (keypair gen 15.465 us). Batch verification improves per-signature cost (~18µs/sig and dropping with batch size). WASM (single-thread, no AVX2) is several× slower. Always sign the reliable channel; make per-frame state-channel signing configurable and measured.

11. **Mode 3 orchestration: managed session-fleet first, Agones only at scale.** *Confidence: inferred.* Agones idle fleets waste capacity and cluster node scale-up takes minutes; managed fleets start containers sub-second from warm pools with per-second billing. Egress is $0.09-0.12/GB on major clouds. Claude's staged recommendation is correct.

---

## Details by Workstream

### A. Engine & Rendering
Bevy 0.19 (ECS) + wgpu. **Browser deployment = two WASM builds**: (1) WebGPU (`webgpu` feature) for Chromium/modern, (2) WebGL2 (default) fallback. A small JS loader probes `navigator.gpu` and loads the matching bundle. This is forced by Bevy #13168 remaining open. Native uses winit + native wgpu (Vulkan/Metal/DX12). Headless Mode 3 server: `App` with MinimalPlugins (no render/winit) + a fixed-tick scheduler at the sim rate. **Size/cold-load budget**: set a conservative per-tier target and validate empirically; lazy-load assets; techniques: `opt-level="z"/"s"`, `lto="thin"` (lightyear's own release profile uses `lto=true`, `codegen-units=1`), `strip`, `wasm-opt -Oz`, feature pruning, brotli, content-addressed lazy asset fetch. **Flag: unoptimized dev builds can exceed 100 MB (Extreme Bevy's was 160 MB) — final optimized+brotli size must be measured in the slice.** Physics: **Avian** (ECS-native); determinism not needed.

### B. Scripting Runtime (Rhai)
Rhai sandboxed, thin high-level logic only; hot loops stay in Rust systems. Integrate via `bevy_mod_scripting` (Rhai backend) or a direct thin bridge. **Hard sandbox limits** (all first-party Rhai `Engine` methods): `set_max_operations` (CPU ceiling — e.g. 100_000/script-call; a Rhai "operation" ≈ one expression node/loop iteration/function call), `set_max_string_size`, `set_max_array_size`, `set_max_map_size`, plus `set_max_call_levels` for recursion. Exceeding max_operations terminates with `ErrorTerminated`. Hot-reload on file change. **Perf ceiling**: Rhai is an interpreted tree-walker — keep per-frame Rhai work bounded; escalate to Rust systems for anything in the tick hot loop. Escape-hatch ladder: Rhai → registered Rust host functions → native Bevy system → engine release.

### C. Transport
**Browser P2P + signaling: matchbox** (matchbox_socket / bevy_matchbox), which natively provides the exact two-channel design — channel 0 `ChannelConfig::unreliable()` (30-60Hz LWW state), channel 1 `ChannelConfig::reliable()` (durable events/handoffs/resync) — and ships matchbox_server. **Native/server: str0m** (sans-IO, lock-free, DataChannel, production SFU pedigree). matchbox's native side uses webrtc-rs; for the Mode-3 hub and native peers prefer str0m for its lock-free `&mut self` model. Browser(web-sys)↔native(str0m) DataChannel interop is standard WebRTC. **STUN/TURN per mode**: Mode 1 none; Mode 2 STUN-only (symmetric-NAT peers silently fail, 15-25%, accepted); Mode 3 TURN provided (paid).

### D. State Replication Protocol — THE PIVOTAL VERDICT
**VERDICT (established): custom-build the per-entity authoritative replication protocol on top of raw WebRTC DataChannels; do NOT adopt lightyear or bevy_replicon as the cross-mode backbone.** Evidence both ways: *For reuse* — lightyear 0.26.4 has prediction/rollback, snapshot interpolation, interest management (Rooms), delta compression, priority bandwidth management, and client↔server authority transfer; its 0.26 refactor made replication peer-agnostic ("each local peer can replicate to a remote peer in the exact same way"), which is architecturally aligned. *Against reuse (decisive)* — (a) lightyear defers IO to aeronet and **neither has any WebRTC-DataChannel transport**; both target WebTransport/WebSocket; (b) distributed/peer authority is "still in flux… not properly tested, distributed_authority example is outdated"; (c) bevy_replicon is server→client-only. Building over lightyear would require writing a WebRTC IO layer AND hardening untested distributed authority — more risk than a purpose-built protocol. **Recommended design**: owner computes state, replicates snapshots/deltas on the unreliable channel; receivers apply directly (predict-own, interpolate-others); no re-simulation of others' entities ⇒ no cross-platform determinism needed. **Wire format**: bincode/postcard structs, quantized floats (fixed-point positions, quantized quaternions), per-component delta vs last-acked baseline. **Tick**: 30-60Hz state; fixed sim tick. **AOI/interest management** as a separate spatial layer gating which entities each receiver gets (bandwidth + read-cheat defense in Mode 3). **Ownership transfer**: explicit reliable-channel event; Mode 2 coordinator-arbitrated; failure modes — double-ownership (resolve by coordinator sequence number), orphaned entity on owner drop (reassign via host-migration election). **Cross-owner interaction quality gap**: interactions between two remotely-owned entities are interpolated/laggy; document as accepted. **Anti-entropy resync** (no CRDT): periodic full-state baseline on reliable channel; single-ownership means no merge — last authoritative snapshot wins.

### E. Anti-cheat & Verification
- **Mode 2**: plausibility/bounds checks on incoming state (position deltas, rate limits); ed25519-signed ops (tamper-evident, not cheat-proof). Sign the reliable channel always; per-frame state-channel signing configurable/measured (WASM cost several× the ~15.6µs native sign).
- **Mode 3**: server-authoritative validation + interest management (out-of-view state withheld structurally blocks some read-cheats).
- **Browser attestation ceiling (accept)**: WASM/JS is inspectable; no client attestation possible; Mode 3 "max anti-cheat" = max-achievable-in-browser, weaker than native kernel-AC. Rhai protects the machine from malicious content, not the game from a modified client.

### F. Identity, Accounts, Billing, Persistence
Device keypair per install: browser via WebCrypto (non-extractable key) persisted in IndexedDB; native via OS keyring. Mode 2 signs ops with device key. Mode 3 requires authenticated account (OAuth) + billing; use a hosted payment provider so raw card data never touches own systems. Persistence: Postgres (identity, billing, published content, rankings, match records) + object storage (content-addressed assets/snapshots). Session state ephemeral; opt-in content-addressed save snapshot.

### G. Publish Pipeline & Versioning
Central chokepoint; immutable content-addressed IDs (hash of bundle). **Bundle format**: because no first-party BSN loader exists, define a custom content-bundle (Rhai + assets + scene data) with a custom Bevy AssetLoader. Enforce the {engine, content, schema} version triple; matchmaker filters by version and gates join (no force-update of clients). **UGC moderation** at publish (the sole vantage — P2P sessions can't be moderated live): automated scan (asset hashing against known-bad, static Rhai analysis, text/emoji-name filters) + human report queue. Realistic split: automation catches known-bad and policy-violating metadata; novel/contextual abuse needs the human queue.

### H. Hot Update & Distribution
Content (Rhai + assets + scene data) hot-reloadable at runtime both targets. Engine/binary is a versioned release, NOT hot-reloadable in production: browser reloads WASM (service worker cache-bust); native auto-updates + relaunch. Native distributed as plain signed executables (code-signed, NOT a webview wrapper). **Version-gating on session join is the desync defense.**

### I. Session/Matchmaking/Coordination Service
Central WebSocket service (required even for free tiers): SDP/ICE signaling + session registry + mode/version filtering + matchmaking (same-mode, same-version only). matchbox_server provides the signaling primitive (room-based, `?next=N` crude matchmaking) — extend it for mode/version scoping. Mode 2 coordinator peer holds bookkeeping only; host migration by oldest-survivor join-order election. Horizontal scaling: stateless signaling nodes behind a load balancer; session registry in Redis/Postgres.

### J. Mode 3 Orchestration & Infrastructure
Per-session headless Bevy sim running the same Rhai logic. **Orchestration verdict (inferred): start with a managed session-fleet / lightweight process-pool with per-second billing and warm-pool sub-second cold-start; adopt Agones only at scale** when you have Kubernetes expertise and steady high concurrency to amortize idle-fleet waste and multi-minute node scale-up. TURN provisioned paid-only (coturn self-hosted is the cost-efficient baseline). **Per-concurrent-session cost model (region-neutral, generic worked example)**: cost ≈ (vCPU-hours × rate) + (egress GB × ~$0.10/GB). Subscription price must exceed per-concurrent-session compute + egress + TURN relay share; payment also provides Sybil resistance.

### K. Cross-Cutting WASM/Browser Constraints
- **Threads**: no-go at launch (COOP/COEP breaks OAuth/payment). Revisit only if a Chrome-only `restrict-properties` path or credentialless-COEP + proxied assets proves viable AND threading materially helps.
- **Memory**: wasm32 is limited to a 4 GB linear-memory address space; memory64 exists but is not universally available/performant in browsers — budget within 4 GB; stream assets.
- **Persistent storage/eviction**: IndexedDB/Cache subject to browser eviction under storage pressure; request persistent storage; treat local caches as evictable, re-fetchable from content-addressed store.
- **Cold-load synthesis**: WASM (two-tier) + wasm-bindgen glue + lazy assets; measure end-to-end time-to-interactive in the slice (dev builds can be 100 MB+; optimized+brotli size TBD).

### L. Security & Trust-Model Synthesis
- **Mode 1 Standalone**: local authority, nothing networked; trust = the local machine only; prevents nothing (no adversary), free.
- **Mode 2 Hybrid**: each peer trusted only for its own entities; plausibility/bounds + signed ops **detect** tampering (tamper-evident) but **cannot prevent** a modified client; hidden-info/Sybil/collusion cheats **unaddressable** without authority; STUN-only ⇒ symmetric-NAT peers excluded.
- **Mode 3 Full-Server**: server authoritative over all entities; server-side validation **prevents** state forgery; interest management **structurally blocks** some read-cheats; browser client still un-attestable ⇒ input-level cheats (aim assist) only partially mitigable; paid ⇒ Sybil-resistant.
- **Residual surface incl. UGC**: malicious Rhai content (mitigated by sandbox limits, not eliminated); publish-time moderation gaps; signaling-server DoS; TURN abuse.

---

## Recommended Stack Table

| Layer | Crate/Service | Version & date | Maturity evidence (primary) | WASM | Native | Justification | Confidence |
|---|---|---|---|---|---|---|---|
| Engine/ECS | Bevy | 0.19.0 (2026-06-19) | crates.io/docs.rs version history | ✅ (2 builds) | ✅ | Fixed decision; ECS + wgpu | established |
| Render | wgpu (via Bevy) | Bevy-vendored | Bevy #13168 open | WebGPU+WebGL2 | Vulkan/Metal/DX12 | Two-build fallback | established |
| Scripting | Rhai (via bevy_mod_scripting) | current | rhai.rs safety docs; BMS repo | ✅ | ✅ | Sandbox limits first-party | established |
| Browser transport | matchbox_socket / bevy_matchbox | 0.13.0 | docs.rs; dual-channel API | ✅ | ✅ | Raw WebRTC-DC, dual channels, signaling server | established |
| Native/server WebRTC | str0m | current | lock-free sans-IO; Lookback prod | ❌ | ✅ | Lock-free hub/peer | established |
| Replication | **custom** (over matchbox/str0m) | n/a | subagent-confirmed gap | ✅ | ✅ | No crate does tri-mode over WebRTC-DC | established |
| Physics | Avian | current | ECS-native; near parity | ✅ | ✅ | Determinism not needed | inferred |
| Signing | ed25519-dalek | current | docs benchmarks | ✅ (slower) | ✅ | Tamper-evident ops | established |
| Persistence | Postgres + object storage | n/a | standard | — | — | Durable-authoritative | established |
| Mode 3 orchestration | Managed session-fleet → Agones at scale | n/a | Agones docs; managed-fleet vendors | — | ✅ | Sub-second cold start, per-sec billing | inferred |

---

## Risk Register

| Risk | Likelihood | Severity | Mitigation | Confidence |
|---|---|---|---|---|
| Custom netcode underestimated (biggest risk) | High | High | Vertical slice first; scope tight; borrow lightyear patterns | established |
| Bevy pre-1.0 breaking changes every ~3mo | High | Medium | Pin versions; budget migration each cycle | established |
| WASM size > budget → slow cold load | Medium | Medium | Measure in slice; wasm-opt/brotli/lazy assets | inferred |
| 15-25% Mode-2 peers fail STUN-only | High | Medium | Documented as accepted; offer Mode 3 | inferred |
| matchbox/str0m are small-team OSS | Medium | Medium | Vendor/fork readiness; abstraction layer | established |
| WASM crypto too slow for per-frame signing | Medium | Low | Sign reliable channel only; configurable | inferred |
| Mode 3 infra cost > subscription | Medium | High | Per-session cost model; managed fleet | inferred |
| UGC moderation gaps (P2P unmoderatable live) | Medium | High | Publish-time scan + report queue | established |

---

## Phased Build Sequence — VERTICAL SLICE FIRST
1. **Slice (proves the thesis)**: one ownership-explicit Bevy+Rhai mini-game. Two browser peers over a WebRTC DataChannel (matchbox), each authoritative over its own entities, replicating snapshots (Mode 2). Run the SAME sim headless-authoritative (Mode 3) — proving swapping ONLY authority assignment yields both modes with no logic fork. Deliberately exercise one A→B ownership handoff mid-session. Instrument WASM size + cold-load.
2. Add STUN/TURN, signaling/matchmaking service, version-gating.
3. Add Mode 1; publish pipeline + content-addressing + version triple.
4. Identity/accounts/billing (OAuth + payment provider), Postgres/object storage.
5. Mode 3 orchestration (managed fleet) + interest management + server validation.
6. Native path (winit, auto-update, code-signing, native headless host via str0m).
7. Moderation tooling, T&S (emoji-only, rate limits, mute/block), hardening.

---

## Compromise Ledger (browser-forced compromises)

| Compromise | Quantified cost | Recommended stance |
|---|---|---|
| Two WASM builds not one | 2× build/CI + branch logic; per-tier build | Accept; JS capability detection |
| No threads (COOP/COEP off) | Single-thread stutter; lose SharedArrayBuffer | Accept at launch; OAuth/payment mandatory |
| WebGL2 fallback perf | Lower fidelity/throughput vs WebGPU | Accept; native is the perf tier |
| STUN-only Mode 2 failures | 15-25% peers excluded | Accept; upsell Mode 3 (TURN) |
| WASM 4GB memory cap | Asset streaming required | Accept; memory64 immature |
| Slower WASM crypto | Per-frame signing several× native ~15.6µs | Sign reliable channel; make state-sign configurable |
| Un-attestable browser client | Mode 3 AC weaker than kernel-AC | Accept; document ceiling |

---

## Corrections to the Prior Reports
- **DeepSeek/ChatGPT — lightyear "0.17 / not a design goal":** stale. Current is lightyear 0.26.4 (June 2026). (crates.io/docs.rs)
- **Grok — "lightyear ~0.28 can back all three modes over WebRTC-DC by swapping authority":** false. lightyear defers IO to aeronet ("Instead of maintaining my own steam, webtransport, websocket io layers, I now defer to the excellent aeronet crate!"), which has NO WebRTC-DataChannel layer; distributed authority is untested and the distributed_authority example is "outdated." (lightyear README/releases; aeronet repo)
- **DeepSeek/ChatGPT/Gemini — "wgpu runtime selection ⇒ one WASM binary auto-picks WebGPU/WebGL2":** false for Bevy. wgpu #5044 added the capability, but Bevy issue #13168 is still OPEN; enabling `webgpu` overrides `webgl2` and won't run on non-WebGPU browsers. (Bevy #13168, Bevy examples README)
- **DeepSeek — "matchbox is single-channel, needs a fork":** false. matchbox supports dual reliable+unreliable channels natively. (matchbox docs.rs)
- **DeepSeek (Bevy 0.15) / version drift:** current Bevy is 0.19.0 (2026-06-19). (docs.rs)
- **Gemini — WASM "1.8-2.5 MB" full app / "enable COOP-COEP threads":** size is optimistic; unoptimized dev builds are 100 MB+ (Extreme Bevy's was 160.01 MB); COOP/COEP would break required OAuth/payment popups. (Extreme Bevy tutorial; MDN/web.dev COOP-COEP)
- **Gemini — ">30% symmetric-NAT":** region-specific; region-neutral is ~15-25% (callstats.io ~22% need relay infrastructure; ~10% call-setup failure, 85% NAT/FW; CelloIP 15-20% consumer).

*Every recommendation labeled established/inferred/speculative above. Measurement gaps (optimized WASM size, cold-load, WASM crypto per-frame cost, actual STUN-only failure rate for your population) must be instrumented in the vertical slice.*