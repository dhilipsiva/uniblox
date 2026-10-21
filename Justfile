# uniblox recipes, from products/templates/Justfile. Run them inside `nix develop` (or an allowed
# direnv), which provides the toolchain, coturn, redis, cargo-deny and the WASM tools.
# `just ci` is the one local gate, and CI runs the same recipe.

set shell := ["bash", "-euo", "pipefail", "-c"]

export RUST_BACKTRACE := "1"

# List the recipes.
default:
    @just --list

# Type-check every crate and target.
check:
    cargo check --locked --workspace --all-targets

# The tests that need no external service (the coturn and redis tests are #[ignore]d).
test:
    cargo test --locked --workspace

# Every test, including the coturn and redis ones (needs turnserver and redis-server on PATH).
test-all:
    cargo test --locked --workspace -- --include-ignored

# Formatting and Clippy, with warnings as errors.
lint:
    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets -- -D warnings

# Build the docs, with warnings as errors.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps

# Advisories, bans, licences and sources against the committed deny.toml.
deny:
    cargo deny check advisories bans licenses sources

# The sandbox's rhai features stay off: neither `unchecked` nor `internals` (ADR-0046).
sandbox-features:
    scripts/check-rhai-features.sh

# Measure what the sample game's script costs per call and per 64 Hz tick (ADR-0046's escape-hatch
# trigger). Release build: debug numbers are 10x slower and meaningless here.
script-cost:
    cargo run --locked --release -p scripting --example script_cost

# The worst-case script recursion fits a 1 MiB stack (the wasm32 default) in release (ADR-0046).
stack-check:
    cargo run --locked --release -p scripting --example stack_check -- 1024

# The measured inputs of the Mode-3 per-session cost model (docs/cost-model.md, ADR-0052): a real
# server process per session with 1, 4 and 8 clients. Release build; Linux (it reads /proc).
session-cost:
    cargo build --locked --release -p server --bin server --example session_cost
    "${CARGO_TARGET_DIR:-target}/release/examples/session_cost"

# The deployable binaries and their OCI images (ADR-0052); `docker load < result` loads an image.
images:
    nix build .#server
    nix build .#server-image -o result-server-image
    nix build .#services-image -o result-services-image

# Mode 1 stays free of the networking stack (ADR-0030).
net-free:
    scripts/check-net-free.sh

# Both WASM builds into dist/, ending with the 8 MB brotli size gate.
wasm:
    scripts/build-wasm.sh

# Browser acceptance (inside `nix develop .#browser`, after `just wasm`): the first frame, an IndexedDB
# save -> reload -> load, and two tabs exchanging both channels, in headless Chromium.
browser-e2e:
    python3 scripts/browser-e2e.py

# The local gate: what CI checks, except the WASM builds (`just wasm`, its own CI job).
ci: lint test-all doc deny net-free sandbox-features stack-check
