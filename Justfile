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

# Advisories, bans, licences and sources against the committed deny.toml.
deny:
    cargo deny check advisories bans licenses sources

# Mode 1 stays free of the networking stack (ADR-0030).
net-free:
    scripts/check-net-free.sh

# Both WASM builds into dist/, ending with the 8 MB brotli size gate.
wasm:
    scripts/build-wasm.sh

# The local gate: what CI checks, except the WASM builds (`just wasm`, its own CI job).
ci: lint test-all deny net-free
