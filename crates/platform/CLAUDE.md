# CLAUDE.md — `platform`

**Purpose:** Postgres / identity / billing / publish / moderation backend.
Becomes a binary in Phases 6–8.
**Risk tier:** standard (MIXED — the entitlement/billing boundary is HIGH; wiring is LOW).

## Status
Stub (Phase 1.1). No functional code yet.

## Crate-local invariants
- **Raw card data never touches our systems** — billing via a hosted payment
  provider; no PAN stored.
- **Entitlement gates Mode 3 join** (and paid-only TURN credentials). Treat the
  entitlement boundary as HIGH-RISK.
- Content is **content-addressed** (hash = content ID); object storage dedupes by
  hash. The publish pipeline is the sole moderation vantage (P2P sessions can't be
  moderated live).
- CSAM perceptual-hash scanning at publish is a distinct, legally-required pass.

## Rules
Inherit all root invariants and always-do rules from `../../CLAUDE.md`. Crypto,
billing/entitlement, and moderation-bypass changes are HIGH-RISK — auditor required.
