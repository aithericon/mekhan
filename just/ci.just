# =============================================================================
# CI Pipeline Recipes
# =============================================================================
# Single source of truth for all CI logic. Called by .woodpecker/*.yml.
# Recipes are designed to run identically locally and inside the CI builder
# image — invoke via `just ci::<recipe>`.
#
# Module note: just modules execute with CWD = the module file (`just/`), so
# shebang recipes start with `cd ..` to operate from the repo root.
# =============================================================================

set shell := ["bash", "-euo", "pipefail", "-c"]

# Cross-compilation target — also lives in the top-level justfile; redefined
# here because module-level variables aren't inherited.
target_triple := env_var_or_default("TARGET_TRIPLE", "aarch64-unknown-linux-musl")
platform      := "linux/arm64"

# ── Quality (lint / format / type-check) ────────────────────────────────────

# Validate every Woodpecker pipeline file against the upstream schema.
# Run before pushing to catch syntax / deprecation errors locally.
lint-woodpecker:
    #!/usr/bin/env bash
    cd ..
    fail=0
    for f in .woodpecker/*.yml; do
      echo "=== $f ==="
      woodpecker-cli lint "$f" || fail=1
    done
    exit "$fail"

# cargo fmt + clippy on the workspace
quality-rust:
    #!/usr/bin/env bash
    cd ..
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings

# svelte-check on the SvelteKit app
quality-frontend:
    #!/usr/bin/env bash
    cd ../app
    npm ci
    npm run check

# ── Test ────────────────────────────────────────────────────────────────────

# cargo check on the workspace (lib + bins). Use this in CI until the
# pre-existing test compile errors in service/tests/ are resolved — running
# `cargo test` would fail at compile time, not on actual test logic.
check-rust:
    #!/usr/bin/env bash
    cd ..
    cargo check --workspace --lib --bins

# cargo test on the workspace
test-rust:
    #!/usr/bin/env bash
    cd ..
    cargo test --workspace --no-fail-fast

# Vitest unit tests
test-frontend-unit:
    #!/usr/bin/env bash
    cd ../app
    npm ci
    npm test

# Playwright e2e (requires a pre-built + served app — caller's responsibility)
test-frontend-e2e:
    #!/usr/bin/env bash
    cd ../app
    npm ci
    npx playwright test

# ── Build (cross-compile + frontend bundle) ─────────────────────────────────

# Cross-compile the service binary for aarch64 musl
# Output: target/${target_triple}/release/mekhan-service
build-backend:
    #!/usr/bin/env bash
    cd ..
    echo "▶ Cross-compiling mekhan-service for {{target_triple}}"
    SQLX_OFFLINE=true cargo build --release \
      --target {{target_triple}} \
      --manifest-path service/Cargo.toml \
      --bin mekhan-service
    ls -lh target/{{target_triple}}/release/mekhan-service

# Build the SvelteKit static SPA bundle
# Output: app/build/index.html + assets
build-frontend:
    #!/usr/bin/env bash
    cd ../app
    echo "▶ Building SvelteKit static SPA"
    npm ci
    npm run build
    if [ ! -f build/index.html ]; then
      echo "✗ Frontend build failed — app/build/index.html not produced" >&2
      exit 1
    fi
    echo "✓ Frontend built — $(du -sh build | cut -f1)"

# ── Package + push (Docker image) ───────────────────────────────────────────

# Stage prebuilt binary + frontend, build OCI image, tag with SHA + branch
# Args:
#   registry_image — full registry/namespace prefix, e.g. registry.aithericon.eu/mekhan
#   commit_sha     — git SHA (full or short)
#   ref_slug       — URL-safe branch name (CI_COMMIT_BRANCH)
package registry_image commit_sha ref_slug:
    #!/usr/bin/env bash
    cd ..
    sha_tag="{{commit_sha}}"
    ref_tag="{{ref_slug}}"
    ctx="/tmp/mekhan-build-context"

    rm -rf "$ctx"
    mkdir -p "$ctx/frontend"

    cp "target/{{target_triple}}/release/mekhan-service" "$ctx/mekhan-service"
    cp -r app/build "$ctx/frontend/build"

    echo "▶ docker buildx build → {{registry_image}}/mekhan-service:$sha_tag"
    docker buildx build \
      --platform {{platform}} \
      --load \
      -t "{{registry_image}}/mekhan-service:$sha_tag" \
      -t "{{registry_image}}/mekhan-service:$ref_tag" \
      -f deploy/docker/Dockerfile.service.prebuilt \
      "$ctx"

    rm -rf "$ctx"

# Push both tags of an already-packaged image
push registry_image commit_sha ref_slug:
    #!/usr/bin/env bash
    cd ..
    docker push "{{registry_image}}/mekhan-service:{{commit_sha}}"
    docker push "{{registry_image}}/mekhan-service:{{ref_slug}}"

# Build the fat CI builder image and push it to the registry
# Usage: just ci::build-ci-builder registry.aithericon.eu/mekhan
build-ci-builder registry_image:
    #!/usr/bin/env bash
    cd ..
    docker build \
      -f deploy/docker/Dockerfile.ci-build \
      -t "{{registry_image}}/ci-builder:latest" \
      .
    docker push "{{registry_image}}/ci-builder:latest"

# ── Convenience ─────────────────────────────────────────────────────────────

# Run the whole local pipeline: quality → test → build → package
build-all registry_image="registry.aithericon.eu/mekhan" commit_sha="local" ref_slug="local":
    just ci::quality-rust
    just ci::quality-frontend
    just ci::test-rust
    just ci::test-frontend-unit
    just ci::build-backend
    just ci::build-frontend
    just ci::package "{{registry_image}}" "{{commit_sha}}" "{{ref_slug}}"

# Clean build artefacts
clean:
    #!/usr/bin/env bash
    cd ..
    cargo clean --manifest-path service/Cargo.toml
    rm -rf app/build app/.svelte-kit
    rm -rf /tmp/mekhan-build-context
