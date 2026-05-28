# Shared cargo recipes — single source of truth for build/check/fmt/lint/test/clean.
#
# Imported (not `mod`-ed) by engine/justfile and executor/justfile via
# `import '../just/cargo.just'`. With `import`, these recipes are inlined into
# the importing justfile's namespace and run with CWD = the importing justfile's
# directory (engine/ or executor/), which is exactly where cargo must run.
#
# `cargo_scope` selects what `build` operates on. `just` forbids redefining an
# imported variable, so this module does NOT define it — each importing justfile
# MUST set `cargo_scope` before the `import` line:
#   - executor sets `--workspace`  (its historical `build` behavior)
#   - engine   sets `-p core-engine` (its historical `build` behavior)
# `check`/`fmt`/`lint`/`test`/`clean` are workspace-wide in both workspaces, so
# they are not parameterized.

# Build crates (scope set by the importer's `cargo_scope`)
build:
    cargo build {{cargo_scope}}

# Type-check all crates
check:
    cargo check --workspace

# Format all code
fmt:
    cargo fmt --all

# Lint all code (clippy, warnings are errors)
lint:
    cargo clippy --workspace -- -D warnings

# Run all workspace tests
test:
    cargo test --workspace

# Run a specific crate's tests
test-crate name:
    cargo test -p {{name}}

# Clean build artifacts
clean:
    cargo clean
