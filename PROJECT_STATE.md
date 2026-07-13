# PROJECT_STATE.md

Living snapshot of where uniblox is. Update it when a phase's status changes.
The **why** behind decisions lives in `DECISIONS.md`; the **what/how** lives in
`docs/final-buildspec.md`; the backlog lives in `TODO.md`.

**Current phase: PHASE 1 COMPLETE; PHASE 2 (transport hardening) COMPLETE bar the deploy-gated telemetry
numbers; PHASE 3 (replication depth, HIGH) HAS BEGUN; PHASE 4 (Mode 1 Standalone) UNDERWAY (A1 — the
net-free `standalone` runtime — DONE, ADR-0030; A2 — browser-playable Mode 1 in the client — DONE, ADR-0031;
B1 `ContentId`/blake3 — DONE, ADR-0032; B2 `persistence` save/load — DONE, ADR-0033; B3 native `FileStore` —
DONE, ADR-0034; B4 browser `IdbStore` — DONE, ADR-0035; C1 client save/load keybinds — DONE, ADR-0036 —
**PHASE 4 COMPLETE**).** The slice proved the authority-swap (gate PASSED,
ADR-0014) and **the Bevy client renders in-browser (ADR-0017)** with every slice measurement taken (real
two-build sizes 3.38/3.40 MB brotli, cold-load, in-browser ed25519; size-budget gate PASSES). **Phase 2 is
done:** str0m native/server peer (ADR-0015), ICE policy tiers + hermetic TURN relay proof (ADR-0016),
connection telemetry + fleet aggregation (ADR-0018), and reconnect / ICE-restart resilience (ADR-0019); the
only open Phase-2 thread is the real-network telemetry NUMBERS (deploy-gated). **Phase 3 underway:** the
delta-vs-last-acked baseline + per-peer ack tracking (ADR-0020, fixed keyframe gone), then **interest
management / AOI (ADR-0021)** — the sender is now PER-PEER (`collect_all`) with a spatial-grid area-of-interest
gating both state AND existence (the Mode-3 read-cheat defense), per-(peer,entity) delta baselines, and
deterministic wire output; then **prediction / reconciliation / interpolation (ADR-0022)** — a separate
`RenderPos` render layer with interpolate-others (snapshot buffer + lerp), predict-own + server reconciliation
(input / `last_input`), and the handoff role reset. The ADR-0020 ack round-trip is now integration-covered
over the real `net_pump` (`server/tests/headless_app.rs::ack_round_trip_confirms_and_goes_quiet` — both
directions to quiescence; the fast-follow is closed). **Interest-management follow-ups (ADR-0023) DONE:**
stage a (quantization hoisted into the once-per-tick snapshot — byte-identical) + stage b (AOI-flicker
hysteresis — two-radius band `set_aoi_hysteresis`, enter at `r_inner` / exit at `r_outer`, band read-cheat
preserved) + stage c (opt-in per-client avatar+focus hook — `build_server_app_focused`: a server-owned
`ControlledBy` avatar per connection, AOI focused on it each net tick, disconnect despawns + prunes
`PendingInputs`; a client sees only its focus radius over the real pump). **Handoff depth + anti-entropy
resync (ADR-0024) DONE + WIRED into the pump:** deep handoff is covered (hand-back, repeated/cycle,
packet-loss); the R6 cross-sender reordering gap (frozen wrong-owner proxy) HEALS via a
digest→request→`ResyncSpawn` round (own-authority + responder-owns + AOI re-guards); and `server::net_pump`
now DRIVES resync — requests/responses every frame, digests on a slow `RESYNC_INTERVAL` (500 ms) accumulator —
proven by `resync_heals_injected_desync_over_pump` (an injected desync self-heals over real transport).
**Ownership-handoff failure modes (ADR-0025) underway:** stage B — host-migration reassignment on owner-drop
(`reassign_orphans`: each survivor deterministically re-tags a departed owner's entities to the lowest-peer-ID
survivor via `elect_owner`, with NO wire event — authority is derived; the elected survivor simulates, the
rest re-tag their proxy) DONE, and it CLOSES the ADR-0024 E4 orphan (exactly one survivor now holds a Local
proxy → witnesses heal via state, non-witnesses via resync). **Stage A-kernel — the `OwnerSeq` gate DONE:** a
per-entity monotonic `OwnerSeq{seq,coordinator}` (`NetIdRecord.owner_seq`, seeded `{0,spawner}`; **WIRE 4→5**)
now arbitrates every owner change — `transfer_ownership` mints `{prev.seq+1, coordinator:local}`, and the
`OwnershipTransfer` apply gate is `seq > rec.owner_seq` (strict), **REPLACING the old `owner!=from` check**, so
the R6 cross-sender reorder now RESOLVES BY RANK at the source (no freeze — resync's R6-freeze-heal role is
retired; its residual role is the stale-value / lost-transfer / orphan / E4 heals). `ResyncSpawn` is seq-gated
too (own-authority guard → same-owner value-heal → owner-change `>=` heal → orphan adopts the rank), closing the
stale-former-owner backdoor; the `>=`(resync)/`>`(transfer) asymmetry is deliberate + auditor-verified. two_world
99 green (Group AK + reworked R6); netcode-audited → MERGE. **Stage A-handshake — claim/commit/reject DONE
(WIRE 5→6):** `claim_ownership` routes a `ClaimOwnership` to the coordinator = lowest live peer (flips NO owner);
`drain_commits` arbitrates — winner = lowest claimant, mints `{prev.seq+1, coordinator:local}`, emits
`OwnershipCommit` to claimants + the DEMOTING prior owner (no double authority) + `ClaimRejected` to losers (and
to any un-arbitrable claim — no silent black-hole). All owner changes share one strict-`>` gate
(`apply_ranked_owner_change`); the commit arm has no own-authority guard (a commit is meant to demote). two_world
105 green (Group AK-H); netcode-audited → MERGE-with-follow-ups. **The ADR-0025 ownership-arbitration item is
COMPLETE.** Deferred (auditor MAJOR carry-forwards): push/pull MUTUAL EXCLUSION per entity (push + coordinator-
pull mint the rank independently, colliding at equal seq) and CONSISTENT-MEMBERSHIP consensus (a persistent
dual-coordinator split isn't bounded by the seq tiebreak) — both close with the `net_pump` Disconnected /
cross-owner-interaction thread. **Cross-owner interactions DOCUMENTED + RULED:** ADR-0026 recorded the
remote-vs-remote LATENCY gap as an accepted quality ceiling; **ADR-0027** built the deterministic
single-authority INTERACTION rule (R1 — each effect decided by the OWNER of the entity it mutates =
`authority_of` on the affected entity; a standing coarse `Interactable`/`Contacts`/`overlaps`/
`resolve_interactions` system in engine-core wired into the server `FixedUpdate`; `interaction_decider=min`
tiebreaks a shared outcome; reads the other's replicated `Position`, never re-simulates; Mode 3 owning all
dissolves the gap frame-perfectly, no fork). engine-core 12 + two_world 107 green; netcode-audited → MERGE.
**ADR-0028 — handshake WIRED into the pump + ADR-0025 carry-forwards CLOSED:** `net_pump` now drives
`drain_commits` (coordinator arbitration) every frame + `reassign_orphans` on disconnect. (a) **Sole-minter** —
a non-coordinator PUSH routes through the coordinator (`request_transfer`→`TransferRequest`, WIRE 6→7);
`drain_commits` unifies claims+requests (candidates = claimants ∪ live transfer target) into ONE
coordinator-minted commit, so a concurrent push+pull can't double-mint (`transfer_ownership` stays the
Mode-1/coordinator/mechanics primitive — the discipline is documented, not hard-guarded). (b) **Membership** —
`poll_peers` is the sole authority (`apply_events` never mutates `peers`; an audit caught + removed a
ghost-peer belt); deterministic `coordinator()` + seq gate + resync converge a split (full partition consensus
out of scope). two_world 110 + headless 8 green; netcode-audited → MAJOR ghost belt removed. **ADR-0029 — AOI size-cap DONE +
splitting/per-entity-acks DEFERRED:** `collect_all` now GUARANTEES each per-peer state datagram ≤
`SAFE_DATAGRAM_BYTES` — it keeps only the nearest entities that fit (rank by dist²/id, conservative full-mask
sizes), routing overflow through the audited AOI-exit path (an existence WITHHOLD, not a state-entry deferral;
no wire change). Deep design work (2 agents) showed true message-splitting is UNSOUND with the current
cumulative-run ack (needs a big negative-ack + reassembly rework) and the over-MTU blob is already correct
(higher loss probability, not a bug) + the stuck-entry stall is bandwidth-only + self-heals — so splitting +
per-entity acks are DEFERRED (YAGNI-until-measured; revisit for a dense-Mode-3 workload via per-bucket
sub-streams). two_world 116 green (Group CAP); netcode-audited → MERGE (byte-bound airtight, no false-confirm,
deterministic, read-cheat-preserving). Next Phase-3 threads: the Phase-5 Mode-2 coordinator peer SERVICE.

