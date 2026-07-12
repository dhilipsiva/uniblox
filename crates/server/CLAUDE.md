# CLAUDE.md — `server`

**Purpose:** headless authoritative Bevy sim (`MinimalPlugins` + fixed tick) —
the Mode 3 authoritative hub.
**Risk tier:** standard (Mode 3 validation logic becomes HIGH in Phases 9/11).

## Status
Implemented (the Mode-3 headless runtime, ADR-0014 — the authority-swap gate PASSED).
`build_server_app`: standalone `bevy_app`+`bevy_time` (TaskPool + Time + ScheduleRunner
at 1/64 s — NOT the `bevy` umbrella; `MinimalPlugins` lives in `bevy_internal`),
FixedUpdate = `sync_sim_dt` → `count_tick` → `advance_tick` → `apply_input` → `simulate`
(chained; `SimDt` fed from the fixed clock at the app boundary), Update = exclusive
`net_pump` (NonSend `Net`; receive every frame, emit acks (`drain_acks`) + resync
requests/responses (`drain_resync_requests`/`drain_resync_responses`) + ownership
COMMITS (`drain_commits`, ADR-0028 — the coordinator arbitrates queued claims /
transfer-requests) every frame, collect+send at `NET_INTERVAL` 50 ms via a
virtual-clock accumulator, and resync DIGESTS (`collect_resync`) on a SLOW separate
`RESYNC_INTERVAL` 500 ms accumulator — ADR-0024; the `send_directed` helper routes
every directed batch). Connect → `on_peer_connected`; **Disconnect → `untrack_peer`
+ `reassign_orphans` (ADR-0028) so a departed owner's entities re-tag to the
surviving lowest peer** + avatar despawn/`PendingInputs` prune (ADR-0023 c).
`poll_peers` is the AUTHORITATIVE membership signal (Connected/Disconnected).
Mode 3 is expressed purely as data: the server spawns/owns everything. Exit via
`Messages<AppExit>` (0.19 renamed Events→Messages). M3/M4 tests drive the real App;
demo entities must keep nonzero vel.x (test predicates observe replay-ordered proxies).
`ack_round_trip_confirms_and_goes_quiet` covers the ADR-0020 ack wiring end-to-end over
the real pump: a stationary server entity goes quiet once the client acks it, AND a
client-OWNED stationary entity (Mode-2-shaped) goes quiet once the server's ack-routing
confirms it — the test `Client` carries the client-side ack/collect pump wiring a real
client will need.

**Per-client AOI focus (ADR-0023 c):** `build_server_app_focused(t,l,n,focus_radius)` is an
OPT-IN focused server (`Net.focus_radius`). On connect it spawns a server-OWNED avatar the
client CONTROLS (`spawn_owned` + `ControlledBy(peer)`) at a distinct lane; each net tick it
focuses that client's AOI on its avatar (`set_aoi_hysteresis`, from the `ControlledBy` scan,
BEFORE `collect_all`). Disconnect despawns the avatar (ControlledBy scan) and prunes its
`PendingInputs`. `build_server_app` stays unfocused (unbounded — the M3/M4 default). Tests
`focused_server_withholds_out_of_focus_entities` + `two_focused_clients_see_disjoint_sets`.

## Crate-local invariants
- Runs the **identical simulation** as the client (same `engine-core` systems),
  with authority reassigned to the server — **NO logic fork**.
- **Mode 3 is authoritative, not a relay/SFU.** That authoritative guarantee is
  what the subscription sells; if it degrades to a relay, the anti-cheat value evaporates.
- Standalone `bevy_app`+`bevy_time` assembly + `ScheduleRunnerPlugin::run_loop(Duration)`;
  sim in `FixedUpdate` at `Time::<Fixed>::from_hz(64.0)`. Network send timing is driven
  separately from the fixed tick (virtual-clock accumulator in Update) — fixed-timestep
  is not wall-clock.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`.
