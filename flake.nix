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

        # Combined hdf5 store path — nixpkgs splits hdf5 into a default
        # output (libs) and a `dev` output (headers). hdf5-metno-sys'
        # build.rs expects a single root with both `include/` and `lib/`,
        # so we symlink-join the two outputs and point `HDF5_DIR` at the
        # unified path below. `pkgs.netcdf` is single-output and already
        # ships both `include/` and `lib/` under one prefix, so it doesn't
        # need joining.
        hdf5Combined = pkgs.symlinkJoin {
          name = "hdf5-combined";
          paths = [ pkgs.hdf5 pkgs.hdf5.dev ];
        };
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

            # CI tooling
            pkgs.woodpecker-cli

            # Misc
            pkgs.jq
            pkgs.curl
            pkgs.git
            pkgs.pkg-config
          ];

          # Native libraries that -sys crates link against. nix's mkShell
          # adds each buildInput's pkgconfig dir to PKG_CONFIG_PATH and
          # exposes its dev headers / lib dirs to the linker. The
          # core-engine bin pulls in the zarrs/vtkio dep chain which links
          # against the netcdf C library (which itself needs hdf5).
          # Wiring them here keeps the build reproducible across nix / CI /
          # fresh machines and avoids the macOS-only brew fallback path
          # that netcdf-sys' build.rs hardcodes when neither pkg-config nor
          # NETCDF_DIR resolves the lib.
          buildInputs = [
            pkgs.netcdf
            pkgs.hdf5
          ];

          shellHook = ''
            # Point cargo at the cross-linker for the aarch64-musl target.
            export CC_aarch64_unknown_linux_musl="${crossCC}/bin/${crossPrefix}cc"
            export CXX_aarch64_unknown_linux_musl="${crossCC}/bin/${crossPrefix}c++"
            export AR_aarch64_unknown_linux_musl="${crossCC}/bin/${crossPrefix}ar"
            export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="${crossCC}/bin/${crossPrefix}cc"

            # hdf5-metno-sys + netcdf-sys ignore pkg-config and require a
            # single root dir containing both include/ and lib/. Point them
            # at the nix store paths so the build is reproducible without a
            # brew install (hdf5 needs the symlinkJoin combined output).
            export HDF5_DIR="${hdf5Combined}"
            export NETCDF_DIR="${pkgs.netcdf}"

            # Honour CARGO_HOME/NPM_CONFIG_CACHE injected by CI; default to
            # repo-local paths so dev users don't pollute $HOME.
            export CARGO_HOME="''${CARGO_HOME:-$PWD/.cache/cargo}"
            export NPM_CONFIG_CACHE="''${NPM_CONFIG_CACHE:-$PWD/.cache/npm}"
          '';
        };
      });
}
