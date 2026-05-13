# =============================================================================
# Database recipes (local dev)
# =============================================================================

set shell := ["bash", "-euo", "pipefail", "-c"]

# Connection string for local dev — overridable via env
db_url := env_var_or_default("MEKHAN__DATABASE_URL", "postgres://mekhan:mekhan@localhost:5439/mekhan")

# Drop into a psql shell against the local DB
psql:
    psql "{{db_url}}"

# Run sqlx-cli migrations against the local DB
# (Service also runs migrations at startup via sqlx::migrate!, so this is
# only needed if you want to apply them ahead of time or for tooling.)
migrate:
    #!/usr/bin/env bash
    cd ../service
    sqlx migrate run --database-url "{{db_url}}"

# List migration status
migrate-info:
    #!/usr/bin/env bash
    cd ../service
    sqlx migrate info --database-url "{{db_url}}"

# Nuke the local DB and re-apply all migrations
reset:
    #!/usr/bin/env bash
    cd ..
    docker compose stop postgres
    docker compose rm -f postgres
    docker volume rm aithericon-platform_mekhan-pgdata 2>/dev/null || true
    docker compose up -d postgres
    echo "▶ waiting for postgres..."
    until docker compose exec -T postgres pg_isready -U mekhan >/dev/null 2>&1; do sleep 1; done
    cd service && sqlx migrate run --database-url "{{db_url}}"
    echo "✓ database reset"
