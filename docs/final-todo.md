# TODO.md — Browser-First Rust/WASM UGC Game Platform (Claude Code build plan)

> A single, self-contained engineering plan and operating manual. Copy this file into your repo root as `TODO.md` and drive it with Claude Code top-to-bottom. Phase 1 (the vertical slice) is built FIRST.

---

## 0. What this is

A **browser-first, native-secondary Rust→WASM user-generated-content game platform** with tri-mode networking. The **core novel piece** is a *single per-entity authoritative state-replication mechanism where only the authority assignment varies by mode* (the "authority-swap"): the same simulation runs Standalone, P2P Hybrid, and Full-Server with **no logic fork**. Engine: Bevy 0.19 (ECS) on wgpu; UGC logic in sandboxed Rhai; transport is WebRTC DataChannels only (matchbox in-browser, str0m native/server).

**How to use this file with Claude Code.** This is both the backlog and the operating manual. Work top-to-bottom; Phase 1 first. For every task, obey its **risk tier** (§2). HIGH-RISK tasks: plan mode → TDD → tight human review → auditor subagent. LOW-RISK tasks: delegate to the agent with the acceptance criteria as the contract. The fixed architecture is **settled** — record it in `CLAUDE.md` and do not relitigate it.

---

## 1. AI-Assisted Engineering Workflow (Claude Code)

All Claude Code capability claims are verified against Anthropic's official docs (`code.claude.com/docs`). Feature availability evolves; version-gated items are labeled and should be re-verified against your installed `claude --version`.

### 1.1 CLAUDE.md / memory structure
Claude Code loads `CLAUDE.md` files by walking from the working directory up to the repo root and concatenating them; nested/subdirectory files load on demand when Claude reads a file in that subtree (`code.claude.com/docs/en/memory`). Three scopes exist: **user** (`~/.claude/CLAUDE.md`), **project** (`./CLAUDE.md`, committed), and **enterprise/managed** (managed policy files cannot be excluded). **Auto memory** — Claude's own notes in the project memory dir, re-injected each session — requires v2.1.59+ and is on by default.