**PHASE 4 (Mode 1 Standalone) HAS BEGUN — Item A1 (standalone runtime) DONE (ADR-0030):** a NEW `standalone`
crate assembles the Mode-1 app — `build_standalone_app(local, entity_count)` = the server spine (TaskPool/Time/
ScheduleRunner + `Time::<Fixed>::from_hz(64)` + `insert_sim` + `spawn_owned(owner=local)` + the FixedUpdate sim
chain via `add_sim_systems`) MINUS `Net`/`net_pump`/`Replication`/`Transport`/`apply_input`/`count_tick`. It is
**net-free by construction** — its crate graph reaches only bevy_app/ecs/time + engine-core→protocol; a
`cargo tree` guard in `scripts/git-hooks/pre-commit` backstops it — which CLOSES Phase-4 bullet-1's "runs with
the networking stack absent" acceptance headlessly. Mode 1 is pure data (every entity owned by `local`), so
`simulate` integrates all and the prediction/interp/input stack is unused/unscheduled. `add_sim_systems` is the
net-free seam the browser-playable client reuses; `server` is NOT refactored to share it. `standalone`:
1 inline + 2 integration tests green; full workspace green; clippy/fmt clean; reviewer → clean. **Item A2
(ADR-0031) DONE — browser-playable Mode 1:** the `client` wires that net-free sim into its `DefaultPlugins`
render app (`Camera2d` + a keyboard-driven `Avatar` + drifting NPCs via `insert_sim`/`spawn_owned`;
`standalone::add_sim_systems` on FixedUpdate; `drive_avatar` input→`Velocity` via a native-tested `move_dir`;
`sync_render` `Position`→`Transform`; no prediction/interp — local authority), replacing the ADR-0017 Bouncer
demo. Both WASM builds compile; size gate re-checked → PASS (3.39/3.41 MB brotli, ~+10 KB); `client` native test
2/2, clippy native+wasm32 + fmt clean, workspace green, reviewer → clean. Live in-browser render+keyboard was NOT
exercisable in this environment (WSL server teardown + no-GPU in-app browser) — flagged for a manual browser
check. **Item B1 (ADR-0032) DONE — content-addressing:** `ContentId([u8;32])` = the blake3-256 digest of a byte blob
(`content_id()`, `to_hex`/`from_hex`, `ContentIdError`, `Ord`) + a reserved `VersionTriple` in `protocol`
(blake3 pinned `pure` — no C toolchain, wasm-safe; already in the lock via bevy_asset, so ~no new wasm code and
the client doesn't use it yet). `protocol` tests green incl. a known blake3 empty-vector; clippy native+wasm32 +
fmt clean; workspace green; reviewer → clean. **Item B2 (ADR-0033) DONE — the content-addressed save:** a new `persistence` crate — `save_world(&World) ->
(ContentId, Vec<u8>)` (read-only, `Owner`-filtered, canonical-sorted so the id is spawn-order-independent) +
`load_world`/`load_world_verified` (two-pass clear → `insert_sim` → rebuild via `spawn_owned` + `Contacts` insert)
+ a `ContentStore` trait + in-memory `MemoryStore`; a DTO-mirror `SaveBlob` keeps engine-core serde-free. **Closes
Phase-4 bullet-2's "save/reload by content ID" acceptance headlessly** (in-memory). 7 tests green (round-trip +
determinism + mismatch/verify + clear-path); clippy native+wasm32 + fmt clean; workspace green; reviewer → clean
(4 nits applied). **Item B3 (ADR-0034) DONE — native durable `FileStore`:** `<content-id-hex>.blob` files under a root dir
(`#[cfg(not(wasm32))]`, `std::fs`; inherent `io::Result` methods, NOT the infallible `ContentStore` trait —
I/O fails; content-addressed dedup + unique-temp+atomic-rename), so the native Mode-1 save survives a process
restart. 13 persistence tests (7 codec + 6 file_store incl. end-to-end save→file→verified-load + tamper detect);
clippy native+wasm32 (FileStore cfg'd out) + fmt clean; workspace green; reviewer → clean (unique-temp fix
applied). **Item B4 (ADR-0035) DONE — browser durable `IdbStore`:** IndexedDB (async; raw web-sys IDB + a hand-rolled
`Closure`+`oneshot` bridge chosen over a helper crate for exact-pin safety; keyed by `ContentId` hex, value =
blob bytes; `put` awaits the tx commit for durability; `open` pins version 1 + propagates a create failure), so a
browser Mode-1 save survives a page reload. Its IDB code CAN'T be machine-tested here (no wasm-test runner matches
the `=0.2.121` pin) — verified by compile (both wasm builds) + reviewer (async bridge affirmed correct, 1 LOW
fixed) + a manual browser self-test in the client (`idb_selftest()`, `[uniblox-idb]` on-load markers, reload flips
"first session"→"durable"); size gate re-checked → PASS (3.39/3.41 MB, ~+2–3 KB). **Item C1 (ADR-0036) DONE — PHASE 4 COMPLETE.** The client gains opt-in save/load keybinds: `K` saves the live
world (`save_world` → `spawn_local` → browser `IdbStore` + a localStorage "latest" pointer), `L` loads it (pointer
→ `IdbStore::get` → a `LoadInbox` NonSend `Rc<RefCell>` bridge → an exclusive `apply_load` runs
`load_world_verified` + re-clothes the reconstructed entities with `Sprite`/`Transform`+`Avatar`, since the
authoritative save omits render/control roles). Verified by compile (both WASM builds) + reviewer (clean) + a
manual browser check (move → K → reload → L restores the world, also B4's end-to-end proof); size gate PASS
(3.40/3.42 MB). **Phase 4 delivered:** the Mode-1 Standalone runtime (A1 net-free `standalone` + A2
browser-playable) and the full content-addressed save (B1 `ContentId`/blake3 → B2 `save_world`/`load_world` +
`MemoryStore` → B3 native `FileStore` → B4 browser `IdbStore` → C1 client keybinds).

**PHASE 5 (central services) HAS BEGUN — scoped signaling + asymmetric filter + `?next=N` grouping DONE (ADR-0037/0038/0039):**
`services` is now a library+binary — `build_signaling_server` wraps matchbox FullMesh with a scope in the room PATH
(`<mode>~<content>.<schema>~<min>~<lobby>`; FullMesh isolates by path, so mismatched content/schema/min/lobby are
structurally never matched) + an `on_connection_request` gate + an in-memory `SessionRegistry` (lifecycle-balanced:
gate stashes room by `origin` → id-assign bridges `peer→room` → connect joins `sessions` → disconnect
removes+prunes). **ADR-0038 asymmetric version filter:** the client's own engine moved OUT of the room key into a
`?engine=N` query, so compatible-but-newer engines share ONE room; the gate admits iff `engine >= min_engine`, else
a REASONED rejection (`426` too-old / `400` malformed/missing-engine via an `axum` dep) instead of a bare 401.
**ADR-0039 custom `NextTopology` + `?next=N` grouping:** replaced FullMesh with a custom `SignalingTopology`
(`SignalingServerBuilder::new`) that re-implements FullMesh's relay (via `common_logic`) GENERALIZED with
client-specified `?next=N` session-SIZE grouping — `?next` absent ⇒ one unbounded session per room, `?next=N` ⇒ the
room subdivides into sessions keyed `<room>#<index>` capped at N (batch-deal / no-backfill: a session seals at N and
never refills). The topology can't see the query, so `?next` is stashed by the gate → bridged to the `PeerId` at id
assignment; the `SessionRegistry` became the topology's shared state (relay senders + grouping + listing).
5 unit + 16 raw-WS integration tests green (the ADR-0037/0038 relay tests now double as a FullMesh regression on the
unbounded path, plus next-caps-and-spills, cross-session relay isolation, no-backfill, invalid-next-400,
unbounded-when-absent, legacy-room grouping); clippy/fmt clean; workspace green. Closes the signaling+registry+scoping
bullet, the same-mode/same-version bullet, the version-triple/asymmetric bullet, AND the `?next=N` grouping bullet.
Remaining Phase-5: horizontal-scale Redis/Postgres registry; the Mode-2 coordinator peer service.

## Done
- **Cargo workspace** — virtual manifest, 10 crates under `crates/*` (glob members),
  size-optimized `[profile.release]`. `cargo build` + `cargo test` green.
- **Single-threaded stance** — no COOP/COEP anywhere (serve script + capability page + ADR-0003).
- **AI-workflow scaffolding** — per-crate `CLAUDE.md`, `DECISIONS.md`, four subagents
  (`test-writer`, `netcode-auditor`, `sandbox-auditor`, `reviewer`), five slash commands
  (`/build-wasm`, `/slice-check`, `/review-netcode`, `/new-crate`, `/write-tests`),
  four hooks (`.claude/settings.json` + `scripts/hooks/`), git pre-commit gate.
- **Build-pipeline scaffolding** — `scripts/build-wasm.sh`, `scripts/slice-check.sh`,
  `scripts/serve.sh`, `crates/client/web/index.html` (capability detection). `build-wasm.sh`
  runs end-to-end (tools via the flake); output is meaningless until a rendering Bevy client exists.
- **`.mcp.json`** scaffold (github, read-only postgres, docs/Context7, playwright).
- **Nix flake devShell + direnv** (ADR-0010) — `flake.nix`/`flake.lock`/`.envrc` provide a pinned
  Rust toolchain (1.96.1, wasm32 target) + `wasm-bindgen`/`wasm-opt`/`brotli`/`twiggy`/`node`,
  auto-activated on `cd` and via `direnv exec .`. Cargo/tool scripts self-activate.
- **Rhai ↔ Bevy-ECS bridge** (ADR-0011, first real deps — rhai 1.25 non-sync + bevy_ecs 0.19) —
  locked-down `new_raw()` engine + all `set_max_*` limits + `eval` disabled, held as a NonSend resource,
  mutating a whitelisted `Health` component per tick; in-memory + file hot-reload. 8 TDD tests green,
  sandbox-audited, compiles for wasm32. Full hardening is Phase 12.
- **The mode-agnostic mini-game sim** (`engine-core` + `protocol::PeerId`) — `Position`/`Velocity`/`Owner`
  components, `LocalPeer`/`SimDt` resources, `authority_of` as THE single authority decision point, one
  branching `simulate` system (Local computes; Remote is the documented apply-path placeholder — never
  re-simulates). 8 TDD tests green incl. the **Mode-2 two-perspective and Mode-3 shape proofs** — the
  authority-swap demonstrated at the unit level before transport exists. netcode-audited; wasm32-clean.
- **matchbox two-channel transport core** (ADR-0012) — `crates/transport` (matchbox 0.14; state=0
  unreliable, events=1 reliable), `crates/services` full-mesh signaling binary, hermetic native↔native
  two-peer datachannel test green, wasm client demo + `scripts/e2e-two-tab.mjs`. The nibli prior-art note
  was resolved obsolete (repo repurposed). **Browser-tab run VERIFIED (2026-07-11):** two tabs of a
  desktop-class Chromium on the Windows host (WSL2 mirrored networking; services in WSL2) each logged the
  other peer Connected plus `[STATE]`+`[EVENT]` receipts — real P2P WebRTC, data on both channels, webgpu
  build. The WSL2-HEADLESS limitation (ICE gathering never completes; matchbox wasm waits on it) still
  applies to headless CI — `scripts/e2e-two-tab.mjs` needs a non-WSL host.
- **The custom replication protocol** (ADR-0013, HIGH) — `protocol` wire format (postcard, spawner-stable
  `NetEntityId`, quantized `QVec2`, reserved signature field) + `replication` (authority-gated cached-
  `SystemState` sender, newest-seq LWW receiver, current-Owner validity, `transfer_ownership`, late-join
  replay). **e2e over real WebRTC** — tests committed before impl; netcode-audited (owned-ghost fix +
  documented cross-sender handoff gaps for Phase 3 resync). Snap-apply per decision — interpolation buffers
  are Phase 3.
- **Delta vs last-acked baseline + per-peer ack tracking** (ADR-0020, Phase 3, HIGH) — the fixed keyframe is
  replaced by a **contiguous-run cumulative-ack** delta: a component is sent while its quantized value
  differs from the per-entity baseline OR is not yet acked by every tracked peer, then goes quiet.
  `NetEvent::Ack{seq}` (reliable, directed) + `WIRE_VERSION`→2; sender `CompSend{value,run_start,last_sent}`
  with decide/commit split (empty tick consumes no seq); receiver `applied_seq`(fully-applied) SEPARATE from
  `last_seq`(seen) so it never acks a value it dropped (the F1 fix — state racing its Spawn, or a handoff
  owner-mismatch, must not falsely confirm). **28-test two-World battery green** (T29–T37 the delta cases,
  incl. the F1 regression `state_before_spawn_defers_ack` + the gap-reset soundness `gap_reset_keeps_run_
  contiguous`); T35 proves the bandwidth win (0 steady-state bytes for a confirmed stationary scene).
  **netcode-audited twice** (F1 blocker → fixed → MERGE). Fast-follow CLOSED (2026-07-12): the ack round-trip
  is now integration-covered over the real `net_pump` by `server/tests/headless_app.rs::
  ack_round_trip_confirms_and_goes_quiet` — the test `Client` gained the client-side ack/collect pump wiring
  and the test drives BOTH directions to quiescence (client acks the server's stationary entity ⇒ server goes
  quiet; a client-OWNED stationary entity exercises the server's ack-routing ⇒ client goes quiet). Both
  plateau assertions fail if either `drain_acks` send is removed; netcode-audited → MERGE.
- **Interest management (AOI, spatial grid)** (ADR-0021, Phase 3, HIGH) — the sender UNIFIED to PER-PEER:
  `collect(world) -> Outbox` became `collect_all(world) -> Vec<(PeerId, Outbox)>`. Each tracked peer sees only
  entities within its AOI (`set_aoi` circle; unset ⇒ unbounded/fail-open), with its own delta baseline
  (`send_state[peer][entity]`), seq stream, and `known` set. Out-of-AOI entities are withheld in BOTH state
  AND existence (spawn-on-enter / despawn-on-exit) — the structural Mode-3 read-cheat defense. New `interest`
  module = `SpatialGrid` (cell-bucketed, floor-celled, exact-dist² filter) + `Aoi`. Per-peer order
  dead→transfer→exit→enter→state (dead wins over transfer; exit drops the baseline; enter Spawns only our
  namespace); deterministic wire output (emissions sorted by `NetEntityId`, which gained `Ord`); `untrack_peer`
  clears all per-peer maps; `on_peer_connected` = track (no blanket replay). The RECEIVER is unchanged (the
  audited ADR-0020 F1 soundness is preserved). Server pump + e2e/mode_proof/slice_metrics/headless harnesses
  migrated to per-peer routing. **46-test two-World battery (AOI groups A–H incl. the white-box exit/re-enter
  re-baseline, read-cheat existence-withholding, both chained-handoff cases) + 5 grid unit tests green;
  netcode-audited THREE times** (F1 adopted-entity orphan + its over-broad fix, both closed → MERGE). Perf
  (shared per-tick snapshot) + AOI-flicker hysteresis are Phase-3 follow-ups.
- **Prediction / reconciliation / interpolation buffers** (ADR-0022, Phase 3, HIGH) — the full client-prediction
  stack, landed in three audited stages behind a SEPARATE `RenderPos` render layer (authoritative `Position`
  stays snap-applied — receivers never re-simulate others). **A (interpolate-others):** remote entities lerp a
  per-proxy snapshot buffer at `RenderTick − 6.4 ticks`, clamping (no extrapolation); `StateMsg.tick` +
  `WIRE_VERSION`→3. **B (predict-own + reconciliation):** a `Controlled`/`Input`/`InputHistory` subsystem +
  `NetEvent::Input` (reliable); the client re-simulates its avatar from local input and the server's snapshot
  re-anchors it via `StateMsg.last_input` (replaying un-acked inputs); the server processes ONE input per tick
  (`apply_input`), zeroing on underrun. **C (handoff):** `reset_render_role` flushes/seeds on the role
  transition (adopt seeds from the authoritative Position, not the stale interp — the Phase-1-flagged bug).
  The settled invariant is REFINED (recorded): prediction re-simulates ONLY the locally-controlled avatar,
  re-anchored each snapshot — bounded/self-correcting, NOT a determinism requirement (lockstep stays rejected).
  **two_world 69 tests + wire round-trips green; netcode-audited each stage (A MERGE, B FIX-FIRST→MERGE, C
  MERGE).** Fast-follows: Mode-2 `Tick` advance, multi-avatar per-entity markers, despawn cleanup, the
  in-browser render wiring (the client gameplay build).
- **THE AUTHORITY-SWAP GATE: PASSED** (ADR-0014, HIGH) — `crates/server` is the Mode-3 headless runtime
  (standalone bevy_app+bevy_time, 64 Hz FixedUpdate, exclusive net pump at ~20 Hz virtual-clock cadence,
  Messages<AppExit>). The M1–M4 battery is the documented side-by-side run: same simulation, same
  replication, Mode 2 vs Mode 3 differing ONLY in spawn/ownership data; Mode-3 clients emit zero
  messages; ~64 ticks/s evidence; net cadence decoupled from the fixed tick. netcode-audited (no hidden
  fork; its reproduced M2 replay-order flake fixed + soaked 6/6).
- **Ownership handoff (exercise once)** — CLOSED, auditor-verified against committed tests: reliable-channel
  Transfer mid-session (T26, matchbox channel config verified in source), clean transfer + stable identity
  (T18), no double-ownership (same-tick local flip + T19/T24 gates), no dropped entity (T18/T20/T26/T27),
  apply→compute switch (T10 before / T18 after — the slice's interpolate→predict stand-in per ADR-0013).
  Auditor findings actioned: T26 now genuinely asserts the replicate-back to A (the old Owner-view predicate
  was trivially true); T18 asserts the old owner's entity freezes; new T28 covers the mint-on-transfer arm.
  Phase-3 carries: adoption-switch re-verification with real buffers; hand-back/repeated/loss handoffs.
- **str0m native/server WebRTC peer** (ADR-0015, Phase 2) — `transport::Str0mPeer` (native-only): sans-IO
  str0m 0.21 (`rust-crypto`, Nix-friendly) speaking matchbox's exact signaling wire (via `matchbox_protocol`
  + blocking tungstenite) with one connection thread per remote peer running the canonical drain-to-Timeout
  loop; matchbox's negotiated no-DCEP channels (ids 0/1, labels `matchbox_socket_{i}`) pre-declared both
  roles. Interop proven hermetically: str0m↔native-matchbox BOTH role directions + str0m↔str0m, both
  channels both ways (3 tests, soaked 4/4). Fresh-reviewer gate caught the trickle-order bug (candidates
  before offer/answer are DROPPED by native matchbox — masked in tests by prflx discovery). Channel
  semantics (reliability/ordering/retransmit) are parameterized in one `CHANNEL_SPECS` source of truth that
  BOTH stacks derive from, locked by config tests (cross-stack parity by construction). Residuals:
  browser pairing (desktop-browser, ADR-0012), TLS signaling, non-loopback bind, reconnect (later items).
  **BROWSER pairing VERIFIED 2026-07-11** (`examples/str0m_browser_demo.rs` + the wasm demo in a
  desktop Chromium; both role directions, all four channel/direction combos) — and it caught a real bug the
  hermetic tests couldn't: `encode_candidate` hardcoded `sdpMid:"0"` but str0m emits a random mid, which
  Chrome rejects (`OperationError`) and matchbox-wasm panics on; webrtc-rs was lenient. Fixed by identifying
  the m-line by index only (`sdpMid:None, sdpMLineIndex:0` — single BUNDLE'd data m-line). Reviewer: MERGE.
- **STUN/TURN policy + relay proof** (ADR-0016) — `IceConfig` tiers (free = STUN-only default; Mode 3 =
  STUN+TURN with per-session paid credentials, carried never minted) + `Transport::connect_with_ice`.
  Hermetic coturn 4.13 (flake-provided) tests: relay-only webrtc-rs peers exchange a payload through the
  allocation (THE relay proof — host/srflx excluded by policy); wrong credentials gather zero relay
  candidates and never open; matchbox pass-through with only the TURN url + creds connects on both
  channels. Gotchas recorded: coturn blocks loopback peers by default (test-only `--allow-loopback-peers`);
  UDP readiness ≠ TCP readiness (STUN-binding probe in the harness). Residuals: STUN-only failure RATE is
  a real-network fleet metric; production coturn + credential issuance ride Phase 6.
- **Reconnect / ICE-restart** (ADR-0019) — `Str0mPeer` is now network-resilient: transient ICE
  `Disconnected` is tolerated (self-heal window, not fatal); the offerer does an in-place
  `sdp_api().ice_restart(true)` if it persists (DTLS/SCTP + channels survive, no data gap); the signaling
  WS reconnects with backoff WITHOUT killing live connections (a blip ≠ teardown); a hard failure triggers
  a bounded full reconnect (offerer-only, present-gated, ≤5 attempts). `request_ice_restart(peer)` is the
  ops/test trigger; `reconnects`/`ice_restarts` telemetry. Hermetic tests: `ice_restart_keeps_channels_and_counts`
  (channels survive an explicit restart, both channels still exchange), `connection_survives_signaling_drop`
  (kill the signaling server → data still flows) + decision unit tests; soaked. Fresh reviewer: MERGE (drain
  invariant + glare rule + reconnect bounds + finalize completeness all hold; SHOULD-FIX folded in). Real
  packet-loss recovery is mechanism-tested (can't simulate loss hermetically). **Phase 2 transport hardening
  is now COMPLETE** bar the deploy-gated telemetry NUMBERS.
- **Connection telemetry instrument + aggregation** (ADR-0018) — `Str0mPeer::telemetry()` records per-peer
  ICE outcome (Connecting/Connected/Failed), winning local-candidate kind (Host today), selected addrs, and
  RTT mean/jitter from str0m's ICE keepalive stats (the candidate-pair `current_round_trip_time`, NOT the
  media-only `PeerStats.rtt` — the load-bearing detail that hung the first test). `FleetMetrics::aggregate`
  turns many records into the STUN-only success fraction + candidate-kind breakdown + RTT/jitter
  distribution (min/mean/p50/p95/max, nearest-rank). Hermetic str0m↔str0m test (Connected + RTT + Host +
  addrs, soaked 4/4) + finalize unit tests + 6 aggregation unit tests; live demo prints `outcome=Connected
  local=Host rtt=0.6ms jitter=0.2ms`. Real NUMBERS need a deployed fleet (collect→aggregate→export wiring
  is Phase-14 observability); browser `getStats()` classification is a follow-up.
- **Instrumentation (native core)** — `slice_metrics` example + `/slice-check` table. **Measured (native
  loopback, 2026-07-10):** state channel 742 B/s per peer @ 2 entities (19.4 msg/s at the 20 Hz net tick,
  ~38 B/msg — comfortably inside the ~1150 B datagram budget); events steady 0 B/s; RTT 4.3 ms ± 0.6 ms
  (loopback, ~1 ms poll-bounded); **ed25519 native sign 13.4 µs / verify 25.7 µs** — AFTER adding an
  opt-level=3 override for the crypto crates (the size-optimized `opt-level="z"` profile made verify ~35×
  slower, 1600 µs — recorded in Cargo.toml as a Phase-6 wasm size-vs-speed consideration). **In-browser
  ed25519 MEASURED (2026-07-11, desktop Chromium, release wasm + the same crypto override): sign
  19.6–23.6 µs / verify 44.5–45.6 µs — only ~1.5–1.8× native, far better than the "several×" estimate;
  an 8-peer mesh at 30 Hz costs ~1% of a core to verify sequentially, so per-frame state-channel signing
  is affordable in-browser before batching.** The crypto's wasm SIZE cost (dalek + override, measured as
  the stub build delta): +106 KB wasm-opt / +55 KB brotli — the Phase-6 tradeoff inputs are now both
  real numbers. A cold-load harness is in `web/index.html` (`[uniblox-metrics] cold-load`); the real TTI
  and STUN success rate remain gated (Bevy client / real network — see TODO).

- **The Bevy client renders + all client-gated measurements** (ADR-0017) — Bevy 0.19 as a wasm32-ONLY
  client dep (`2d`+`bevy_winit`+`webgl2`; `webgpu` feature for build 2), minimal Camera2d + bouncing
  sprite into `#uniblox-canvas`, first-frame metric. Fixed two live pipeline bugs (the page served the
  UNOPTIMIZED wasm; `wasm-opt -all` emitted stable-browser-rejected encodings — baseline feature flags
  now, which also un-broke twiggy). **Measured:** brotli 3.38/3.40 MB per build (wasm-opt ~15.6 MB);
  feature-prune saves 34% wire size (5.16→3.38 MB); --converge −0.27%; cold-load 351 ms instantiate /
  381 ms first frame local (headless webgl2), ~2.7 s download @10 Mbps. **Size-budget gate PASS**
  (≤ ~8 MB brotli; first frame in target @10 Mbps, marginal @5 Mbps). Two-tab [STATE]/[EVENT] receipts
  re-verified with Bevy in-binary. PHASE 1 COMPLETE.

## Blocked / deferred (prerequisites do not exist yet)
- **MCP reachability** (github / read-only postgres / docs / playwright) — `node`/`npx` now provided by
  the flake; still needs a running read-only Postgres role, a GitHub PAT (in `settings.local.json`), and
  Playwright browsers. (`docs` should be reachable on the flake alone; playwright's chromium is now
  installed in `~/.cache/ms-playwright` and drives the headless render/metrics runs.)
- **Web Audio worklet** investigation — needs audio added to the client (the render core exists now,
  ADR-0017; the `2d` prune excludes bevy_audio).
- **STUN-only connection success rate** — real-network peers (Phase-2 telemetry bullet).

## Next
Phase 2 (transport hardening) is underway — str0m is landed; remaining Phase-2 items: two-channel config
parameterization, STUN/TURN, reconnect/ICE-restart, plus the browser-pairing residuals. Other candidates:
the **Bevy client rendering work** (closes every Phase-1 residual: real WASM sizes, cold-load, browser
metrics, Web Audio); Phases 4–8 are LOW/MIXED and delegate-friendly. Phase 3 (replication depth) and
Phase 12 (sandbox hardening) each need a human-owned HIGH pass.

## Toolchain notes
WSL2 Ubuntu. **The toolchain comes from the Nix flake devShell** (ADR-0010): pinned Rust 1.96.1
(edition 2024, wasm32 target) + `wasm-bindgen`/`wasm-opt`/`brotli`/`twiggy`/`node`/`coturn`. Run `direnv allow`
once per clone. Interactive `cd` auto-activates; for the WSL wrapper, prefix cargo/WASM-tool/npx
commands with `direnv exec .`:
`wsl -d Ubuntu -e bash -lc "cd ~/projects/dhilipsiva/uniblox && direnv exec . <CMD>"` (compound chains:
`direnv exec . bash -lc '<a && b>'`). Pure git/file commands use the plain wrapper. Ambient rustup
(cargo 1.92) still exists as a fallback for un-routed commands. Hook/build scripts self-activate the
flake and parse event JSON with `/usr/bin/python3` (the rye shim fails non-interactively; `jq` is absent).
No `just` here.
