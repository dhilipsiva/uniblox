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
        # once Bevy pulls it in transitively, or the WASM build errors. No Bevy yet
        # => no wasm-bindgen crate => any recent CLI passes build-wasm.sh's presence
        # guard. When Bevy lands, read the crate version from Cargo.lock and pin the
        # CLI here, DECOUPLED from the nixpkgs rev (uncomment + fill the hashes nix
        # prints on first build):
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
          ];

          shellHook = ''
            echo "uniblox devShell:"
            echo "  $(cargo --version)  |  wasm-bindgen $(wasm-bindgen --version 2>/dev/null | awk '{print $2}')  |  wasm-opt $(wasm-opt --version 2>/dev/null | awk '{print $3}')  |  node $(node --version)"
            echo "  reminder: wasm-bindgen-cli must match the wasm-bindgen crate in Cargo.lock once Bevy is added."
          '';
        };
      }
    );
}
