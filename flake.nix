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

        # Pinned Rust toolchain (flake owns Rust — see DECISIONS.md ADR-0010). An exact version,
        # equal to `rust-version` in Cargo.toml (CONVENTIONS.md MSRV policy); Bevy 0.20 needs >= 1.97.1.
        rustToolchain = pkgs.rust-bin.stable."1.99.1".default.override {
          targets = [ "wasm32-unknown-unknown" ];
          extensions = [
            "rust-src"
            "clippy"
            "rustfmt"
          ];
        };

        # `wasm-bindgen-cli` MUST equal the `wasm-bindgen` crate in Cargo.lock (Bevy pulls the crate
        # in; Cargo.toml pins it with `=`), or the WASM build errors. The CLI is the versioned nixpkgs
        # attribute, so a nixpkgs bump cannot move it, and evaluating the dev shell fails loudly if
        # the two ever differ. To move both: change the `=` pin in Cargo.toml and this attribute.
        lockedWasmBindgen =
          (pkgs.lib.findFirst (p: p.name == "wasm-bindgen") (throw "Cargo.lock has no wasm-bindgen")
            (builtins.fromTOML (builtins.readFile ./Cargo.lock)).package
          ).version;
        wasmBindgenCli = pkgs.wasm-bindgen-cli_0_2_129;
      in
      nixpkgs.lib.recursiveUpdate
        {
          devShells.default =
            assert pkgs.lib.assertMsg (wasmBindgenCli.version == lockedWasmBindgen)
              "Cargo.lock has wasm-bindgen ${lockedWasmBindgen} but the flake's CLI is ${wasmBindgenCli.version}: update wasmBindgenCli in flake.nix together with the = pin in Cargo.toml";
            pkgs.mkShell {
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
        # Linux only: nixpkgs builds Chromium for Linux alone.
        (
          # On the system string, so listing shells never imports nixpkgs for another system.
          nixpkgs.lib.optionalAttrs (nixpkgs.lib.hasSuffix "-linux" system) {
            # The browser acceptance tests (`just browser-e2e`): the default shell plus headless Chromium and
            # the chromedriver of the same nixpkgs, so the two versions always match.
            devShells.browser = pkgs.mkShell {
              name = "uniblox-browser";
              inputsFrom = [ self.devShells.${system}.default ];
              packages = [
                pkgs.chromedriver
                pkgs.python3
              ];
              CHROMEDRIVER = "${pkgs.chromedriver}/bin/chromedriver";
              UNIBLOX_CHROMIUM = "${pkgs.chromium}/bin/chromium";
            };
          }
        )
    );
}
