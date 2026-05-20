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
    pnpm install --frozen-lockfile
    pnpm run check

# Regenerate the OpenAPI spec + TS schema and fail if they differ from
# what's committed. Catches PRs that change a Rust DTO / handler but forget
# `just dev::openapi`. Run inside the CI builder so `cargo build` + the
# `openapi-typescript` devDep are both available.
openapi-drift:
    #!/usr/bin/env bash
    cd ..
    echo "▶ Rebuilding mekhan CLI and dumping spec…"
    cargo build --bin mekhan
    ./target/debug/mekhan openapi > /tmp/openapi-mekhan.regen.json
    if ! diff -u openapi-mekhan.json /tmp/openapi-mekhan.regen.json; then
      echo "✗ openapi-mekhan.json is stale. Run \`just dev::openapi\` and commit." >&2
      exit 1
    fi
    echo "▶ Rebuilding TS schema and diffing…"
    (cd app && pnpm install --frozen-lockfile && pnpm run openapi:generate)
    if ! git diff --exit-code app/src/lib/api/schema.d.ts; then
      echo "✗ app/src/lib/api/schema.d.ts is stale. Run \`just dev::openapi\` and commit." >&2
      exit 1
    fi
    echo "✓ OpenAPI spec + TS schema are in sync."

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

# Layer-2 real-Zitadel e2e: bring up the full stack, bootstrap Zitadel
# (provisions the introspection API app + writes mekhan.local.toml), then
# run the MEKHAN_E2E_ZITADEL-gated test which mints/uses/revokes a live
# service-user PAT. Heavy + Docker-dependent — its own gated lane, never a
# PR gate. `down -v` always, so a fresh Zitadel each run.
test-e2e-zitadel:
    #!/usr/bin/env bash
    set -euo pipefail
    cd ..
    cleanup() { docker compose down -v >/dev/null 2>&1 || true; }
    trap cleanup EXIT
    echo "▶ infra + zitadel up…"
    docker compose up -d
    echo "▶ waiting for zitadel…"
    for _ in $(seq 1 90); do
      curl -fsS http://localhost:8080/debug/ready >/dev/null 2>&1 && break
      sleep 2
    done
    echo "▶ waiting for postgres…"
    for _ in $(seq 1 30); do
      docker compose exec -T postgres pg_isready -U mekhan -d mekhan >/dev/null 2>&1 && break
      sleep 1
    done
    echo "▶ bootstrap zitadel (idempotent)…"
    bash deploy/zitadel/bootstrap.sh
    echo "▶ running gated e2e…"
    TEST_POSTGRES_URL="postgres://mekhan:mekhan@localhost:5439/mekhan" \
    TEST_NATS_URL="nats://localhost:4333" \
    TEST_S3_ENDPOINT="http://localhost:9099" \
    MEKHAN_E2E_ZITADEL=1 \
      cargo test --test zitadel_e2e -- --test-threads=1 --nocapture

# Vitest unit tests
test-frontend-unit:
    #!/usr/bin/env bash
    cd ../app
    pnpm install --frozen-lockfile
    pnpm test

# Playwright e2e (requires a pre-built + served app — caller's responsibility)
test-frontend-e2e:
    #!/usr/bin/env bash
    cd ../app
    pnpm install --frozen-lockfile
    pnpm exec playwright test

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
    # CI=true tells pnpm it's running non-interactively. Without it, pnpm
    # aborts (no TTY) when it detects that existing node_modules came from a
    # different platform — which is exactly what happens when this recipe
    # runs in the linux CI builder over a node_modules previously populated
    # on macOS. With CI=true pnpm silently purges + reinstalls cleanly.
    export CI=true
    pnpm install --frozen-lockfile
    pnpm run build
    if [ ! -f build/index.html ]; then
      echo "✗ Frontend build failed — app/build/index.html not produced" >&2
      exit 1
    fi
    echo "✓ Frontend built — $(du -sh build | cut -f1)"

