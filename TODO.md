# TODO.md — uniblox

## What this is

uniblox is a browser-first, native-secondary Rust→WASM **platform** (not a framework) for user-generated-content (UGC) multiplayer games, with tri-mode WebRTC networking. Its one novel, load-bearing idea is the **authority-swap**: a single per-entity authoritative state-replication mechanism where only the authority *assignment* varies across Standalone (Mode 1), P2P-Hybrid (Mode 2), and Full-Server (Mode 3) — the same simulation runs in all three modes with **NO logic fork**. The engine is **Bevy 0.19 ECS on wgpu**; UGC logic runs in **sandboxed Rhai**; transport is **WebRTC DataChannels only** (matchbox in-browser, str0m native/server). Build Phase 1 — the vertical slice that proves the authority-swap — **FIRST**.

**Reference class:** Roblox / Rec Room / Core (UGC multiplayer platforms) and PICO-8 / TIC-80 (constrained creation). This is a platform whose value is the creation-and-play loop, not an engine you ship to developers.

> **Naming:** "uniblox" is the committed product / crate / repo name and **supersedes the earlier open naming decision** (CONTEXT.md §7, which left the name unresolved between candidates such as `skeinia` — which cleared .com + crates.io + GitHub — and `manifld` — stronger but failed .com). **Before first public publish, confirm .com + crates.io + GitHub availability for "uniblox" and complete the trademark screen** — that screening gate was open in the source docs and must be closed here.

## How to use this file

- **Drive this file top-to-bottom.** Phase 1 (the vertical slice) is COMPLETE — it proved the authority-swap (ADR-0014) and the client renders with all slice measurements taken (ADR-0017). Phases 2+ follow in order, fanning out per the staged next steps at the bottom.
- **Why slice-first (kept as rationale):** roughly **10% of the system is novel and 90% is well-trodden**, and ~80% of the value sits in the ~15% authority-swap slice. That slice was simultaneously the highest-risk and highest-value part; everything else is delegable glue now that it is proven.
- **Obey each task's risk tier.** HIGH-RISK tasks are human-verified, TDD-first, plan-mode-first, with a fresh independent auditor subagent mandatory. LOW/MIXED tasks are delegated heavily, with acceptance criteria as the contract.
- **The architecture is settled. Record it; do not relitigate it.** Breaking a settled invariant silently re-opens a decision already made for a reason. Rejected alternatives are logged below so they are not re-proposed.
- See **docs/CONTEXT.md** for the full "why" (rationale, trust envelope, rejected-alternative reasoning) and **docs/final-buildspec.md** for the technical verdicts.

## AI-assisted engineering workflow (Claude Code operating manual)

This project is built primarily by Claude Code sessions under human supervision. The mechanisms below exist to make LLM-authored netcode/sandbox code trustworthy. (Reference — live status in `PROJECT_STATE.md`.)

**Subagents — four, with tool restrictions:**
- **test-writer** (Read, Write, Edit, Bash) — writes tests FIRST; for HIGH-RISK areas the human specifies the cases.
- **netcode-auditor** (read-only) — fresh independent review of replication/authority/handoff diffs; never the session that wrote the code.
- **sandbox-auditor** (read-only) — adversarial review of the Rhai sandbox surface and resource limits.
- **reviewer** (read-only) — general read-only diff review before merge.

**Hooks:**
- `cargo fmt` on write (PostToolUse).
- Blocking `cargo clippy --all-targets -- -D warnings` + `cargo test` gate.
- **PreToolUse deny** of destructive commands (`rm -rf`, force push, `DROP TABLE`).
- Hook that **blocks edits to `tests/` during implementation turns** — guards the agent from editing tests to make them pass.

**MCP servers:** GitHub; read-only Postgres; a docs/context (Context7-style) server to counter hallucinated Bevy/matchbox/str0m APIs; Playwright (browser E2E). *Scaffolded in `.mcp.json` (node/npx via the flake, routed through `direnv exec`). Reachability is the remaining task: it needs a running read-only Postgres role (see Phase 7), a GitHub PAT in `.claude/settings.local.json` (never in the tracked `.mcp.json`), and Playwright browsers; `docs` should be reachable on the flake alone.*

**Slash commands:** `/build-wasm` (two-build pipeline), `/slice-check` (print the instrumentation table), `/review-netcode` (invoke netcode-auditor), `/new-crate` (scaffold a workspace crate).

**Headless / CI:** `claude -p` for headless runs; a locked-down `dontAsk`/no-prompt mode; a GitHub Action that does first-pass work only (human merges); `--bare` for scripted output.

**Context management:** `PROJECT_STATE.md` (current status), `DECISIONS.md` / ADR log (why), per-crate `CLAUDE.md`; `/compact` + `/clear` between phases; an Explore subagent for read-only codebase spelunking.

**Git / PR workflow:** parallel git **worktrees** for concurrent phases; **commit tests before implementation** so `git diff tests/` across the implementation commit proves the tests were not tampered with.

**Known failure modes to watch for:** hallucinated or version-drifted APIs (Bevy/matchbox move fast — verify against the docs MCP, never memory); papering over errors (`.clone()`/`unsafe`/`.unwrap()` to make it compile); scope creep beyond the approved plan; self-review softness (a session rating its own code as fine) — which is why HIGH-RISK diffs go to a fresh auditor.

## Settled invariants (do NOT break)

