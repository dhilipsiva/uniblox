# Mode-3 per-session cost model

Mode 3 runs one headless server process per session (ADR-0014, ADR-0052). This page turns measured
per-session resource use into a cost per session-hour, so a subscription price can be checked
against it. Prices change; the ones below are the planning inputs the research recorded. Look them up
again before you set a price.

## The model

A session's cost per hour is:

    cost/h = compute/h + egress/h + TURN/h

- **compute/h** = (the price of one VM per hour) / (sessions one VM holds). The sessions a VM holds is
  the smaller of its CPU and memory limits, with headroom left for bursts.
- **egress/h** = (GB the server sends per hour) × (the price per GB). Clients send far less than they
  receive: inputs and acks.
- **TURN/h** = (the fraction of clients that relay) × (their GB per hour) × 2 × (the price per GB). A relay
  receives and resends every byte, so relayed traffic costs twice on a host that bills both directions.

The subscription must cover cost/h × (expected hours played per subscriber) with margin. Payment also
gives Sybil resistance (the repository's Phase 9 notes).

## Measured inputs

Measured 2026-10-21 with `just session-cost` (`crates/server/examples/session_cost.rs`). One real
release-built `server` process per session, 8 moving server-owned entities, the 64 Hz fixed tick, state
sent at 20 Hz. The clients replicate fully: they apply state and ack it, so delta baselines settle as they
do in play. Loopback on one Linux machine: no network loss.

| Clients | Server CPU (% of one core) | Server resident memory | Bytes each client receives per second |
|---|---|---|---|
| 1 | 0.8 % | 9.5 MiB | 3,703 |
| 4 | 1.2 % | 10.6 MiB | 3,527 |
| 8 | 1.3 % | 11.4 MiB | 3,702 |

Read them as a floor, not a forecast. Each scales with the game:
- Egress grows with the entities each client sees in a tick. The AOI (ADR-0021) and the per-datagram
  cap (ADR-0029) bound it.
- CPU grows with scripted entities (`just script-cost`: about 1 µs per scripted entity per tick) and with
  clients.

Measure a real game the same way before setting a price.

## Worked example (planning inputs, not quotes)

An 8-client session at the measured rates:

- **Egress:** 8 × 3.7 KB/s ≈ 30 KB/s ≈ 107 MB per hour.
  - at $0.09/GB (hyperscaler egress, as recorded in the research): about $0.0096 per session-hour;
  - at $0.005/GB (bandwidth-pooled VPS overage): about $0.0005 per session-hour.
  - A NAT-gateway processing fee (about $0.045/GB on some hyperscalers) adds about half again.
- **Compute:** at about 1.3 % of a core and 11.4 MiB, one vCPU could hold about 70 such sessions on CPU,
  and 2 GB about 170 on memory. Planning with 50 % headroom gives about 35 sessions per vCPU. So
  compute/h ≈ (VM price per vCPU-hour) / 35. The VM price is an input to look up for the chosen host
  and region.
- **TURN:** if 10 % of clients relay, add 0.1 × 107 MB × 2 ≈ 21 MB per session-hour at the same per-GB
  price.

At these rates egress, not compute, dominates on a hyperscaler, and the host's bandwidth pricing
decides the margin. The research reached the same conclusion: prefer bandwidth-pooling hosts for Mode-3
servers and TURN relays.

## What a session costs when idle

A server with no client exits after `UNIBLOX_IDLE_SHUTDOWN_SECS` (default 300 s, ADR-0052), so an
abandoned session costs at most five minutes of one process: about 10 MiB and well under 1 % of a core.

## Re-measuring

`just session-cost` runs the measurement; change `ENTITIES`, the client counts or the window in the
example to model a game. The server's `/metrics` endpoint (`uniblox_server_peers`,
`uniblox_server_entities`, `uniblox_server_ticks_total`) and the signaling service's `/metrics` give
the live counts to compare against in production.
