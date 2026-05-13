# =============================================================================
# Local dev recipes
# =============================================================================

set shell := ["bash", "-euo", "pipefail", "-c"]

# Start local infra (Postgres + NATS) via docker compose
up:
    #!/usr/bin/env bash
    cd ..
    docker compose up -d
    echo "✓ Postgres on :5439, NATS on :4333 / :8333"

# Stop local infra
down:
    #!/usr/bin/env bash
    cd ..
    docker compose down

# Tail infra logs
logs:
    #!/usr/bin/env bash
    cd ..
    docker compose logs -f

# Run the service against local infra (cargo run, debug build)
service:
    #!/usr/bin/env bash
    cd ../service
    cargo run --bin mekhan-service

# Run the SvelteKit dev server
app:
    #!/usr/bin/env bash
    cd ../app
    npm install
    npm run dev

# Run service with auto-reload on source change (requires cargo-watch)
watch:
    #!/usr/bin/env bash
    cd ../service
    cargo watch -x 'run --bin mekhan-service'
