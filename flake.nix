{
  description = "uniblox — reproducible dev shell (Rust + WASM toolchain), auto-activated via direnv";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Pinned Rust toolchain (flake owns Rust — see DECISIONS.md ADR-0010).
        # edition 2024 needs Rust >= 1.85; `stable.latest` is well past that.
        # Swap `latest` for a `"1.XX.0"` literal to pin an exact version.
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          targets = [ "wasm32-unknown-unknown" ];
          extensions = [
            "rust-src"
            "clippy"
            "rustfmt"
          ];
        };

        # GOTCHA: `wasm-bindgen-cli` MUST equal the `wasm-bindgen` *crate* version
        # in Cargo.lock (Bevy pulls the crate in), or the WASM build errors. Today
        # both are 0.2.121, but only because this nixpkgs rev ships that CLI. To pin
        # the CLI DECOUPLED from the nixpkgs rev, use the override below (uncomment +
        # fill the hashes nix prints on first build):
        wasmBindgenCli = pkgs.wasm-bindgen-cli;
        # wasmBindgenCli = pkgs.wasm-bindgen-cli.override {
        #   version = "0.2.100";            # == the wasm-bindgen crate version
        #   hash = pkgs.lib.fakeHash;       # nix prints the real hash on first build
        #   cargoHash = pkgs.lib.fakeHash;
        # };
      in
      {
        devShells.default = pkgs.mkShell {
          name = "uniblox-dev";

          # Rust is intentionally IN the flake (not ambient rustup). `nix develop` /
          # `use flake` is impure (additive PATH, nothing scrubbed), so the flake's
          # cargo/rustc/clippy/rustfmt win over rustup's when the env is active.
          packages = [
            rustToolchain
            wasmBindgenCli # wasm-bindgen
            pkgs.binaryen # wasm-opt
            pkgs.brotli # .br compression for the size table
            pkgs.twiggy # per-function byte attribution
            pkgs.nodejs_22 # node + npx for .mcp.json servers / Playwright
            pkgs.coturn # turnserver for the hermetic TURN-relay tests (ADR-0016)
            pkgs.redis # redis-server for the hermetic shared-registry tests (ADR-0040)
            pkgs.just # the local gate: `just ci` (Justfile), also what CI runs
            pkgs.cargo-deny # `just deny`: advisories, bans, licences, sources (deny.toml)
          ];

          shellHook = ''
            echo "uniblox devShell:"
            echo "  $(cargo --version)  |  wasm-bindgen $(wasm-bindgen --version 2>/dev/null | awk '{print $2}')  |  wasm-opt $(wasm-opt --version 2>/dev/null | awk '{print $3}')  |  node $(node --version)"
            echo "  reminder: wasm-bindgen-cli must equal the wasm-bindgen crate in Cargo.lock (see flake.nix)."
          '';
        };
      }
    );
}
