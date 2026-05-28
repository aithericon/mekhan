# Setup

## Prerequisites

Top-level tools: `docker`, Rust stable (`cargo`), Node ≥22, [`just`](https://github.com/casey/just), `git`.

Cargo doesn't fetch these — they must be present at build time (and, for HDF5/NetCDF, at runtime too):

| Native dep | Why it's needed | macOS (brew) | Debian/Ubuntu (apt) | Alpine (apk) |
|---|---|---|---|---|
| **HDF5 + NetCDF** | `aithericon-file-metadata` ([shared/file-metadata/Cargo.toml:50](../shared/file-metadata/Cargo.toml#L50)) is pulled with `all-backends` by [executor/Cargo.toml:110](../executor/Cargo.toml#L110); the `netcdf` backend links `libnetcdf` + `libhdf5` via `hdf5-metno-sys`. Required transitively by `executor-storage`, so disabling executor features does **not** drop it. | `hdf5 netcdf` | `libhdf5-dev libnetcdf-dev` | `hdf5-dev netcdf-dev` |
| **`protoc`** (protobuf compiler) | `aithericon-executor-ipc`'s `build.rs` runs `prost-build` to generate the FlatBuffers/Protobuf sidecar protocol. | `protobuf` | `protobuf-compiler` | `protobuf-dev` |
| **`cmake` + `pkg-config`** | Various transitive `*-sys` build scripts. | `cmake pkg-config` | `cmake pkg-config` | `cmake pkgconf` |
| **C/C++ toolchain** | Linking the native libs above. | Xcode Command Line Tools | `build-essential` | `build-base` |
| **`pnpm` 10.33.0** | Matches the `packageManager` pin in [app/package.json](../app/package.json#L9). | `pnpm` (or `corepack enable && corepack prepare pnpm@10.33.0 --activate`) | corepack | corepack |

TLS uses `rustls` throughout — no `libssl-dev` / OpenSSL needed.

## One-liner per OS

**macOS:**
```bash
brew install hdf5 netcdf protobuf cmake pkg-config pnpm just
```

**Debian/Ubuntu:**
```bash
apt-get install -y libhdf5-dev libnetcdf-dev protobuf-compiler \
                   cmake pkg-config build-essential just nodejs
corepack enable && corepack prepare pnpm@10.33.0 --activate
```

**Alpine (for slim Docker images):**
```bash
apk add --no-cache hdf5-dev netcdf-dev protobuf-dev \
                   cmake pkgconf build-base just nodejs npm
corepack enable && corepack prepare pnpm@10.33.0 --activate
```

## Docker image notes

- The executor binary dynamically links HDF5 + NetCDF, so the **runtime** stage of a multi-stage image also needs `libhdf5` + `libnetcdf` (not just the `-dev` packages). To eliminate them entirely, replace `"all-backends"` in [executor/Cargo.toml:110](../executor/Cargo.toml#L110) with an explicit feature list omitting `"netcdf"` (see the full list at [shared/file-metadata/Cargo.toml:133](../shared/file-metadata/Cargo.toml#L133)).
- Production target is `aarch64-unknown-linux-musl` (see [flake.nix](../flake.nix) and `Cross.toml`). The Nix dev shell already wires up the cross-linker; outside Nix, use [`cross`](https://github.com/cross-rs/cross).
- Nix users get everything (rust, node, just, cross-cc) from `nix develop`.
