{
  description = "Aithericon Platform — dev + CI toolchain";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, fenix }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        # Rust stable + aarch64-unknown-linux-musl target via fenix.
        # Adding the std lib for the target lets `cargo build --target ...`
        # work without rustup.
        rustToolchain = fenix.packages.${system}.combine [
          fenix.packages.${system}.stable.toolchain
          fenix.packages.${system}.targets.aarch64-unknown-linux-musl.stable.rust-std
        ];

        # Cross-stdenv for aarch64-musl. Gives us the gcc/ld/ar prefixed for
        # the target triple — Cargo uses these to link the static binary.
        crossPkgs = pkgs.pkgsCross.aarch64-multiplatform-musl;
        crossCC = crossPkgs.stdenv.cc;
        crossPrefix = "${crossCC.targetPrefix}";
      in {
        devShells.default = pkgs.mkShell {
          packages = [
            # Rust
            rustToolchain
            crossCC

            # Frontend
            pkgs.nodejs_22

            # Task runner + image build
            pkgs.just
            pkgs.docker-client
            pkgs.docker-buildx

            # Misc
            pkgs.jq
            pkgs.curl
            pkgs.git
            pkgs.pkg-config
          ];

          shellHook = ''
            # Point cargo at the cross-linker for the aarch64-musl target.
            export CC_aarch64_unknown_linux_musl="${crossCC}/bin/${crossPrefix}cc"
            export CXX_aarch64_unknown_linux_musl="${crossCC}/bin/${crossPrefix}c++"
            export AR_aarch64_unknown_linux_musl="${crossCC}/bin/${crossPrefix}ar"
            export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="${crossCC}/bin/${crossPrefix}cc"

            # Honour CARGO_HOME/NPM_CONFIG_CACHE injected by CI; default to
            # repo-local paths so dev users don't pollute $HOME.
            export CARGO_HOME="''${CARGO_HOME:-$PWD/.cache/cargo}"
            export NPM_CONFIG_CACHE="''${NPM_CONFIG_CACHE:-$PWD/.cache/npm}"
          '';
        };
      });
}