- **Single-ownership per entity.** Exactly one authority per entity is the precondition for last-write-wins replication and for "no CRDT." Default ownership = the entity's spawner/controller.
- **No CRDT in the runtime.** Single-ownership means no concurrent writes to the same datum, so there is nothing to merge — CRDT was overhead pretending to be the sync layer. (Permitted only in the separable collaborative-editing subsystem of the authoring tool.)
- **No cross-platform float determinism.** Receivers apply and interpolate others' replicated state rather than re-simulating it, and prediction only touches entities you own — so no two machines ever have to agree on a float result.
- **WebRTC DataChannels only — no media, no SFU, anywhere.** Emoji-only social means no voice/video path; Mode 3 is an authoritative hub, not an SFU relay.
- **Two WASM builds, not one.** Bevy cannot serve WebGPU and WebGL2 from a single binary (issue #13168 open; the `webgpu` feature overrides `webgl2`) — ship two builds plus JS capability detection.
- **Single-threaded WASM at launch (no COOP/COEP).** Cross-origin isolation for SharedArrayBuffer threads severs `window.opener` and breaks the OAuth sign-in and payment-checkout popups Mode 3 requires.
- **Browser persistent storage is evictable.** IndexedDB/Cache are subject to browser eviction under storage pressure — **request persistent storage** and treat all local caches (content bundles, snapshots) as evictable and **re-fetchable from the content-addressed store**. (The device keypair must persist; see Phase 6.)
- **The reliable channel is always signed — from Phase 6 on.** ed25519 op-signing makes durable events tamper-evident. This depends on the device keypair and is a **Phase 6 deliverable, NOT part of the Phase 1 slice**; the slice only reserves the signature field and measures sign/verify cost.
- **Custom replication, not lightyear/replicon/renet.** No existing Rust crate backs all three modes over WebRTC DataChannels by varying only authority (lightyear defers IO to aeronet, which has no WebRTC-DataChannel layer; bevy_replicon is server→client-only).
- **Thin custom Rhai bridge, not `bevy_mod_scripting`.** BMS has no WASM support (issue #166), which is disqualifying for a browser-first platform.
- **The Rhai sandbox is not anti-cheat.** It protects the player's machine from malicious content; it does nothing against a modified client, so it must never count as part of the anti-cheat story.
- **Mode 3 is authoritative, and that is what the subscription sells.** If Mode 3 degrades into "just a relay/SFU," the anti-cheat value evaporates and you are charging money for the weaker guarantee.

## Risk-tiering guide

**HIGH-RISK** — plan-mode-first, TDD (human specifies the cases), tight human review, and a fresh independent auditor subagent mandatory. LLM code here compiles but can be subtly wrong or insecure. Applies to:
- The **custom per-entity replication + authority-swap netcode** (concurrency, ownership handoff, double-ownership, orphaned entities, host-migration election, anti-entropy resync). No existing crate does this over WebRTC DataChannels by varying only authority — it is custom and unproven.
- The **Rhai sandbox hardening** (resource limits, whitelisted API surface, no eval/fs/network, sandbox-escape prevention).
- Anything touching **crypto/signing** (ed25519 op signing), **billing/entitlement** boundaries, or **anti-cheat validation**.

**LOW / MIXED** — delegate heavily; acceptance criteria are the contract. High-volume, well-trodden, verifiable by compiler/tests: boilerplate and glue, ECS component/system scaffolding, platform/backend services (Postgres schema + migrations, OAuth wiring, billing-provider integration, signaling/matchmaking service, publish pipeline), build tooling and the two-WASM-build pipeline, CI, size instrumentation, and tests (agent writes them; for HIGH-RISK areas the human specifies the cases). **MIXED** (our convention) = split the task: the risky core is HIGH, the wiring is LOW.

**Always-do rules:**
- **Clippy gate:** `cargo clippy --all-targets -- -D warnings` must pass (blocking gate); run after major changes.
- **No `unwrap()`/`expect()`** in non-test code.
- **No new `unsafe`** without a `// SAFETY:` comment and human sign-off.
- **Never paper over an error** with `.clone()` or `unsafe` — fix the root cause.
- **TDD + a fresh independent auditor subagent** for all HIGH-RISK code; never let the session that wrote the code audit its own work.
- **Plan-mode-first** for every HIGH-RISK task and any change touching 3+ files; if reality diverges from the approved plan, return to plan mode rather than improvising.
- Remember: **"compiles but subtly wrong" is the dominant netcode risk** — neither the compiler nor clippy catch it; that is what TDD, the netcode-auditor, human review, and deterministic replay tests exist to catch.

## Workspace layout (settled)

Cargo workspace, multi-crate:
- `protocol` — shared types: versions, messages, content IDs.
- `replication` — the custom protocol: wire format, quantization, delta/baseline, authority, handoff, resync. **[HIGH-RISK]**
- `transport` — matchbox (browser) + str0m (native/server) abstraction; two-channel config.
- `scripting` — Rhai engine, sandbox, ECS bridge. **[HIGH-RISK — sandbox]**
- `engine-core` — Bevy setup, shared systems, ECS components.
- `client` — WASM/native client (winit + wgpu).
- `server` — headless authoritative Bevy sim (MinimalPlugins + fixed tick).
- `services` — signaling / session-registry / matchmaking WebSocket service.
- `platform` — Postgres / identity / billing / publish / moderation backend.

## Recommended stack

| Layer | Crate/Service | Version (if given) | WASM | Native | Justification |
|---|---|---|---|---|---|
| Engine/ECS | Bevy | 0.19.0 (2026-06-19) | ✅ (2 builds) | ✅ | Fixed decision; ECS + wgpu |
| Render | wgpu (via Bevy) | Bevy-vendored | WebGPU + WebGL2 | Vulkan/Metal/DX12 | Two-build fallback (Bevy #13168 open) |
| Scripting | Rhai (thin custom bridge, not bevy_mod_scripting) | 1.25 (pinned) | ✅ | ✅ | First-party sandbox limits; BMS lacks WASM support |
| Browser transport | matchbox_socket | 0.14 (pinned) | ✅ | ✅ | Raw WebRTC-DataChannel, dual channels, ships signaling server |
| Native/server WebRTC | str0m | 0.21 (pinned, rust-crypto) | ❌ | ✅ | Lock-free sans-IO hub/peer |
| Replication | **custom** (over matchbox/str0m) | n/a | ✅ | ✅ | No crate does tri-mode over WebRTC-DC |
| Physics | Avian **or** bevy_rapier | current | ✅ | ✅ | ECS-native; determinism not needed (rapier is more mature) |
| Signing | ed25519-dalek | 3 (pinned) | ✅ (slower) | ✅ | Tamper-evident ops |
| Persistence | Postgres + object storage | n/a | — | — | Durable-authoritative store |
| Mode 3 orchestration | Managed session-fleet → Agones at scale | n/a | — | ✅ | Sub-second cold start, per-second billing |

> **Remaining pins to verify when they land:** the full Bevy engine (client work) and the physics crate (Avian vs bevy_rapier) — the netcode/scripting pins above are locked in the workspace `Cargo.toml`. The Bevy ecosystem moves fast (release cadence ~3 months, pre-1.0): re-verify the load-bearing facts above (issue #13168 open; lightyear/aeronet lacking a WebRTC-DataChannel layer; bevy_replicon server→client-only; BMS issue #166; in-browser ed25519 cost; STUN-only ~15–25% failure) at each Bevy upgrade.

## Corrections to prior reports (do NOT re-propose)

The five source reports contained stale/incorrect claims that were refuted during synthesis. Recorded so they are not re-introduced:
- **lightyear "0.17" / lightyear as the netcode backbone** — refuted; lightyear defers IO to aeronet, which has no WebRTC-DataChannel layer. Custom protocol instead.
- **A single WASM binary with a wgpu WebGL2 fallback** — refuted; Bevy cannot serve WebGPU and WebGL2 from one binary (#13168). Two builds.
- **Single-channel matchbox** — refuted; matchbox supports multiple DataChannels; we use two (unreliable state + reliable events).
- **NAT/STUN failure ">30%"** — corrected to ~15–25% (some environments up to ~30%).
- **COOP/COEP SharedArrayBuffer threads at launch** — refuted; cross-origin isolation breaks the OAuth/payment popups Mode 3 needs. Single-threaded at launch.

## Open questions register (measure/decide before locking)

Genuinely open technical unknowns (CONTEXT.md §9). None block starting the slice, but each must be resolved before the dependent decision is locked:
- **Remaining crate pins** — the full Bevy engine (when the client lands) and the physics crate (Avian vs bevy_rapier); verify compatibility then. (The netcode/scripting pins are locked in `Cargo.toml`.)
- **Rhai performance escape-hatch trigger** — what measured cost (per-tick script time / ops budget) triggers stepping down the ladder (see Phase 12). Undecided.
- **Network-tick default** — ~20–30 Hz is an **assumed, unmeasured starting point** (client interpolates to display rate); buildspec §C/§D cites up to 30–60 Hz. Measure in the slice and pick.
- **Full-mesh session cap** — **~8 peers** is a soft, upstream-bandwidth-bound assumption, not a measured limit. Measure.
- **Default ownership rule** — assumed default is "entity is owned by its spawner/controller." Confirm it holds across handoff/migration.

**Commercial open questions (CONTEXT.md §5 — these gate whether to build at all, not how):** the specific wedge / first audience; cold-start content seeding; whether to pursue competitive ambition (which would reopen the anti-cheat model); and the end goal (sell / run / raise). Resolve at the product level before heavy infra spend.

## Measurement gaps to instrument (tie to Phase 1)

Unknowns to **measure, not assume** — build instrumentation into Phase 1 and re-report every phase.

- **WASM binary size** per build (WebGPU vs WebGL2), **before and after `wasm-opt -Oz` + brotli** — measured baseline 2026-07-11 (ADR-0017, 2d-pruned, no assets yet): raw ~21 MB → wasm-opt ~15.6 MB → **brotli 3.38/3.40 MB**. Re-report per release as assets/features land; budget within the wasm32 4 GB memory ceiling.
- **Cold-load time** in-browser: download + instantiate + first frame (time-to-interactive).
- **Replication bandwidth per peer** — the pre-delta native baseline is in `/slice-check` (742 B/s @ 2 entities, 20 Hz); the delta win is proven DETERMINISTICALLY (ADR-0020 two-World T35: 0 steady-state bytes for a confirmed stationary scene) and AOI now withholds out-of-range entities entirely (ADR-0021). Still to measure over the real transport: the post-delta/post-AOI B/s under motion with a real per-client focus, whether 30–60 Hz is affordable, and realistic entity counts.
- **In-browser ed25519 sign/verify cost** (via WebCrypto/WASM) — decides whether per-frame state-channel signing is affordable vs reliable-channel-only. (Native measured 13.4 µs sign / 25.7 µs verify with the opt-level=3 crypto override; WASM single-thread, no AVX2, several× slower — measure it.)
- **STUN-only connection failure / P2P connection-success rate** — fraction of peers behind symmetric NAT / restrictive firewalls requiring TURN (est. ~15–25%, some up to ~30%). Silent failure accepted on free tiers; Mode 3 provides TURN.
- Use **`twiggy`** to record per-function/section byte counts per release (not just total file size) so WASM-size regressions can be attributed to specific code, paired with the wasm-opt/brotli totals. *Acceptance:* `twiggy top` output is captured alongside the size table each release.

## Phased build sequence

### PHASE 3 — Replication depth [HIGH]
**Goal:** turn the slice's replication into a robust layer. Every task: plan-mode + netcode-auditor + TDD + deterministic replay tests.
- Replication throughput follow-ups — DEFERRED (ADR-0029; revisit ONLY when a MEASURED dense-Mode-3 workload needs >1-datagram of entities visible to one peer with existence PRESERVED). The over-MTU RISK is CLOSED by the AOI size-cap (each per-peer state datagram is bounded to `SAFE_DATAGRAM_BYTES` — the nearest entities that fit are kept, overflow routed through the audited AOI-exit path). REMAINING (only if measured): **true message splitting** (fragment one snapshot across datagrams to preserve ALL-entity visibility instead of dropping the farthest) and **per-entity ack granularity** — these are COUPLED: sound splitting needs per-entity/negative acks (the current cumulative-run ack false-confirms a stuck/lost middle fragment), a large ack-model rework (WIRE bump + reassembly buffer replacing the LWW gate). Do it via per-bucket sub-streams (keep the cumulative-run ack sound per bucket), not tick-fragmentation. NOTE: the over-MTU blob is already CORRECT today (higher loss probability, not a bug) and the stuck-entry stall is bandwidth-only + self-heals via resync — hence YAGNI-until-measured. *Acceptance (if revisited):* an over-budget snapshot reaches every in-AOI entity across datagrams with no false-confirm under loss/reorder; a stuck entry stalls only itself.

### PHASE 4 — Mode 1 Standalone [LOW]
**Goal:** free, local-authority, no networking, no anti-cheat. Scope (planning session): runtime = headless + browser-playable; save persists via browser IndexedDB. The runtime tier landed — **ADR-0030** added the net-free `standalone` crate (`build_standalone_app`, no transport/replication in its crate graph; a `cargo tree` guard enforces it), closing the "runs with the networking stack absent" acceptance headlessly; **ADR-0031** wired that net-free sim into the client's `DefaultPlugins` render app (playable Mode 1: `Camera2d` + keyboard `Avatar` + drifting NPCs; input→`Velocity`; `Position`→`Transform`; size gate re-checked → PASS 3.39/3.41 MB). NOTE: the live in-browser render + keyboard was not machine-verifiable in the build environment (WSL dev-server teardown + no-GPU in-app browser) — do a one-off manual browser check (`scripts/serve.sh` + open dist/: scene renders, NPCs drift, arrow/WASD moves the avatar).
- **Opt-in content-addressed save (B1–B4 + C1):** B1 `ContentId`+blake3 in `protocol`; B2 `persistence` crate (`SaveBlob` DTO codec + `save_world`/`load_world`/`load_world_verified` + `MemoryStore`); B3 native `FileStore`; B4 browser `IdbStore` (IndexedDB, async — durable browser save); C1 opt-in save/load keybind wired into the playable client. *Acceptance:* save/reload by content ID round-trips (in-memory + native file), and a browser Mode-1 save survives a page reload (IndexedDB).

### PHASE 5 — Central services (signaling / session / matchmaking) [LOW]
**Goal:** the WebSocket service required even for free tiers. Delegate heavily. (Extend `crates/services`' full-mesh signaling with a custom `SignalingTopology` for `?next=N` matchmaking + mode/version scoping — see ADR-0012.)
- SDP/ICE signaling + session registry, extending `matchbox_server`'s room-based `?next=N` matchmaking with mode/version scoping. *Acceptance:* peers exchange offers/answers; sessions listed; scoping enforced.
- Matchmaking groups only same-mode, same-version players. *Acceptance:* mismatched version/mode never matched.
- Version-triple enforcement/gating (`{engine, content, schema}`); gate join, no force-update. Version-gating on session join is the desync defense (engine/binary is a versioned release, not hot-reloadable in prod; content is hot-reloadable at runtime). *Acceptance:* incompatible client rejected at join with a clear reason.
- Make the version-triple filter **asymmetric**: admit if client **engine >= the game's declared minimum** (engine releases are backward-compatible), but require **content ID and schema version to match exactly**. *Acceptance:* an older-but-compatible engine joins; any content/schema-hash mismatch is rejected.
- Horizontal scale: stateless nodes + Redis/Postgres session registry. *Acceptance:* two nodes share the registry.
- Mode 2 coordinator peer holds bookkeeping only (arbitrates ownership by sequence number — see Phase 3); host migration by oldest-survivor election. **[MIXED — election logic is distributed-edge-case-prone; human-review.]**

### PHASE 6 — Identity + accounts + billing [MIXED]
- Device keypair per install: browser WebCrypto **non-extractable** key in IndexedDB; native OS keyring. This is the keypair the "reliable channel always signed" invariant depends on. **[HIGH for key handling.]** *Acceptance:* key persists; never exportable in browser.
- Mode 2 op signing (`ed25519-dalek`): **always sign the reliable channel** (now that the Phase 6 keypair exists); per-frame state-channel signing configurable and measured. **[HIGH.]** *Acceptance:* tamper-evident ops; verify cost measured; reliable-channel events are signed and verified.
- Evaluate **`ed25519-dalek` batch verification (`verify_batch`)** for per-frame verification — verification, not signing, is the cost center. At ~30 Hz in an 8-peer mesh a peer verifies ~210 sig/s; batching a tick's packets amortizes far below sequential single-verify (native measured 25.7 µs — and ONLY with the opt-level=3 crypto override; the size profile is ~35× slower, a live wasm size-vs-speed tradeoff for this phase), which decides whether per-frame state-channel signing fits the frame budget vs defaulting to reliable-channel-only. *Acceptance:* measured batched verify/sig in-browser is recorded in the instrumentation table.
- OAuth account for Mode 3; billing via a hosted payment provider (raw card data never touches our systems). **[MIXED — entitlement boundary is HIGH; wiring is LOW.]** *Acceptance:* entitlement gates Mode 3 join; no PAN in our systems.

### PHASE 7 — Persistence [LOW]
- Postgres schema: identity, billing, published-content metadata, rankings, match records. *Acceptance:* migrations apply; constraints enforced.
- Object storage, content-addressed (hash = content ID). *Acceptance:* store/fetch by hash; dedupe.
- Session state ephemeral; opt-in content-addressed snapshot. *Acceptance:* snapshot restorable by ID.
- **Request browser persistent storage**; treat IndexedDB/Cache as **evictable under storage pressure** and re-fetchable from the content-addressed store. *Acceptance:* persistent-storage requested; eviction of a content cache triggers transparent re-fetch by content ID.

### PHASE 8 — Publish pipeline + versioning + UGC moderation [MIXED]
**Goal:** the central chokepoint and the *sole* moderation vantage.
- Immutable content-addressed IDs + `{engine, content, schema}` version triple stamped at publish. [LOW] *Acceptance:* republish yields a new ID; triple recorded.
- Custom content-bundle loader (Bevy 0.19 BSN has no first-party file loader yet). [MIXED] *Acceptance:* a bundle (Rhai + assets + scene data) loads and hot-reloads.
- Moderation: automated scan at publish (**asset hashing against a known-bad set, static Rhai analysis, and text/emoji-name filters**) + a **human report queue**. Realistic split: automation catches known-bad assets and policy-violating metadata; **novel/contextual abuse needs the human queue** (P2P sessions can't be moderated live, so publish is the only vantage). [MIXED] *Acceptance:* known-bad hash blocked; a static-analysis flag blocks; name filters applied; reports enqueued; novel-abuse routes to humans.
- Add a mandatory CSAM perceptual/PhotoDNA-style hash-scan pass (e.g. a provider such as Thorn) at publish, as a distinct legally-required category separate from the generic known-bad asset-hash set. *Acceptance:* a known CSAM perceptual-hash match is blocked and routed to the mandated reporting path.
- Malware/polyglot scan uploaded binary assets (glTF, textures, archives) — e.g. clamav — and validate each declared file type against actual content, catching malicious polyglots masquerading as assets (separate from Rhai static analysis, which only covers scripts). *Acceptance:* a polyglot whose real type ≠ declared type is rejected; a known-malware asset is blocked.
- Define the publishable bundle **manifest**: `{engine, content, schema}` triple + entry-point Rhai script + an asset list where **each asset is referenced by its own content hash** (in addition to the bundle-level content ID). Gives the loader a concrete format and enables per-asset object-storage dedup and per-asset known-bad matching (a bad asset reused across bundles is caught once). *Acceptance:* the loader reads the manifest; a shared asset stores once; a known-bad per-asset hash blocks every bundle containing it.

### PHASE 9 — Mode 3 infra / orchestration [MIXED]
- Per-session headless Bevy sim process; managed session-fleet / process-pool with warm-pool **sub-second cold-start** and per-second billing; Agones only at scale. *Acceptance:* session spins up sub-second from the warm pool.
- TURN provisioning (coturn) with paid-only credentials; entitlement-gated. *Acceptance:* only paid sessions get TURN creds.
- **Per-session cost model:** cost ≈ (vCPU-hours × rate) + (egress GB × ~$0.10/GB, range $0.09–0.12) + TURN-relay share; **the Mode 3 subscription must exceed compute + egress + TURN**. Payment also provides Sybil resistance. *Acceptance:* a per-session unit-cost estimate exists and the subscription price clears it.
- Egress-provider guidance for the cost model: hyperscaler egress (~$0.09/GB) can carry a **hidden NAT-gateway per-GB processing fee (~$0.045/GB)**, whereas bandwidth-pooling VPS (e.g. Akamai/Linode, Vultr) bundle multi-TB transfer with low (~$0.005/GB) overage. Prefer bandwidth-pooling hosts for Mode 3 sims **and** TURN relays; watch for NAT-gateway processing charges. **Re-verify live pricing before committing.** *Acceptance:* the per-session unit-cost estimate names its host/egress tier and accounts for (or excludes) hidden NAT-gateway fees.
- Implement paid-only TURN via **coturn's time-limited REST API / shared-secret credential** mechanism (short-TTL credentials minted per authenticated paid session), so free tiers cannot consume relay bandwidth. *Acceptance:* a session issues a time-limited TURN credential that expires; unauthenticated requests get none.
- Capture concrete managed-vs-Agones decision inputs: managed fleets give seconds-to-sub-second cold-start with per-second billing (e.g. Edgegap ~3 s; Gameye ~0.5 s from a warm pool), while self-hosted **Agones** carries ~20–30% idle-fleet waste, a documented worst-case scale-up of ~10–15 min, and ~1 FTE of k8s expertise — start managed, move to Agones only when the managed premium exceeds a dedicated infra hire. *Acceptance:* the managed-vs-self-hosted decision cites these thresholds. **Re-verify vendor numbers before committing.**
- Add an **idle-session teardown** policy (reclaim a headless sim after ~N minutes with no players) and seed capacity planning with per-session density estimates to validate under load (order-of-magnitude ~50–100 MB RAM and ~0.2–0.5 vCPU per session). *Acceptance:* an emptied session is reclaimed after the idle timeout; measured per-session RAM/vCPU is recorded against the cost model.
- Server-authoritative validation + interest management as max-achievable-in-browser anti-cheat. **[HIGH for validation logic.]**

### PHASE 10 — Hot update + native distribution [MIXED]
- Content hot-reload at runtime on both targets. [LOW] *Acceptance:* content swaps without engine restart.
- Engine/binary as a **versioned release** (NOT hot-reloadable in prod): browser reloads WASM via service-worker cache-bust; native auto-update (`self-update` crate) + relaunch. [MIXED] *Acceptance:* new engine version loads on reload/relaunch; version-gating on join defends against desync.
- Native distribution as plain **code-signed executables** (NOT a webview wrapper). [MIXED] *Acceptance:* signed binaries for target OSes.

### PHASE 11 — Anti-cheat hardening [HIGH]
- Mode 2: plausibility/bounds checks on incoming state + signed ops (tamper-evident, **not** cheat-proof). *Acceptance:* out-of-bounds state rejected; forged ops detected.
- Enumerate the Mode 2 plausibility checks on incoming non-owned state as concrete, testable rules: (1) per-game speed cap; (2) teleport detection (position delta vs elapsed time exceeding max speed); (3) acceleration/jerk limits; (4) per-action rate limiting. *Acceptance:* each check has a threshold and a unit test that rejects a violating packet.
- Mode 3: server-authoritative validation + interest-management read-cheat blocking. *Acceptance:* out-of-view state withheld; invalid client input rejected.
- Track residual **operational** attack surface (distinct from the structural ceilings below): **signaling-server DoS** (rate-limit and authenticate room creation/join) and **TURN abuse** (paid-only credentials + quotas). *Acceptance:* signaling has rate limits; TURN creds are entitlement-gated and quota-limited.
- Document accepted structural ceilings and the trust envelope (do NOT try to "fix" these):
  - **Free-tier peer verification detects inconsistency but cannot attribute blame** (two disagreeing peers are a standoff). Escaping that needs N-player quorum, which has two defeats you cannot close without a server: **Sybil** (anonymous free identity lets one attacker run many fake peers and become the majority) and **collusion** (a cheating clique outvotes honest players).
  - **Hidden-information cheats are unenforceable in P2P:** every peer must hold enough world state to verify, so any cheat that only *reads* legitimately-held data (wallhack, maphack, aimbot, seeing positions) is undetectable because the cheater never lies.
  - **The design is sound only inside this envelope:** low-stakes; casual/creative/co-op games; small sessions (full-mesh caps at ~8 peers, soft/upstream-bandwidth-bound); no hidden information; no real-money economy (subscription-only).
  - Inside the envelope, free-tier anti-cheat is honestly **cost-imposition, not prevention** — most players won't bother, and the few who do can only grief a single session. It **does not survive contact with competitive play**; competitive integrity is gated into paid Mode 3, where an authoritative server supplies ground truth and payment supplies Sybil resistance.
  - **Browser clients are unattestable** (WASM/JS inspectable and modifiable, no secure attestation), so Mode 3 "max anti-cheat" = *max achievable in a browser* (server-authoritative sim), structurally weaker than native kernel-level anti-cheat.
  - **The Rhai sandbox protects the machine, not the game** — it stops malicious *content* from harming a player's *machine*; it does nothing against a *modified client*. Orthogonal problems.

### PHASE 12 — Rhai sandbox hardening [HIGH — security-critical]
**Goal:** the security pass protecting the machine from malicious content. Plan-mode + sandbox-auditor + adversarial tests mandatory.
- Hard resource limits finalized and tested adversarially: max operations, call depth, string/array/map sizes (Rhai default call depth is 64 release / 8 debug; set explicitly). *Acceptance:* adversarial scripts (deep recursion, huge allocations, tight loops, deeply-nested maps) all terminate with errors.
- Also set `engine.set_max_expr_depths(64, 64)` (bounds expression/statement nesting depth — a DoS surface distinct from call depth) and `engine.set_max_modules(N)` (bounds module resolution/imports), beyond the operation/call-depth/size limits already listed. *Acceptance:* a deeply-nested-expression script and an import-bomb script both terminate with errors.
- **Keep Rhai's `unchecked` cargo feature OFF** (directly and transitively) — it compiles out operation counting, depth, and size checks, silently voiding every `set_max_*` limit; keep `internals` OFF so scripts cannot reach engine internals. Add a build/CI assertion that neither feature is in the resolved feature set. *Acceptance:* CI fails if `unchecked`/`internals` is enabled.
- Add a wall-clock watchdog via `engine.on_progress(...)` returning an abort token once an elapsed-time budget is exceeded — the operation counter only bounds interpreted work and cannot catch a blocking whitelisted host function. *Acceptance:* a script that blocks inside a host call is aborted on the time budget, not just the op budget. **(The watchdog must be a gating prerequisite before engine-core whitelists ANY host fn that can block or do non-trivial work — flagged by the Phase-1 sandbox-auditor. Note the wasm32 gotcha: `Instant::now()` panics on wasm — use `web-time` or an op-count budget.)**
- Bound the two DoS surfaces the op counter misses (surfaced by the Phase-1 sandbox-auditor): (a) `set_max_operations` is **per-`call_fn`** (per-entity, per-tick), so a script can lawfully burn the cap × N entities × tick-rate — add a **per-frame aggregate op/time budget** to cap frame-rate DoS; (b) parsing is not governed by `max_operations` — add a **source-size cap before `engine.compile()`** (and bound/stream `from_file` rather than `read_to_string` on untrusted content) so a huge-but-shallow script isn't a parse-time CPU/RAM DoS at content load. *Acceptance:* an oversized script is rejected before compile; a per-frame op/time budget caps aggregate script cost across all entities.
- Anchor the (open) escape-hatch trigger with data: Rhai is a tree-walking AST interpreter (~2× slower than Python 3 on tight loops), so spatial queries, raycasting, heavy math, and broad per-entity ECS iteration must stay out of Rhai. Use **measured script time exceeding ~10–20% of the frame budget** as the starting trigger to step down the ladder, to validate in the slice. *Acceptance:* the trigger threshold is instrumented and revisited with measured data.
- Whitelisted API surface only via the minimal engine (`new_raw` + explicit `register_*`); no eval, no filesystem, no network. *Acceptance:* enumerated allowed calls; everything else rejected.
- Keep scripts **thin** (high-level logic only); hot loops stay in Rust/Bevy systems. Define the **performance escape-hatch ladder** for a script that is too slow: **Rhai → registered Rust host function → native Bevy system → (last resort) a new engine release.** The measured **trigger** for stepping down the ladder is an open question (see Open questions register). *Acceptance:* no per-entity hot path in Rhai; the ladder is documented.
- Fresh `Scope` per invocation where state must not leak. *Acceptance:* no cross-script/session state bleed.
- Document that the sandbox protects the machine from malicious **content**, not the game from a modified **client**.

### PHASE 13 — Social / Trust & Safety [LOW]
- Emoji-only social (no text/voice/media anywhere — hence no SFU/media server in any mode), rate-limited. *Acceptance:* only emoji payloads accepted; rate limit enforced.
- Local mute/block. *Acceptance:* a muted peer's social payloads suppressed client-side.

### PHASE 14 — Native parity + scaling + observability [MIXED]
- **Browser-client audio (single-thread stutter mitigation):** investigate routing audio through a **Web Audio worklet** (runs on its own audio thread, needs no COOP/COEP cross-origin isolation) to decouple audio from main-thread simulation stalls, rather than accepting single-thread audio stutter unconditionally. *Acceptance:* audio does not glitch when the main thread stalls for a frame. (Browser-only; needs a running WASM client with audio.)
- Native parity: same Bevy binary, native winit + native wgpu; native can host the Mode 3 headless authoritative server. *Acceptance:* native client + native-hosted server run the slice.
- Scaling: fleet autoscale; session registry under load. [LOW]
- Observability: metrics/tracing/logging across services; keep reporting the instrumentation metrics in prod. [LOW] *Acceptance:* dashboards for bandwidth, session cold-start, connection success, and cold-load.


### PHASE 1 — THE VERTICAL SLICE: COMPLETE

The slice proved the authority-swap (gate PASSED, ADR-0014; full trail ADR-0011…ADR-0017 in
`DECISIONS.md`, live status in `PROJECT_STATE.md`) and the Bevy client renders in-browser with every
slice measurement taken (ADR-0017: real two-build sizes, cold-load, size-budget gate PASS). The one
measurement the slice could never take — the STUN-only connection success rate — needs real-network
peers and lives in Phase 2's telemetry bullet.

### PHASE 2 — Transport hardening [MIXED]
**Goal:** production-grade transport across browser and native/server.
- STUN-only failure rate + real-network RTT/jitter — the telemetry INSTRUMENT + AGGREGATION are DONE (ADR-0018: `Str0mPeer::telemetry()` records per-peer ICE outcome, winning-candidate kind, and RTT/jitter from str0m's ICE keepalive stats; `FleetMetrics::aggregate` turns many records into the STUN-only success fraction + candidate-kind breakdown + RTT/jitter distribution; hermetic + unit tested; live-demoed). Remaining is purely deploy-gated: the real-network NUMBERS need a fleet behind diverse NATs, and the collect→aggregate→export wiring lands with Phase-14 observability; browser-side candidate-pair classification via `getStats()` is a follow-up (matchbox-wasm doesn't surface it). [LOW] *Acceptance:* a fleet aggregates the telemetry into the STUN-only success fraction + RTT/jitter distributions once real sessions run.


## Risk register

| Risk | Likelihood | Severity | Mitigation |
|---|---|---|---|
| Custom netcode underestimated (biggest risk) | High | High | Vertical slice first; scope tight; borrow lightyear patterns |
| Bevy pre-1.0 breaking changes every ~3mo | High | Medium | Pin versions; budget migration each cycle |
| WASM size > budget → slow cold load | Medium | Medium | Measure in slice; wasm-opt/brotli/lazy assets |
| 15–25% Mode-2 peers fail STUN-only | High | Medium | Documented as accepted; offer Mode 3 |
| matchbox/str0m are small-team OSS | Medium | Medium | Vendor/fork readiness; abstraction layer (`crates/transport`) |
| WASM crypto too slow for per-frame signing | Medium | Low | Sign reliable channel only; configurable |
| Mode 3 infra cost > subscription | Medium | High | Per-session cost model (vCPU + egress + TURN); managed fleet |
| UGC moderation gaps (P2P unmoderatable live) | Medium | High | Publish-time scan (hash/static/name filters) + report queue |
| Signaling DoS / TURN abuse (operational) | Medium | Medium | Rate-limit + authenticate signaling; paid-only TURN creds + quotas |

## Compromise ledger

| Compromise | Quantified cost | Recommended stance |
|---|---|---|
| Two WASM builds not one | 2× build/CI + branch logic; per-tier build | Accept; JS capability detection |
| No threads (COOP/COEP off) | Single-thread stutter; lose SharedArrayBuffer | Accept at launch; OAuth/payment mandatory |
| WebGL2 fallback perf | Lower fidelity/throughput vs WebGPU | Accept; native is the perf tier |
| STUN-only Mode 2 failures | 15–25% peers excluded | Accept; upsell Mode 3 (TURN) |
| WASM 4GB memory cap | Asset streaming required | Accept; memory64 immature |
| Browser storage evictable | Local caches can vanish under storage pressure | Request persistent storage; re-fetch from content-addressed store |
| Slower WASM crypto | Per-frame signing several× native (13.4 µs sign / 25.7 µs verify measured) | Sign reliable channel; make state-sign configurable |
| Cross-owner interactions laggy | Remote-vs-remote interactions interpolated, higher latency | Accept; documented ceiling, do not re-simulate locally |
| Un-attestable browser client | Mode 3 AC weaker than kernel-AC | Accept; document ceiling |

## Rejected alternatives

Compact list; see **docs/CONTEXT.md** for the full rationale.

| Rejected option | Why it lost (brief) |
|---|---|
| CRDT for gameplay sync | Single-ownership ⇒ no concurrent writes ⇒ nothing to merge. Kept only for the collaborative editor. |
| Lockstep / input-sync | Makes cross-platform float determinism a correctness requirement (x86/ARM/WASM diverge); perfect-information-only. |
| SFU for Mode 3 | Forwards without validating ⇒ no real anti-cheat; and no media means no SFU needed. ⇒ authoritative headless-Bevy hub. |
| Users author in Rust→WASM | Compile times, toolchain friction, danger of running arbitrary user WASM. ⇒ interpreted, sandboxed Rhai (cf. Roblox/Luau). |
| Fyrox instead of Bevy | Native (not browser-embedded) editor; weaker WASM story — disqualifying for browser-first. |
| Custom wgpu stack instead of Bevy | YAGNI — re-solves ECS/assets/render-graph; revisit only on a concrete Bevy blocker. |
| lightyear / bevy_replicon / renet backbone | lightyear defers IO to aeronet (no WebRTC-DC layer), distributed authority untested; bevy_replicon is server→client-only. ⇒ custom protocol. |
| `bevy_mod_scripting` for the Rhai bridge | No WASM support (issue #166) — disqualifying. ⇒ thin custom bridge. |
| Per-zone authority | Coarser; pushes P2P toward host-authority, contradicting distributed-mesh intent. ⇒ per-entity authority + spatial AOI layer. |
| Authoritative-host-peer for anti-cheat | Discards client-heavy/minimal-server thesis; host is a cheat vector and SPOF. Revisit only if competitive ambition appears. |
| Byzantine-tolerant signed-op CRDT + quorum for Mode 3 | Inherits Sybil/collusion ceiling + determinism tax; caps AC below server-authority. ⇒ authoritative server. |

## Go/no-go gates & staged next steps

**Gates:**
- **Size-budget gate (standing; re-check per release as assets/features land):** ≤ **~8 MB brotli per WASM build**, aiming for a **playable first frame in ~2–5 s on ~5–10 Mbps** links (treat >~1 s first-contentful-paint on a high-end desktop as a signal to prune harder). **Current (2026-07-11, ADR-0017): PASS — 3.38/3.40 MB brotli; first frame ~3.1 s @10 Mbps (in target), ~5.8 s @5 Mbps (marginal — prune further when real game assets land).** If a release breaches the gate, pause and run a size spike (feature-prune, lazy-load assets) before proceeding.
- **Benchmark thresholds that change the plan:** STUN-only failure materially above ~20–25% → prioritize TURN earlier / reconsider free-tier P2P expectations; per-frame sign/verify cost too high in-browser → default to reliable-channel-only signing; cold-load unacceptably slow → invest in binary-splitting/lazy assets before feature work.

**Staged next steps:**
1. **Finish Phase 2 (transport hardening)** — reconnect/ICE-restart, plus the environment-gated verifications (desktop-browser pairings, real-network failure-rate telemetry) as environments become available.
2. **Grow the client from the minimal render (ADR-0017) toward the playable mini-game view** — render the replicated entities, wire input, add the `ui` feature collection when needed; re-check the size-budget gate as it grows. The Phase-14 Web Audio worklet is now unblocked (needs audio added to the client first).
3. **Fan out Phases 4–8** (Mode 1, services, identity/billing, persistence, publish) — LOW/MIXED, safe to delegate broadly with acceptance criteria as the contract; run them in parallel git worktrees if using multiple sessions.
4. **Sequence the two HIGH-RISK deep passes (Phase 3 replication depth, Phase 12 sandbox) with a human owner each.** Never let these merge on auto-review alone; each goes to a fresh auditor subagent.
5. **Keep instrumenting the measurement gaps and re-report every phase:** WASM binary size per build (before/after `wasm-opt -Oz` + brotli, within the wasm32 4 GB ceiling); cold-load time in-browser; replication bandwidth per peer post-delta/AOI (and whether 30–60 Hz is affordable); per-message sign/verify cost in-browser (ed25519); STUN-only connection failure rate (plan 15–25%; TURN provided only in Mode 3).