Keep the root `CLAUDE.md` **high-signal and short** (community guidance converges on ~80–120 lines because models reliably follow only a bounded number of simultaneous instructions). Put in it:
- **Build/test/lint commands** (the single highest-value section): exact `cargo` commands, the *two* WASM build invocations (WebGPU + WebGL2), `wasm-bindgen`, `wasm-opt`, and the dev-serve command. Without this, the agent guesses wrong tooling and burns turns.
- **Workspace map** (see §4) and what each crate owns.
- **Settled architecture invariants**: single-ownership per entity; **no CRDT** (single-ownership ⇒ no concurrent writes); **no cross-platform determinism** (receivers never re-simulate others' entities); **WebRTC DataChannels only, no media/SFU anywhere**; two-WASM-build requirement; **single-threaded WASM** (no SharedArrayBuffer/COOP-COEP at launch — it breaks the OAuth + payment popups Mode 3 needs).
- **"Always do" rules**: run `cargo clippy` after major changes; no `unwrap()`/`expect()` in non-test code; no new `unsafe` without a `// SAFETY:` comment and human sign-off; never silence a compiler error with `.clone()` or `unsafe`.
- Use `@path/to/file.md` imports and a `.claude/rules/` topic split (netcode, sandbox, replication wire format) so heavy detail loads only when relevant.

Per-crate `CLAUDE.md`: a short file in each crate directory (e.g., `crates/replication/CLAUDE.md` capturing the wire format, quantization scheme, and baseline/ack rules) loads when Claude touches that crate — this is how you keep large-project context focused.

### 1.2 Custom subagents (`.claude/agents/*.md`)
Subagents run in isolated context windows with their own tool restrictions and system prompt (`code.claude.com/docs/en/sub-agents`). Built-ins: **Explore** (read-only search), **Plan**, **general-purpose**. Define these custom project agents (Markdown + YAML frontmatter; give them job-shaped names and action-oriented descriptions — auto-routing is unreliable, so invoke explicitly):
- **netcode-auditor** *(read-only: Read, Grep, Glob)* — reviews replication/authority/handoff code for concurrency bugs, double-ownership, orphaned entities, last-write-wins violations, missing ack/baseline handling. Structurally cannot edit.
- **sandbox-auditor** *(read-only)* — reviews Rhai integration for resource limits, API-surface whitelisting, and sandbox-escape vectors.
- **test-writer** *(Read, Write, Edit, Bash)* — writes tests first from acceptance criteria.
- **reviewer** *(read-only)* — general diff review against CLAUDE.md invariants.

Cost note: multi-agent workflows are token-expensive. Per Anthropic's engineering write-up on their multi-agent research system, *"agents typically use about 4× more tokens than chat interactions, and multi-agent systems use about 15× more tokens than chats"* — reserve heavy fan-out for genuine parallel exploration, and route cheap read tasks (Explore) to cheaper models.

### 1.3 Plan mode
Plan mode is a **read-only permission mode**: Claude researches and proposes a plan and edits nothing until you approve (`code.claude.com/docs/en/common-workflows`; toggle with Shift+Tab, `--permission-mode plan`, or headless `claude --print --permission-mode plan`). The plan names files to change, the approach, side effects, and verification steps — so you can reject a bad approach in seconds *before code exists*. **Use it for every HIGH-RISK task and any change touching 3+ files.** Put this rule in CLAUDE.md: *"If reality doesn't match the approved plan, return to plan mode rather than improvising."*

### 1.4 Hooks / automated gates (`.claude/settings.json`)
Hooks are shell/HTTP/prompt/agent handlers firing at lifecycle events (`code.claude.com/docs/en/hooks`). Key events: **PreToolUse** (can block via exit code 2, and a deny is evaluated *before* the permission-mode check, so it can't be bypassed by switching modes), **PostToolUse** (runs after edits; cannot undo them but can surface feedback), **Stop/SubagentStop**. Set up:
- **PostToolUse** matcher `Edit|Write|MultiEdit` → `cargo fmt` on the changed file.
- **Stop / PostToolUse gate** → `cargo clippy --all-targets -- -D warnings` and `cargo test`; block (exit 2) on failure so Claude must fix before finishing the turn.
- **PreToolUse** matcher `Bash` → deny destructive commands (`rm -rf`, force pushes, `DROP TABLE`) and block edits to `tests/` during implementation turns (guards against the agent editing tests to make them pass).

Hooks are **deterministic** — they run regardless of what the model "decides" — which is exactly why security/quality gates belong here rather than in CLAUDE.md prose.

### 1.5 MCP servers
MCP connects Claude Code to external tools (`code.claude.com/docs/en/mcp`; `claude mcp add`, `stdio` for local / `http` for remote). Worth connecting here:
- **GitHub MCP** — PRs, issues, CI status, code review.
- **Postgres MCP** (read-only connection string) — inspect the platform schema during backend work.
- **A docs/context MCP** (e.g., a Context7-style server) — injects up-to-date, *version-specific* docs for Bevy/wgpu/matchbox/str0m/Rhai to counter hallucinated APIs from stale training data (see §1.11).
- **Playwright/browser MCP** — drive the browser build, read console output, measure cold-load.

Keep the set small — each server's tool definitions consume context (Tool Search defers loading, but not all servers support it). Scope DB access read-only; audit any new server before connecting.

### 1.6 Slash commands (`.claude/commands/*.md`)
Reusable prompts invoked by filename (`code.claude.com/docs/en/slash-commands`; frontmatter sets `allowed-tools`/`model`; `$ARGUMENTS` for input; note Anthropic is migrating commands toward the Skills format, but `.claude/commands/*.md` still works). Define:
- `/build-wasm` — run both WebGPU and WebGL2 builds + wasm-bindgen + wasm-opt, report sizes.
- `/slice-check` — run the Phase-1 acceptance checks and print the §3 instrumentation table.
- `/review-netcode` — dispatch the netcode-auditor.
- `/new-crate` — scaffold a workspace crate with the standard `CLAUDE.md` stub.

### 1.7 Headless / CI
`claude -p` runs non-interactively (`code.claude.com/docs/en/headless`). Use `--allowedTools` and a non-interactive permission mode — **`dontAsk` denies anything not explicitly allowlisted**, the docs' recommended locked-down CI mode — since there's no one to approve prompts. Add `--output-format json` to capture `total_cost_usd`. The official `anthropics/claude-code-action@v1` GitHub Action wires @claude mentions and automatic PR review; use it for **first-pass mechanical review only** — human review remains mandatory for netcode and sandbox code. Add `--bare` in CI to skip auto-discovery of local hooks/MCP/CLAUDE.md for reproducible runs.

### 1.8 Permissions / safety
Permission rules evaluate **deny → ask → allow** (per docs, as of mid-2026). A PreToolUse deny hook cannot be escaped by switching permission mode. **Never use `--dangerously-skip-permissions` outside a throwaway sandbox** (containers/VMs without production credentials or network). For unattended runs, containerize with no prod secrets mounted.

### 1.9 Context management for a long multi-crate project
Claude Code auto-compacts near the window limit; the root `CLAUDE.md` and auto memory survive compaction, but conversation detail is summarized away (`code.claude.com/docs`). Practices:
- Scope each session to specific crates/files; tell Claude which files are relevant *before* it explores.
- Run `/compact` proactively at task boundaries; `/clear` when switching subsystems; `/context` to inspect usage.
- Delegate codebase exploration to the **Explore** subagent to keep the main window clean.
- Maintain `PROJECT_STATE.md` (current state) and `DECISIONS.md` (an ADR log) that Claude reads at session start — this defeats the cold-start amnesia that bites long multi-session builds.

### 1.10 Git / PR workflow
- One feature branch per phase task; small PRs (one logical unit).
- Use **git worktrees** to run parallel sessions without collisions.
- **Commit tests before implementation** so `git diff tests/` proves the agent didn't tamper with them.
- Automatic PR review via the GitHub Action for mechanical issues; HIGH-RISK PRs get mandatory human review *plus* the relevant auditor subagent.

### 1.11 Known failure modes & mitigations
- **Hallucinated / version-drifted APIs** (acute for fast-moving crates like Bevy and matchbox — the agent interpolates from neighboring/older APIs). The compiler catches type errors; add a docs MCP; pin versions in CLAUDE.md. Rust's type system is a strong hallucination-defense layer but does **not** catch logic errors.
- **"Compiles but subtly wrong"** — the dominant risk for netcode/concurrency; agents "reach for the path that compiles, not the path that's correct." Neither the compiler nor clippy catch it. Mitigation: TDD, netcode-auditor, human review, deterministic replay tests.
- **Papering over errors** with `.unwrap()`, `.clone()`, or `unsafe`: forbid in CLAUDE.md; enforce with the clippy gate; the reviewer subagent flags them.
- **Silent scope creep / over-eager edits**: plan mode + small PRs + approve-the-plan discipline.
- **Context loss across sessions**: `PROJECT_STATE.md` + `DECISIONS.md` + per-crate `CLAUDE.md`.
- **Self-review softness**: never let the session that wrote code audit its own work — spin up a fresh auditor subagent with a dedicated brief.

---

## 2. RISK-TIERING GUIDE (read before every phase)

**HIGH-RISK — human-verified, tight loops, TDD, plan-mode-first, auditor subagent mandatory.** LLM code here compiles but can be subtly wrong or insecure:
- **The custom per-entity replication + authority-swap netcode** (Phases 1, 3): concurrency, distributed edge cases, ownership handoff, double-ownership, orphaned entities, host-migration election, anti-entropy resync. **No existing crate does this over WebRTC DataChannels by varying only authority**, so it is custom and unproven — verified research: `lightyear` defers IO to `aeronet` which has no WebRTC-DataChannel layer and its distributed authority is untested; `bevy_replicon` is server→client-only; `renet`/`renet2` lack WebRTC transport.
- **The Rhai sandbox hardening** (Phase 12): security-critical — resource limits, whitelisted API surface, no eval/fs/network, preventing sandbox escape. Protects the machine from malicious *content*.
- Anything touching **crypto/signing** (ed25519 op signing), **billing/entitlement** boundaries, and **anti-cheat validation**.

**LOW-RISK — delegate heavily; the agent writes most of it, acceptance criteria are the contract.** High-volume, well-trodden, verifiable by tests/compiler:
- Boilerplate, glue, ECS component/system scaffolding.
- Platform/backend services: Postgres schema + migrations, auth/OAuth wiring, billing-provider integration, signaling/matchmaking WebSocket service, publish-pipeline plumbing.
- Build tooling, the two-WASM-build pipeline, CI, size instrumentation.
- Tests (agent writes them; for HIGH-RISK areas the *human specifies the cases*).

Each phase below is labeled **[HIGH]**, **[LOW]**, or **[MIXED]**.

---

## 3. MEASUREMENT GAPS / EXPERIMENTS TO INSTRUMENT (tie to the slice)
Unknowns to *measure*, not assume. Build instrumentation into Phase 1 and re-report every phase:
- [ ] **WASM binary size** per build (WebGPU vs WebGL2), before and after `wasm-opt -Oz` + brotli. Bevy WASM binaries are big: per the Bevy Cheat Book, *"Even when optimized for size, they can be upwards of 30MB (reduced down to 15MB with wasm-opt)"*; the WebGPU fox demo's `wasm_example_bg.wasm` was reported at ~22MB. Budget within the wasm32 **4 GB memory ceiling** (memory64 immature).
- [ ] **Cold-load time** in-browser (download + instantiate + first frame) — explicitly a measurement gap.
- [ ] **Replication bandwidth per peer** at 30–60 Hz (bytes/s pre- and post-delta-compression and interest management).
- [ ] **Per-message sign/verify cost in-browser** (ed25519 via WebCrypto/WASM) — decides whether per-frame state-channel signing is affordable vs reliable-channel-only.
- [ ] **STUN-only connection failure rate.** Expect a meaningful fraction of peers behind symmetric NAT / restrictive firewalls to require TURN: industry estimates put TURN-required consumer WebRTC sessions at roughly **15–20%** (Fora Soft), with some sources citing up to ~30% (RTC Insights). Silent failure for these peers is accepted on free tiers (STUN-only); Mode 3 provides TURN.

---

## 4. WORKSPACE LAYOUT (settled)
Cargo workspace, multi-crate:
- `engine-core` — Bevy setup, shared systems, ECS components.
- `replication` — the custom protocol (wire format, quantization, delta/baseline, authority, handoff, resync). **HIGH-RISK.**
- `transport` — matchbox (browser) + str0m (native/server) abstraction, two-channel config.
- `scripting` — Rhai engine, sandbox, ECS bridge. **HIGH-RISK (sandbox).**
- `client` — WASM/native client (winit + wgpu).
- `server` — headless authoritative Bevy sim (MinimalPlugins + fixed tick).
- `services` — signaling / session-registry / matchmaking WebSocket service.
- `platform` — Postgres / identity / billing / publish / moderation backend.
- `protocol` — shared types (versions, messages, content IDs).

---

## PHASE 1 — THE VERTICAL SLICE (build FIRST) [HIGH]
**Goal:** one ownership-explicit Bevy+Rhai mini-game; two peers over a matchbox WebRTC DataChannel, each authoritative over its own entities, replicating quantized snapshots on the unreliable channel + events on the reliable channel (Mode 2); then run the SAME simulation headless-authoritative (Mode 3); prove that swapping ONLY authority assignment yields both modes with NO logic fork; deliberately exercise one A→B ownership handoff. Instrument everything in §3. This de-risks replication, authority-swap, handoff, matchbox transport, and Rhai integration at once.

**Workflow for this phase:** plan-mode-first for every replication/authority task; TDD (human specifies cases); netcode-auditor after each implementation; small commits.

### 1.1 Project + build scaffolding [LOW — delegate]
- [ ] Initialize the Cargo workspace with the §4 crates (stubs). *Acceptance:* `cargo build` and `cargo test` succeed.
- [ ] Set up the **two-WASM-build** pipeline: a WebGPU build and a WebGL2 build, plus JS capability detection to select the build. Bevy cannot serve both from one binary — open issue **#13168** ("Support WebGL2 and WebGPU in the same WASM file"); per the Bevy README, *"To build for WebGPU, you'll need to enable the webgpu feature. This will override the webgl2 feature,"* and WebGPU requires `RUSTFLAGS=--cfg=web_sys_unstable_apis`. So two separate `cargo build --target wasm32-unknown-unknown` invocations are required. *Acceptance:* both artifacts produced; a stub page loads the correct one per browser capability.
- [ ] Add the size-optimized release profile (`opt-level="z"`, `lto=true`, `codegen-units=1`, `strip=true`, `panic="abort"`) and a `wasm-opt -Oz` + brotli post-step (run wasm-opt on the *final* file after wasm-bindgen). *Acceptance:* `/build-wasm` prints sizes for both builds.
- [ ] Ship **single-threaded** (do NOT enable SharedArrayBuffer/COOP-COEP — it breaks the OAuth/payment popups Mode 3 needs). *Acceptance:* no cross-origin-isolation headers set.
- [ ] `CLAUDE.md` + `.claude/` setup: root CLAUDE.md, per-crate stubs, the four subagents, hooks (fmt/clippy/test gates), slash commands. *Acceptance:* `/slice-check` exists; clippy gate blocks on warnings.

### 1.2 Rhai ↔ Bevy ECS bridge (thin custom bridge) [HIGH]
**Decision (confirmed by research): use a thin custom bridge, NOT `bevy_mod_scripting` (BMS).** BMS's own platform-support table lists **WASM as unsupported** ("no, see this issue" — open issue #166), which is disqualifying for a browser-first project. (BMS also pins to a specific Bevy patch version because it generates bindings per Bevy release — verify its current Bevy pin against its `Cargo.toml` before considering it for the *native-only* path; its latest releases have targeted Bevy 0.18.)
- [ ] Build the `scripting` crate holding the Rhai `Engine` + compiled `AST` + `Scope` in a Bevy `Resource`. Register a minimal whitelisted API via `engine.register_type::<T>()` / `engine.register_fn(...)`; call per-tick logic with `engine.call_fn(...)` against the compiled `AST`. Use `Engine::new_raw()` for a locked-down surface (adds nothing by default — not even arithmetic — so every capability is explicit). *Acceptance:* a Rhai script mutates a whitelisted component each tick; unregistered calls fail.
- [ ] Apply initial sandbox limits now (full hardening is Phase 12): `set_max_operations`, `set_max_call_levels`, `set_max_string_size`, `set_max_array_size`, `set_max_map_size`; no eval/fs/network. *Acceptance:* an infinite-loop script terminates with an error (Rhai returns `ErrorTerminated` when `max_operations` is hit), not a hang.
- [ ] Hot-reload: detect script file change → recompile `AST` → swap the stored `AST` (keep the `Engine`; reset or retain `Scope` as needed). This is the idiomatic Rhai pattern. *Acceptance:* editing the script changes behavior at runtime without restart.

### 1.3 The mini-game simulation (mode-agnostic) [MIXED]
- [ ] Define ECS components for a tiny game (positions/velocities, a couple of owned entities per peer) with explicit `Owner`/authority tags. *Acceptance:* runs Standalone (Mode 1) with local authority.
- [ ] Split logic into "authority computes state" vs "receiver applies state" so the SAME systems run in all modes and only authority assignment differs. *Acceptance:* a single `authority_of(entity)` decision point; **no mode-specific gameplay branches** (proven by grep/audit).

### 1.4 Transport: matchbox two-channel [MIXED]
- [ ] Wire `matchbox_socket`/`bevy_matchbox` with TWO channels — unreliable/unordered (positions/inputs) + reliable/ordered (events/handoffs/resync) — e.g. `WebRtcSocketBuilder::new(url).add_channel(ChannelConfig::unreliable()).add_channel(ChannelConfig::reliable())`. Matchbox natively supports multiple channels with configurable ordering/retransmit. Stand up a matchbox signaling server. *Acceptance:* two browser tabs connect P2P; data flows on both channels.

### 1.5 The replication protocol (custom) [HIGH]
- [ ] Wire format: bincode/postcard with **quantized floats** (fixed-point positions, quantized quaternions) + **per-component delta vs last-acked baseline**. TDD: write serialization round-trip and quantization-bound tests FIRST. *Acceptance:* round-trip within quantization tolerance; delta-vs-baseline verified.
- [ ] Owner computes snapshot/delta → sends on the unreliable channel; receiver applies directly (predict-own, interpolate-others). Last-write-wins, no causal metadata. *Acceptance:* two peers each authoritative over own entities; remote entities interpolate smoothly.
- [ ] Durable events (spawn/despawn/ownership) on the reliable channel. *Acceptance:* events never lost.

### 1.6 Authority-swap to Mode 3 (the proof) [HIGH]
- [ ] Run the SAME simulation as a headless authoritative server: Bevy `MinimalPlugins` + `ScheduleRunnerPlugin::run_loop(Duration)` with sim systems in `FixedUpdate` and `Time::<Fixed>::from_hz(tick_rate)` (default fixed tick is 64 Hz); server owns ALL entities; clients connect in a star. Note the Bevy caveat: fixed-timestep does not run in real-world wall-clock time, so drive network send timing separately, not off the fixed tick. *Acceptance:* identical gameplay to Mode 2 with authority reassigned to the server — NO logic fork (same systems crate).
- [ ] Prove the thesis: a test/demo that boots the identical sim in Mode 2 and Mode 3 by changing *only* authority assignment. *Acceptance:* documented side-by-side run.

### 1.7 Ownership handoff (exercise once) [HIGH]
- [ ] Implement one explicit A→B ownership handoff mid-session as a reliable-channel event. *Acceptance:* authority transfers cleanly; no double-ownership; no dropped entity; the receiver switches from interpolate to predict.

### 1.8 Instrumentation [LOW — delegate]
- [ ] Emit the §3 metrics (WASM size, cold-load, bandwidth/peer, sign/verify cost, connection success) from the running slice. *Acceptance:* `/slice-check` prints the table.

---

## PHASE 2 — Transport hardening [MIXED]
**Goal:** production-grade transport across browser and native/server.
- [ ] str0m (sans-IO) integration for native/server WebRTC; browser(web-sys)↔native(str0m) DataChannel interop (standard WebRTC). **[MIXED — the sans-IO poll/timeout event loop is fiddly; human-review the driving loop.]** *Acceptance:* a native str0m peer exchanges data with a browser matchbox peer on both channels.
- [ ] Two-channel config parameterized (reliability/ordering/retransmit). [LOW] *Acceptance:* config test.
- [ ] STUN/TURN: STUN-only for free modes; coturn TURN with **paid-only credentials** for Mode 3. [MIXED] *Acceptance:* TURN relay works with credentials; measure STUN-only failure rate (§3).
- [ ] Reconnect / ICE-restart handling. [MIXED]

## PHASE 3 — Replication depth [HIGH]
**Goal:** turn the slice's replication into a robust layer. Every task: plan-mode + netcode-auditor + TDD + deterministic replay tests.
- [ ] Delta compression vs last-acked baseline, per-peer ack tracking. *Acceptance:* bandwidth drops measurably vs full snapshots (§3).
- [ ] Interest management (AOI, spatial grid) as a separate bandwidth+visibility layer. *Acceptance:* out-of-range entities not replicated; **structurally withholds out-of-view state** (feeds the Mode 3 read-cheat defense).
- [ ] Prediction/reconciliation/interpolation buffers (predict-own, interpolate-others). *Acceptance:* smooth remote motion; own-entity correction on divergence.
- [ ] Anti-entropy resync: periodic state-hash + full-snapshot refetch from the owning peer (no CRDT). *Acceptance:* injected desync self-heals.
- [ ] Ownership-handoff failure modes: double-ownership resolution, orphaned-entity-on-owner-drop reassignment via host-migration election (**lowest-peer-ID tiebreak / oldest-survivor join-order**). *Acceptance:* kill an owner mid-session → entity reassigned exactly once.

## PHASE 4 — Mode 1 Standalone [LOW]
**Goal:** free, local-authority, no networking, no anti-cheat.
- [ ] Local-only session path (authority over all entities, replication disabled). *Acceptance:* runs with the networking stack absent.
- [ ] Opt-in content-addressed save. *Acceptance:* save/reload by content ID.

## PHASE 5 — Central services (signaling / session / matchmaking) [LOW]
**Goal:** the WebSocket service required even for free tiers. Delegate heavily.
- [ ] SDP/ICE signaling + session registry. *Acceptance:* peers exchange offers/answers; sessions listed.
- [ ] Matchmaking groups only same-mode, same-version players. *Acceptance:* mismatched version/mode never matched.
- [ ] Version-triple enforcement/gating (`{engine, content, schema}`); gate join, no force-update. *Acceptance:* incompatible client rejected at join with a clear reason.
- [ ] Horizontal scale: stateless nodes + Redis/Postgres session registry. *Acceptance:* two nodes share the registry.
- [ ] Mode 2 coordinator peer holds bookkeeping only; host migration by oldest-survivor election. **[MIXED — election logic is distributed-edge-case-prone; human-review.]**

## PHASE 6 — Identity + accounts + billing [MIXED]
- [ ] Device keypair per install: browser WebCrypto **non-extractable** key in IndexedDB; native OS keyring. **[HIGH for key handling.]** *Acceptance:* key persists; never exportable in browser.
- [ ] Mode 2 op signing (`ed25519-dalek`): always sign the reliable channel; per-frame state-channel signing configurable and measured (§3). **[HIGH.]** *Acceptance:* tamper-evident ops; verify cost measured.
- [ ] OAuth account for Mode 3; billing via a hosted payment provider (**raw card data never touches our systems**). **[MIXED — entitlement boundary is HIGH; wiring is LOW.]** *Acceptance:* entitlement gates Mode 3 join; no PAN in our systems.

## PHASE 7 — Persistence [LOW]
- [ ] Postgres schema: identity, billing, published-content metadata, rankings, match records. *Acceptance:* migrations apply; constraints enforced.
- [ ] Object storage, content-addressed (hash = content ID). *Acceptance:* store/fetch by hash; dedupe.
- [ ] Session state ephemeral; opt-in content-addressed snapshot. *Acceptance:* snapshot restorable by ID.

## PHASE 8 — Publish pipeline + versioning + UGC moderation [MIXED]
**Goal:** the central chokepoint and the *sole* moderation vantage.
- [ ] Immutable content-addressed IDs + `{engine, content, schema}` version triple stamped at publish. [LOW] *Acceptance:* republish yields a new ID; triple recorded.
- [ ] Custom content-bundle loader (Bevy 0.19 BSN has no first-party file loader yet). [MIXED] *Acceptance:* a bundle (Rhai + assets + scene data) loads and hot-reloads.
- [ ] Moderation: automated scan at publish + human report queue (P2P sessions can't be moderated in real time, so publish is the only vantage). [MIXED] *Acceptance:* flagged content blocked; reports enqueued.

## PHASE 9 — Mode 3 infra / orchestration [MIXED]
- [ ] Per-session headless Bevy sim process; managed session-fleet / process-pool with warm-pool **sub-second cold-start** and per-second billing; Agones only at scale. *Acceptance:* session spins up sub-second from the warm pool.
- [ ] TURN provisioning (coturn) with paid-only credentials; entitlement-gated. *Acceptance:* only paid sessions get TURN creds.
- [ ] Server-authoritative validation + interest management as max-achievable-in-browser anti-cheat. **[HIGH for validation logic.]**

## PHASE 10 — Hot update + native distribution [MIXED]
- [ ] Content hot-reload at runtime on both targets. [LOW] *Acceptance:* content swaps without engine restart.
- [ ] Engine/binary as a **versioned release** (NOT hot-reloadable in prod): browser reloads WASM via service-worker cache-bust; native auto-update (`self-update` crate) + relaunch. [MIXED] *Acceptance:* new engine version loads on reload/relaunch; version-gating on join defends against desync.
- [ ] Native distribution as plain **code-signed executables** (NOT a webview wrapper). [MIXED] *Acceptance:* signed binaries for target OSes.

## PHASE 11 — Anti-cheat hardening [HIGH]
- [ ] Mode 2: plausibility/bounds checks on incoming state + signed ops (tamper-evident, **not** cheat-proof). *Acceptance:* out-of-bounds state rejected; forged ops detected.
- [ ] Mode 3: server-authoritative validation + interest-management read-cheat blocking. *Acceptance:* out-of-view state withheld; invalid client input rejected.
- [ ] Document accepted structural ceilings: browser clients are unattestable (WASM/JS inspectable) ⇒ Mode 3 "max anti-cheat" = max-achievable-in-browser; hidden-info + Sybil/collusion cheats are unwinnable without an authority (gated to paid Mode 3).

## PHASE 12 — Rhai sandbox hardening [HIGH — security-critical]
**Goal:** the security pass protecting the machine from malicious content. Plan-mode + sandbox-auditor + adversarial tests mandatory.
- [ ] Hard resource limits finalized and tested adversarially: max operations, call depth, string/array/map sizes (Rhai default call depth is 64 in release / 8 in debug; set explicitly). *Acceptance:* adversarial scripts (deep recursion, huge allocations, tight loops, deeply-nested maps) all terminate with errors.
- [ ] Whitelisted API surface only via the minimal engine (`new_raw` + explicit `register_*`); no eval, no filesystem, no network. *Acceptance:* enumerated allowed calls; everything else rejected.
- [ ] Keep scripts **thin** (high-level logic only); hot loops stay in Rust/Bevy systems. *Acceptance:* no per-entity hot path in Rhai.
- [ ] Fresh `Scope` per invocation where state must not leak. *Acceptance:* no cross-script/session state bleed.
- [ ] Document that the sandbox protects the machine from malicious **content**, not the game from a modified **client**.

## PHASE 13 — Social / Trust & Safety [LOW]
- [ ] Emoji-only social (no text/voice/media anywhere — hence no SFU/media server in any mode), rate-limited. *Acceptance:* only emoji payloads accepted; rate limit enforced.
- [ ] Local mute/block. *Acceptance:* a muted peer's social payloads suppressed client-side.

## PHASE 14 — Native parity + scaling + observability [MIXED]
- [ ] Native parity: same Bevy binary, native winit + native wgpu; native can host the Mode 3 headless authoritative server. *Acceptance:* native client + native-hosted server run the slice.
- [ ] Scaling: fleet autoscale; session registry under load. [LOW]
- [ ] Observability: metrics/tracing/logging across services; keep reporting §3 metrics in prod. [LOW] *Acceptance:* dashboards for bandwidth, session cold-start, connection success, and cold-load.

---

## 5. Recommended crates (settled stack)
Bevy 0.19 (ECS/wgpu), **Rhai via a thin custom bridge (NOT `bevy_mod_scripting`, which lacks WASM support)**, `matchbox_socket`/`bevy_matchbox` (browser P2P + signaling, native multi-channel WebRTC), `str0m` (native/server WebRTC, sans-IO — note the maintainers test it primarily as an SFU; the P2P path has had less testing, so budget integration time), custom replication protocol, Avian or `bevy_rapier` (physics; determinism not needed), `ed25519-dalek` (signing), Postgres + object storage, coturn (TURN), managed session-fleet for Mode 3.

**Confidence:** matchbox / str0m / Rhai / ed25519-dalek are established for their roles; the unified replication + authority-swap layer is **novel**, unbacked by any existing crate over WebRTC DataChannels, and must be custom-built and proven first in Phase 1. Verify every crate's current Bevy-version compatibility (the ecosystem pins tightly to Bevy releases) before pinning in `CLAUDE.md`.

---

## Recommendations (staged next steps)
1. **Start with §1.1–1.6 setup, then Phase 1.1–1.2.** Get the build tooling, CLAUDE.md, hooks, and the Rhai bridge working before any netcode. Benchmark that flips this: if the WebGPU+WebGL2 two-build size after `wasm-opt`+brotli is prohibitive (e.g., the "upwards of 30MB → ~15MB" range is unacceptable for your cold-load target), pause and do a size-budget spike (feature-prune Bevy, lazy-load assets) before proceeding.
2. **Treat Phase 1.5–1.7 as the go/no-go gate for the whole architecture.** If you cannot demonstrate Mode 2 and Mode 3 from one simulation by changing only authority (1.6), or a clean A→B handoff (1.7), stop and revisit the replication design before building services — everything downstream assumes the authority-swap works.
3. **Only after the slice is green, fan out.** Phases 4–8 (Mode 1, services, identity/billing, persistence, publish) are LOW/MIXED and safe to delegate broadly to Claude Code with the acceptance criteria as the contract; run them in parallel worktrees if using multiple sessions.
4. **Sequence the two HIGH-RISK deep passes (Phase 3 replication depth, Phase 12 sandbox) with a human owner each.** Never let these merge on auto-review alone.
5. **Benchmarks/thresholds that change the plan:** STUN-only failure materially above ~20–25% → prioritize TURN earlier / reconsider free-tier P2P expectations; per-frame sign/verify cost too high in-browser → default to reliable-channel-only signing; cold-load unacceptably slow → invest in binary-splitting/lazy assets before feature work.

## Caveats
- **Claude Code specifics evolve.** Version-gated features (auto memory v2.1.59+, permission-mode names, the commands→skills migration, GitHub Action versioning) should be re-verified against your installed version and `code.claude.com/docs`. Durable patterns (plan-mode-first for risky work, deterministic hook gates, TDD, isolated auditor subagents, small PRs) are the stable core.
- **The replication+authority-swap layer is genuinely novel.** The claim that no existing Rust crate backs all three modes over WebRTC DataChannels by varying only authority is a design premise, not a measured guarantee — Phase 1 exists to validate it. `lightyear` does offer transferable client/server authority and prediction/interpolation but over WebTransport/WebSocket/Steam, not WebRTC DataChannels.
- **Third-party token/size figures are estimates.** The "4×/15× token" multipliers are Anthropic's own reported numbers; the "15–20% (up to ~30%) TURN-required" and "30MB→15MB" WASM figures are from secondary industry/community sources and will vary with your NAT population, feature set, and Bevy version — measure your own (§3).
- **`bevy_mod_scripting`'s Bevy pin and WASM status should be re-checked** against its current `Cargo.toml`/README; the decisive fact for this project (no WASM support) is what drives the thin-custom-bridge decision, and that could change if issue #166 is resolved.