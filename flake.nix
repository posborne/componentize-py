{
  description = "A Python component tooling prototype";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (rust-overlay.overlays.default) ];
        pkgs = import nixpkgs {
          inherit system;
          inherit overlays;
        };

        wasi-sdk-src = pkgs.fetchurl {
          url = "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-27/wasi-sdk-27.0-x86_64-linux.tar.gz";
          sha256 = "b7d4d944c88503e4f21d84af07ac293e3440b1b6210bfd7fe78e0afd92c23bc2";
        };

        wasi-sdk = pkgs.stdenv.mkDerivation {
          name = "wasi-sdk";
          src = wasi-sdk-src;
          installPhase = ''
            mkdir -p $out
            tar -xzf $src -C $out --strip-components=1
          '';
        };

      in
      {
        devShell = pkgs.mkShell {
          buildInputs = [
            pkgs.rustup
            pkgs.clang
            pkgs.python3
            pkgs.gnumake
            pkgs.openssl
            pkgs.pkg-config
          ];

          shellHook = ''
            # Set up a local rustup directory to avoid conflicts
            export RUSTUP_HOME=$(pwd)/.rustup

            # Ensure nightly toolchain is available through rustup
            if ! rustup toolchain list | grep -q "nightly"; then
              echo "Installing nightly toolchain..."
              rustup toolchain install nightly --component rust-src --target wasm32-wasip1
            fi

            # Make sure we have the required components and targets for both
            # stable and nightly
            rustup component add --toolchain nightly rust-src
            rustup target add --toolchain nightly wasm32-unknown-unknown wasm32-wasip1
            rustup component add rust-src rust-analyzer
            rustup target add wasm32-unknown-unknown wasm32-wasip1

            echo "Rust environment ready:"
            echo "  Nightly: $(rustup run nightly rustc --version)"
            echo "  WASI SDK: ${wasi-sdk}"
          '';

          WASI_SDK_PATH = "${wasi-sdk}";
        };
      }
    );
}