# ── GitOps: publish git-authored workflow templates ─────────────────────────

# Build the mekhan CLI and `apply` every template directory under `dir` (each
# must contain a committed `.mekhan.json`). Each apply atomically versions +
# publishes the chain from the git artifact, recording commit provenance.
# Auth: MEKHAN_CLI_TOKEN — a Zitadel service-user PAT the server validates
# via RFC 7662 introspection. Target server is the `server_url` baked into
# each `.mekhan.json`.
apply-workflows dir="workflows":
    #!/usr/bin/env bash
    cd ..
    echo "▶ Building mekhan CLI"
    SQLX_OFFLINE=true cargo build --release \
      --manifest-path service/Cargo.toml --bin mekhan
    bin=target/release/mekhan
    shopt -s nullglob
    found=0
    for d in {{dir}}/*/; do
      [ -f "${d}.mekhan.json" ] || continue
      found=1
      echo "▶ mekhan apply ${d}"
      "$bin" apply "$d"
    done
    if [ "$found" -eq 0 ]; then
      echo "✗ No template directories with .mekhan.json under {{dir}}/" >&2
      exit 1
    fi
    echo "✓ Applied all workflow templates under {{dir}}/"

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

# Build + push the aithericon-executor image.
#
# Unlike `ci::package`, this builds Rust INSIDE the Docker image (multi-stage
# Dockerfile.executor) rather than from a prebuilt artifact — kreuzberg's
# native deps (tesseract, hdf5, …) make the cross-musl path painful, and the
# executor runs glibc-Debian in production anyway. Heavier build, ~5-8 min.
#
# Uses `docker buildx --push` so the push happens inline (no separate
# `push-executor` recipe needed). Caller still has to `docker login` first.
package-and-push-executor registry_image commit_sha ref_slug:
    #!/usr/bin/env bash
    set -euo pipefail
    cd ..
    sha_tag="{{commit_sha}}"
    ref_tag="{{ref_slug}}"

    echo "▶ docker buildx build + push → {{registry_image}}/aithericon-executor:$sha_tag"
    docker buildx build \
      --platform {{platform}} \
      --push \
      -t "{{registry_image}}/aithericon-executor:$sha_tag" \
      -t "{{registry_image}}/aithericon-executor:$ref_tag" \
      -f deploy/docker/Dockerfile.executor \
      .

# Build the fat CI builder image and push it to the registry.
#
# Every .woodpecker/*.yml step uses this image as `image:`, so rebuilding it
# is how you get a new toolchain / native dep into CI. Rebuild + push when
# deploy/docker/Dockerfile.ci-build changes.
#
# Usage:
#   docker login forge.aithericon.eu     # one-time, as the registry user
#   just ci::build-ci-builder forge.aithericon.eu/milanender
#
# The image is published as `<registry_image>/mekhan-ci-builder:latest`.
# `latest` is intentional — the woodpecker YAMLs pin to this tag, so the next
# pipeline run picks up your update without a coordinated YAML change. If you
# need atomic rollouts, switch the YAMLs to a SHA tag and produce that here.
build-ci-builder registry_image:
    #!/usr/bin/env bash
    cd ..
    docker build \
      -f deploy/docker/Dockerfile.ci-build \
      -t "{{registry_image}}/mekhan-ci-builder:latest" \
      .
    docker push "{{registry_image}}/mekhan-ci-builder:latest"

# ── Deploy (Terraform / Nomad) ──────────────────────────────────────────────
#
# Deploys the mekhan-service image to the dev or prod Nomad cluster. State
# lives in S3-compatible storage (rustfs / MinIO); the backend is configured
# via `backend.hcl` inside each layer — see deploy/{dev,prod}/backend.hcl.example.
#
# Secrets the deploy step expects (sourced from Woodpecker secrets in CI, or
# from your shell when running locally):
#
#   TF_VAR_nomad_token            Nomad ACL token
#   TF_VAR_registry_user          Forgejo registry username
#   TF_VAR_registry_password      Forgejo registry password
#   TF_VAR_database_url           Postgres URL for the target environment
#   TF_VAR_s3_access_key          rustfs access key (for the artifact bucket)
#   TF_VAR_s3_secret_key          rustfs secret key
#   AWS_ACCESS_KEY_ID             For the Terraform S3 *state* backend
#   AWS_SECRET_ACCESS_KEY         Same
#
# Non-secret config lives in {dev,prod}.auto.tfvars (gitignored — copy from
# the .example next to it).

# Plan + apply against the dev layer.
# `image_tag` is required; CI sets it to CI_COMMIT_SHA.
deploy-dev image_tag:
    #!/usr/bin/env bash
    set -euo pipefail
    cd ../deploy/dev
    if [ ! -f dev.auto.tfvars ]; then
      echo "✗ deploy/dev/dev.auto.tfvars missing. Copy dev.auto.tfvars.example." >&2
      exit 1
    fi
    echo "▶ tofu init (dev)"
    tofu init -input=false
    echo "▶ tofu plan (image_tag={{image_tag}})"
    tofu plan -input=false -var="image_tag={{image_tag}}" -out=plan.out
    echo "▶ tofu apply"
    tofu apply -input=false plan.out
    rm -f plan.out
    echo "✓ dev deploy applied"

# Plan + apply against the prod layer. Same shape as deploy-dev.
deploy-prod image_tag:
    #!/usr/bin/env bash
    set -euo pipefail
    cd ../deploy/prod
    if [ ! -f backend.hcl ]; then
      echo "✗ deploy/prod/backend.hcl missing." >&2
      exit 1
    fi
    if [ ! -f prod.auto.tfvars ]; then
      echo "✗ deploy/prod/prod.auto.tfvars missing." >&2
      exit 1
    fi
    echo "▶ tofu init (prod)"
    tofu init -input=false -backend-config=backend.hcl
    echo "▶ tofu plan (image_tag={{image_tag}})"
    tofu plan -input=false -var="image_tag={{image_tag}}" -out=plan.out
    echo "▶ tofu apply"
    tofu apply -input=false plan.out
    rm -f plan.out
    echo "✓ prod deploy applied"

# Poll Nomad until the latest deployment for the mekhan-service job reaches a
# terminal state. Used as the post-deploy gate in CI so a failing rollout
# fails the pipeline. NOMAD_ADDR + NOMAD_TOKEN (and NOMAD_NAMESPACE if set)
# must already be in the environment.
verify-deploy env="dev":
    #!/usr/bin/env bash
    set -euo pipefail
    cd ../deploy/{{env}}
    job=$(tofu output -raw job_id)
    ns=$(tofu output -raw job_namespace)
    echo "▶ waiting for Nomad deployment of $job ($ns) to settle…"
    # NOMAD_ADDR + NOMAD_TOKEN come from the caller's env (set by the
    # Woodpecker step). The Nomad CLI's `deployment status -latest` flag was
    # removed in 1.6+ so we query the HTTP API directly — that endpoint
    # always returns the latest deployment for the job.
    deadline=$(( $(date +%s) + 300 ))
    while :; do
      status=$(curl -sk --max-time 10 \
                 -H "X-Nomad-Token: ${NOMAD_TOKEN:-}" \
                 "${NOMAD_ADDR}/v1/job/${job}/deployment?namespace=${ns}" \
               | jq -r '.Status // ""')
      case "$status" in
        successful)         echo "✓ deployment successful"; exit 0 ;;
        failed|cancelled)   echo "✗ deployment $status"; exit 1 ;;
        running|"")         : ;;  # still rolling out
        *)                  echo "  status=$status (continuing to poll)" ;;
      esac
      [ "$(date +%s)" -lt "$deadline" ] || { echo "✗ verify-deploy timed out"; exit 1; }
      sleep 5
    done

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
