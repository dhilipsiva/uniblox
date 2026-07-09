# CONTEXT.md — the reasoning, constraints, and reality the build docs leave out

> Companion to the three build artifacts (the research prompt, the **BUILD SPEC**, and **TODO.md**). Those tell you *what* to build and *how* to drive Claude Code. They deliberately state decisions as settled facts. **This file holds what they omit**: *why* each decision was made, what was considered and rejected (so it isn't silently re-litigated), the hard limits the design lives inside, and — absent entirely from the technical docs — the commercial and strategic reality of the project.
>
> Read this before (a) changing any "fixed" decision, (b) letting a coding agent "improve" the architecture, or (c) deciding whether to keep going.

---

## 1. What this is (and what it is not)

**One-liner:** a browser-first, native-secondary, Rust→WASM platform for user-made multiplayer games, where the same authored game runs peer-to-peer or server-authoritative by changing only *who owns each object*.

**It is a platform, not a framework.** The litmus test that settles this cleanly is *who ships to the end user*. With a framework (Bevy is one), the developer builds a game and ships the finished artifact themselves. With a platform, creators publish content *into* your system and *you* run it and deliver it to players — you keep the runtime, the distribution chokepoint, and the end-user relationship. Everything distinctive here (central publish pipeline, content catalog, matchmaking, identity/billing, you hosting Mode 3 servers, moderation as your responsibility) is platform behavior.

**It is a stack of different things, which is why no single word fits:** a *game engine* (Bevy/wgpu — reused, not built), an *application framework / authoring contract* (the ownership-explicit sim model + whitelisted Rhai API creators plug into — this one layer genuinely is a framework), a *sandboxed application runtime* (Rhai host + replication substrate executing untrusted UGC), *networking middleware* (the custom replication protocol), and *backend platform services* (publish, accounts, billing, matchmaking, persistence, moderation).

**Reference class:** Roblox / Rec Room / Core are the structural match (engine + sandboxed scripting runtime + publish pipeline + services). Fantasy consoles (PICO-8, TIC-80) are the closest analogy to the *runtime + authoring core specifically* — a fixed sandboxed runtime + constrained scripting API + cartridge/bundle format + distribution channel.

**The one genuinely novel, distinctive claim** — the thing that makes this *this* and not "Roblox in Rust" — is the **authority-agnostic replication substrate**: the same authored logic runs unchanged across three deployment topologies (local / distributed-peer / central-authoritative) with authority assignment as the *only* variable. This has no standard name. It is also unproven — see §4 and the TODO's Phase 1.

---

## 2. Settled invariants a contributor or agent is most likely to "helpfully" break

Each of these is load-bearing. The build docs enforce them; this explains *why*, so they survive contact with someone trying to be helpful.

- **No CRDT in the runtime.** Single-ownership per entity ⇒ no concurrent writes to the same datum ⇒ nothing to merge. CRDT was in an earlier design and was doing *no work* even in the P2P mode; it was overhead pretending to be the sync layer. It is permitted **only** in the separable collaborative-editing subsystem of the authoring tool. If someone proposes CRDT for gameplay sync, the ownership model already solved that problem.
- **No cross-platform determinism.** Receivers never re-simulate others' entities (they apply replicated state and interpolate), and prediction only touches entities you own — so no two machines must agree on a float. This is deliberate and is the reason lockstep was rejected (§3). Do not introduce any mechanism that requires browser/x86/ARM peers to compute identical float results.
- **WebRTC DataChannels only. No media, no SFU, anywhere.** Emoji-only social ⇒ no voice/video ⇒ no media path ⇒ no SFU/media-server machinery in any mode. Mode 3 is an authoritative *hub*, not an SFU relay.
- **Two WASM builds, not one.** Bevy cannot serve WebGPU and WebGL2 from a single binary (issue #13168 open; the `webgpu` feature overrides `webgl2`). Ship two builds + JS capability detection. Do not chase a single-binary runtime fallback assuming it exists.
- **Single-threaded WASM at launch.** Enabling SharedArrayBuffer threads requires COOP/COEP cross-origin isolation, which breaks the OAuth sign-in and payment-checkout popups Mode 3 needs. This is a hard trade, not an oversight.
- **Mode 3 is authoritative, and that is what the subscription sells.** If Mode 3 becomes "just a relay/SFU," the anti-cheat value evaporates and you are charging money for the weaker guarantee.
- **Custom replication, not an off-the-shelf netcode crate.** Verified: no existing Rust crate backs all three modes over WebRTC DataChannels by varying only authority (§4, §8). Do not "simplify" by adopting lightyear/replicon as the cross-mode backbone.

---

## 3. Decision rationale & rejected alternatives

The build docs list the winners. Here is what lost, and why — so these don't get re-proposed as fresh ideas.

| Rejected option | Why it lost |
|---|---|
| **CRDT for gameplay sync** | Single-ownership means no concurrent writes ⇒ nothing to merge. It did no work even in P2P. Kept only for the collaborative editor. |
| **Lockstep / input-sync** | Would make cross-platform float determinism a *correctness requirement* (x86/ARM/WASM diverge on honest float math without fixed-point). Explicitly refused. Also perfect-information-only (no hidden-info genres). |
| **SFU for Mode 3** | An SFU is a relay — it forwards without validating, so it cannot produce real anti-cheat. Replaced by an authoritative headless-Bevy hub. (No media also means no SFU is needed at all.) |
| **Users author in Rust→WASM** | Compile times, toolchain friction, and the security of executing arbitrary user WASM. Interpreted + sandboxed (Rhai) is the proven model and is *why* Roblox forked Luau. |
| **Fyrox instead of Bevy** | Ships a scene editor (tempting), but the editor is native, not browser-embedded, and its WASM story is weaker — disqualifying for browser-first. |
| **Custom wgpu stack instead of Bevy** | YAGNI. Re-solves ECS, asset loading, and render-graph problems for control not yet needed. Revisit only if a concrete Bevy limitation blocks you. |
| **lightyear / bevy_replicon / renet as the replication backbone** | lightyear defers IO to `aeronet`, which has **no WebRTC-DataChannel layer**, and its distributed authority is untested; `bevy_replicon` is server→client-only. ⇒ custom protocol over matchbox/str0m. |
| **`bevy_mod_scripting` for the Rhai bridge** | No WASM support (issue #166) — disqualifying for browser-first. ⇒ thin custom bridge. |
| **Per-zone authority** | Coarser; pushes the P2P mode toward host-authority, contradicting the distributed-mesh intent. ⇒ per-entity authority + spatial interest management (AOI) as a separate layer. |
| **Authoritative-host-peer for anti-cheat** | Closes the peer-verification holes but discards the client-heavy/minimal-server thesis and makes the host both a cheat vector and a single point of failure. Only revisit if competitive ambition appears. |
| **Byzantine-tolerant signed-op CRDT + quorum for Mode 3** | Inherits the Sybil/collusion ceiling and the determinism tax, and caps anti-cheat *below* server-authority — i.e. you'd charge money for the mode with the weaker guarantee. ⇒ authoritative server. |

---

## 4. The trust & anti-cheat model — and the envelope it is valid inside

This is the most important reasoning the technical docs under-weight. The anti-cheat design is **not** "best-effort security that will improve with engineering." It has a **structural** ceiling, and the whole design is only sound inside a specific envelope.

**Peer verification (the free P2P mode) can detect inconsistency but cannot, by itself, attribute blame.** Two peers disagreeing is a standoff; nothing says which one lied. You escape that only with N-player quorum (majority recomputes and outvotes the liar), and quorum has two defeats you cannot close without a server: **Sybil** (with anonymous free-tier identity, one attacker runs many fake peers and *becomes* the majority) and **collusion** (a cheating clique outvotes honest players).

**Hidden-information cheats are unenforceable in P2P.** Every peer needs enough world state to simulate/verify, so the full state sits on every potentially-malicious machine. Any cheat that only *reads* data the client legitimately holds — wallhack, maphack, aimbot, seeing another player's position — is undetectable, because the cheater isn't lying about anything. Server-authority + fog-of-war exists precisely because this class is otherwise unwinnable.

**Therefore the design is sound *only* inside this envelope:** low-stakes, casual/creative/co-op games, small sessions, **no hidden information**, and **no real-money economy**. Competitive play is gated into paid Mode 3, where an authoritative server provides ground truth *and* payment provides Sybil resistance. Framed honestly, anti-cheat in the free modes is **cost-imposition, not prevention** — for a no-economy sandbox that's genuinely good enough, because most players won't bother and the ones who do only grief a single session. **It does not survive contact with competitive play, and it must never be relied on as if it did.**

**Two ceilings to accept, never "fix":**
- **Browser clients cannot be attested** (WASM/JS is inspectable/modifiable; no secure attestation). Mode 3 "max anti-cheat" means *max achievable in a browser* — server-authoritative simulation — which is structurally weaker than native games with kernel-level anti-cheat.
- **The Rhai sandbox is not anti-cheat.** It protects a player's *machine* from malicious *content*; it does nothing against a player running a *modified client*. Orthogonal problems.

---

## 5. Commercial & strategic reality (absent from every technical report)

The technical docs answer "can this be built." They do not answer "is this worth building or sellable," which was a large part of the conversation. Confidence here is **medium at best on the reasoning, low on any number** — distrust anyone (including this doc) who gives precise odds on a startup outcome.

**You cannot sell the *design*. Its transferable value is ~$0.** Acquirers pay for one of four things: revenue, users/traction, defensible technology, or talent. A design is none of them. Designs aren't defensible — a rational buyer reads the idea and rebuilds it, because the design is the cheap part and execution is the whole cost. The one thing that's yours and not copyable is your ability to *execute* this.

**A UGC platform's entire value is the two-sided network** (creators making games + players playing them). An empty platform with every feature working is precisely the part nobody wanted to build — the plumbing is a means to the network, and you can't sell the means without the end. This is the category's defining trap.

**The category is a graveyard, and buyers know it.** Stadia, Core/Manticore, Horizon, and especially **SpatialOS/Improbable** (raised over a billion dollars on essentially "revolutionary multiplayer substrate" and pivoted away from that exact pitch) all had *working tech and no durable network*. An acquirer's first question about a prototype is not "does it work" but "will anyone build games on it and will anyone play them" — which a feature-complete demo answers with a story, not evidence.

**A fully-working prototype *is* sellable — but as *technology + talent*, not as a platform business.** It sells to a **game-engine/middleware vendor** (who'd fold in the netcode substrate and largely ignore the platform layer), an existing UGC/metaverse platform, or a studio betting on browser-native multiplayer — at *technology-and-team* value, not platform-business multiples, because a platform with no users isn't a business yet.

**The value curve (each step ~an order of magnitude over the last):**
`design (~$0)` → `working authority-swap slice (de-risked novel tech)` → `launched platform with real creators/traction (first genuine acquisition interest)` → `Mode 3 revenue (a business with a multiple)`. **Traction is the discontinuity.** Everything before it sells as tech+team; only after it do you sell "a platform."

**~80% of the acquirable value sits in the authority-swap vertical slice, which is ~15% of the build.** Building the entire spec *in order to sell it* optimizes the wrong thing: it maximizes effort while the value curve is flattest. If the goal is to sell, build the slice, take it to the handful of engine/middleware players it would interest, and let *their* interest set the price.

**Monetization odds, decomposed** (the single question "can I monetize this" hides three very different ones):
- *Any revenue at all* — **high**, conditional on shipping (charge Mode 3 the day it works). Nearly meaningless as a goal.
- *Self-sustaining / ramen-profitable (a salary or small team)* — **low-to-moderate**; a rough prior is low-single-digit to low-double-digit percent for a solo-built consumer platform. This is the question that matters, and it's dominated by demand and cold-start risk that no engineering resolves.
- *Venture-scale* — **very low (<1%)**, the honest base rate for any new consumer platform before the category penalty.

**The structural monetization problem:** the free tiers (Modes 1–2) are where *all* adoption happens; the paid tier (Mode 3) is the hard conversion. Competitive integrity only becomes worth paying for *after* a game has enough players that competition matters — which requires the free network to thrive first. Revenue is gated behind network liquidity, gated behind creators making good games, gated behind the platform being worth building on. Multiplied conditional probabilities get small fast.

**You removed the monetization that actually works for this category.** UGC platforms monetize through the *economy* — cosmetics, creator payouts, currency, marketplace fees (Roblox = Robux + a cut of a creator economy, *not* "pay for authoritative servers"). Scoping out the real-money economy (subscription-only) is a defensible product decision that dodges enormous trust-and-safety and regulatory burden — but it also removes the proven revenue mechanism and keeps the one that historically underperforms. **This is the biggest revenue-specific risk.**

**The wedge is the highest-leverage lever on the middle probability.** Do *not* launch a general-purpose empty platform head-on against Roblox (near-hopeless for a solo builder). Find one specific game, genre, or creator community for which browser-native, no-install, opt-in-competitive multiplayer is *meaningfully better* than the alternatives. This single factor swings the odds more than any other, and it is currently **unanswered** (see §9).

**Cold-start is the other platform-killer.** Empty catalog / empty lobby. You need a concrete seeding answer (build the first games yourself, or court a specific small creator community) before the probability means anything.

**The honest reframe you were left with:** the value of building this was never primarily the expected revenue. On pure expected-monetary-value, a solo consumer-platform build is usually *negative*, and that should be known going in. It's the right thing to build anyway in two cases: (a) the wedge is real *and* you've validated a sliver of demand before committing years; or (b) you'd build it for reasons other than revenue — the systems problem is one you *want* to solve, the skills and the artifact compound into your career regardless of outcome, and the authority-swap is a genuinely novel thing you'd be proud to have built. Those are legitimate. **"It will probably make good money" is not a reason the odds support.**

---

## 6. Scope & effort reality

Rough, solo, order-of-magnitude, low-confidence (scope-dependent):
- Single-player 3D scene in-browser with GPU: **days–weeks**.
- + physics/input/gameplay loop: **weeks–months**.
- + sandboxed scripting so *you* can script games: **months**.
- + authoritative multiplayer: **several more months** (netcode is unforgiving).
- + an in-browser creation editor for *other people*: the Roblox-defining leap, **1–2+ years**, arguably never "done."
- + discovery/economy/moderation at scale: **multi-year, team**.

**The 10/90 novelty split:** ~90% of the build spec is competent assembly of existing pieces (Bevy, matchbox, str0m, Rhai, Postgres, coturn) — work, but not defensible or premium-worthy because a buyer could assemble it too. ~10% — the authority-agnostic replication substrate — is the novel, defensible, valuable part. Spend the differentiation budget accordingly.

**The moment you host UGC you inherit a non-optional, legally serious trust-and-safety burden** (illegal-content scanning, moderation pipeline, reports/appeals). P2P sessions give *zero* real-time moderation vantage, which is why the publish pipeline is the sole chokepoint. This is routinely under-weighted and is real.

---

## 7. Naming state

Constraints were: ≤8 chars, one pronounceable word, coined/inventable, and — critically — free `.com` + crates.io + GitHub org. That combination is nearly unsatisfiable; short pronounceable `.com`s are almost universally squatted, and short GitHub handles are almost all claimed.

- **`skeinia`** — the only candidate free on all three including `.com`. (Skein = a coil of woven thread *and* the word for a flock of geese in V-formation — threads-in-a-mesh + flocking-without-a-center. Reads as a clean brandable coinage.)
- **`manifld`** — the stronger *name* (a manifold is locally simple, globally complex = each world local-simple, the platform the global fabric; also "many-fold" = many worlds), free on crate + GitHub + `.dev`/`.io`, but **not** `.com`. `manifld.rs` was untested (Serbia's registry wasn't in the RDAP path) and would be thematically perfect for a Rust project — check it manually.

Decision rule: if `.com` is sacred → `skeinia`; if it can flex to `.rs`/`.dev` → `manifld` is the better name. No trademark screening was done (a separate required gate; `manifld` is phonetically "manifold," and Manifold Markets/Finance exist in other sectors).

---

## 8. Verified-facts & corrections ledger (as of the research, ~mid 2026)

Load-bearing current facts the build docs rely on. **Re-verify before pinning versions** — the Bevy ecosystem moves fast.

- **Bevy 0.19.0** (released 2026-06-19); ~3-month breaking-release cadence; still pre-1.0.
- **WebGPU/WebGL2:** Bevy issue **#13168 open**; two WASM builds + JS capability detection required. (Browser WebGPU support is broad but incomplete — Chrome/Edge since 2023, Safari 26, Firefox partial; WebGL2 fallback is still needed.)
- **lightyear (~0.26.4)** defers IO to `aeronet`, which has **no WebRTC-DataChannel layer**; its distributed authority is untested. **bevy_replicon** is server→client-only. ⇒ custom replication.
- **matchbox** natively supports the two required channels (reliable + unreliable), works browser + native, ships a signaling server. (One source report wrongly called it single-channel.)
- **str0m** is sans-IO, lock-free, supports DataChannels, production SFU pedigree; its *P2P* path is less battle-tested — budget integration time.
- **bevy_mod_scripting** lacks WASM support (issue #166) ⇒ thin custom Rhai bridge.
- **WASM size** is a measurement gap: optimized Bevy apps are large (roughly the ~15–30 MB range pre-compression per community reports; brotli helps); **unoptimized dev builds exceed 100 MB** (a Bevy tutorial build measured ~160 MB). Instrument it in the slice; don't trust the optimistic sub-3 MB figures some reports gave.
- **STUN-only failure** ~**15–25%** of peer connections (region-neutral); those peers silently fail on free tiers, Mode 3 provides TURN.
- **ed25519** ≈ 15–16 µs sign / ~46 µs verify *native* single; WASM (single-thread, no AVX2) is several× slower — measure before committing to per-frame state-channel signing.

**What the five source reports got wrong** (so nobody trusts the stale copies): DeepSeek/ChatGPT ran on stale versions (Bevy 0.15, lightyear 0.17) and a false "wgpu single-binary fallback" claim; Grok wrongly asserted lightyear can back all three modes over WebRTC-DC; Gemini's WASM sizes were optimistic, its ">30% symmetric-NAT" was region-specific, and its "enable COOP/COEP threads" recommendation would break the OAuth/payment popups Mode 3 needs; DeepSeek's "matchbox is single-channel" was false.

---

## 9. Assumed defaults (choices, not deductions) & genuinely open questions

**Defaults chosen for you — reasonable, but reversible:**
- **Per-entity authority + spatial AOI** (over per-zone). Fits Bevy's ECS and avatar-ownership; AOI does double duty as the Mode 3 read-cheat defense.
- **Entity default-owned by its spawner/controller;** ownership handoff is an explicit reliable-channel event, coordinator-arbitrated (lowest-peer-ID tiebreak) in P2P.
- **~20–30 Hz network tick, client interpolates to display rate** — a starting point to measure, not a tuned value.
- **Full-mesh session cap ~8 peers** (soft, upstream-bandwidth-bound).

**Genuinely open (unanswered, and some are load-bearing):**
- **The wedge** — which specific game/genre/community. Unanswered, and it dominates the commercial odds (§5).
- **Cold-start seeding strategy** — how the first catalog and first lobbies get populated.
- **Whether to ever add competitive ambition** — doing so would force a host-authority or server model and break the free-tier trust assumptions (§4). Decide consciously.
- **Rhai performance escape-hatch trigger** — at what point script-bound logic must move to Rust systems or a faster path.
- **Final crate version pins** — verify current Bevy compatibility for every crate before locking.
- **Whether the goal is to sell, to run, or to raise** — this changes the whole plan (build-the-slice-and-sell vs build-everything-and-pursue-traction). The technical docs assume "build it"; the commercial reasoning (§5) assumes you might not want to.

---

## 10. Builder context that shaped these decisions

Project-relevant only (not a profile):
- Strong Rust / systems / distributed-systems background — the design leans minimal-abstraction and first-principles deliberately, matching a YAGNI, directness-over-comfort working style. Keep the abstractions thin.
- **`nibli`** (github.com/dhilipsiva/nibli) already implements a **WebRTC P2P gossip transport** (NAT traversal, browser-native, no central relay) — this is directly reusable prior art for the transport/signaling layer, and the P2P-first instinct throughout this design traces to it.
- Preference for metric units and for explicit epistemic labeling (fact / inference / assumption / speculation) with stated confidence — this doc and the build docs follow that; maintain it.

---

*This file is reasoning and context, not instructions. Where it conflicts with a later explicit decision you make, the decision wins — but record the new decision and the reason here, so the next reader (human or agent) inherits the why, not just the what.*
