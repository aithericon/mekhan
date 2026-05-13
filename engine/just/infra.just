# Infrastructure lifecycle recipes
# Usage: just infra nats-up, just infra nomad-up, just infra slurm-ssh, etc.

# Run recipes from the project root, not the just/ directory
set working-directory := '..'

# ============================================
# NATS
# ============================================

# Start NATS server (idempotent)
nats-up:
    @echo "Starting NATS server..."
    @docker compose up -d nats
    @just infra nats-wait

# Stop NATS server
nats-down:
    @echo "Stopping NATS server..."
    @docker compose down

# Wipe NATS — destroy container and all JetStream data, then restart fresh
nats-wipe:
    @echo "Wiping NATS (destroying container + all data)..."
    @docker compose down 2>/dev/null || true
    @docker rm -f petri-nats 2>/dev/null || true
    @echo "Starting fresh NATS..."
    @docker compose up -d nats
    @just infra nats-wait
    @echo "NATS wiped and restarted (all streams, KV buckets, and consumers gone)"

# Wait for NATS to be healthy
nats-wait:
    @echo "Waiting for NATS to be healthy..."
    @for i in $(seq 1 30); do \
        if docker compose exec -T nats wget -q --spider http://localhost:8222/healthz 2>/dev/null; then \
            echo "NATS is healthy!"; \
            exit 0; \
        fi; \
        echo "Waiting... ($i/30)"; \
        sleep 1; \
    done; \
    echo "ERROR: NATS did not become healthy in time"; \
    exit 1

# Check NATS status
nats-status:
    @docker compose ps nats
    @echo ""
    @echo "JetStream info:"
    @curl -s http://localhost:8333/jsz 2>/dev/null | head -20 || echo "NATS not running"

# Subscribe to all Petri events (for debugging)
nats-subscribe:
    @docker compose exec nats-box nats sub -s nats://nats:4222 'petri.events.>'

# Publish a test token injection (for debugging)
nats-test-inject place_id='p_tasks' data='{"id":"test-1"}':
    @docker compose exec nats-box nats pub -s nats://nats:4222 \
        petri.commands.inject.token \
        '{"place_id":"{{place_id}}","color":{{data}}}'

# View NATS streams
nats-streams:
    @docker compose exec nats-box nats -s nats://nats:4222 stream ls

# Remove orphaned testcontainer NATS instances (keeps docker-compose petri-nats)
nats-cleanup:
    #!/usr/bin/env bash
    echo "Removing orphaned NATS testcontainers..."
    ids=$(docker ps -aq --filter "ancestor=nats:2.10.14")
    if [ -n "$ids" ]; then
        count=$(echo "$ids" | wc -l | tr -d ' ')
        docker rm -f $ids >/dev/null 2>&1
        echo "Removed $count container(s)."
    else
        echo "No orphaned containers found."
    fi

# Clean up test streams (removes streams matching test_* pattern)
nats-cleanup-test-streams:
    @echo "Cleaning up test streams..."
    @docker compose --profile tools up -d nats-box 2>/dev/null || true
    @sleep 1
    @docker compose exec -T nats-box sh -c 'for stream in $(nats -s nats://nats:4222 stream ls -n 2>/dev/null | grep test_); do nats -s nats://nats:4222 stream rm -f "$stream" 2>/dev/null; done' || echo "No test streams to clean"
    @echo "Done"

# Start NATS Box for interactive CLI
nats-shell:
    @docker compose --profile tools up -d nats-box
    @docker compose exec nats-box sh

# ============================================
# Tempo (Distributed Tracing)
# ============================================

# Start Grafana Tempo (OTLP receiver for trace spans)
tempo-up:
    @echo "Starting Tempo..."
    @docker compose --profile tracing up -d tempo
    @just infra tempo-wait

# Stop Tempo
tempo-down:
    @echo "Stopping Tempo..."
    @docker rm -f petri-tempo 2>/dev/null || true

# Wait for Tempo to be healthy
tempo-wait:
    @echo "Waiting for Tempo to be healthy..."
    @for i in $(seq 1 30); do \
        if curl -sf http://localhost:3200/ready > /dev/null 2>&1; then \
            echo "Tempo is healthy!"; \
            exit 0; \
        fi; \
        echo "Waiting... ($i/30)"; \
        sleep 1; \
    done; \
    echo "ERROR: Tempo did not become healthy in time"; \
    exit 1

# Check Tempo status
tempo-status:
    @docker ps --filter name=petri-tempo --format "table {{{{.Names}}\t{{{{.Status}}\t{{{{.Ports}}"

# Query a trace by ID from Tempo
tempo-trace trace_id:
    @curl -s "http://localhost:3200/api/traces/{{trace_id}}" -H "Accept: application/json" | python3 -m json.tool

# ============================================
# RustFS (S3-compatible object storage)
# ============================================

# Start RustFS and create the research bucket with public-read policy
rustfs-up:
    #!/usr/bin/env bash
    echo "Starting RustFS..."
    docker compose --profile storage up -d rustfs
    echo "Waiting for RustFS S3 API..."
    for i in $(seq 1 30); do
        if curl -so /dev/null --max-time 2 http://localhost:9005/ 2>/dev/null; then
            echo "RustFS is ready!"
            break
        fi
        if [ "$i" -eq 30 ]; then echo "ERROR: RustFS did not start"; exit 1; fi
        sleep 1
    done
    echo "Creating 'research' bucket..."
    AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
        aws --endpoint-url http://localhost:9005 --region us-east-1 \
        s3 mb s3://research 2>/dev/null || true
    echo "Setting public-read policy on 'research' bucket..."
    AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
        aws --endpoint-url http://localhost:9005 --region us-east-1 \
        s3api put-bucket-policy --bucket research \
        --policy '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["*"]},"Action":["s3:GetObject"],"Resource":["arn:aws:s3:::research/*"]}]}'
    echo "RustFS bucket 'research' ready (public-read)"

# Stop RustFS
rustfs-down:
    @echo "Stopping RustFS..."
    @docker rm -f petri-rustfs 2>/dev/null || true

# Check RustFS status
rustfs-status:
    @docker ps --filter name=petri-rustfs --format "table {{{{.Names}}\t{{{{.Status}}\t{{{{.Ports}}"
    @echo ""
    @echo "Console: http://localhost:9006 (rustfsadmin / rustfsadmin)"
    @echo "S3 API:  http://localhost:9005"

# ============================================
# Nomad
# ============================================

# Start Nomad dev agent (requires nomad binary on PATH)
nomad-up:
    #!/usr/bin/env bash
    if curl -sf --max-time 2 http://127.0.0.1:4646/v1/status/leader > /dev/null 2>&1; then
        echo "Nomad dev agent already running"
        exit 0
    fi
    if ! command -v nomad &> /dev/null; then
        echo "ERROR: nomad binary not found on PATH"
        echo "Install from: https://developer.hashicorp.com/nomad/install"
        echo "  macOS: brew install hashicorp/tap/nomad"
        exit 1
    fi
    echo "Starting nomad agent -dev..."
    nomad agent -dev -log-level WARN -config infra/nomad/dev.hcl &
    disown
    just infra nomad-wait

# Wait for Nomad dev agent to be ready
nomad-wait:
    #!/usr/bin/env bash
    echo "Waiting for Nomad to be ready..."
    for i in $(seq 1 30); do
        if curl -sf --max-time 2 http://127.0.0.1:4646/v1/status/leader > /dev/null 2>&1; then
            echo "Nomad is ready"
            exit 0
        fi
        echo "  Waiting... ($i/30)"
        sleep 1
    done
    echo "ERROR: Nomad did not become ready in time"
    exit 1

# Stop Nomad dev agent
nomad-down:
    #!/usr/bin/env bash
    if pgrep -f "nomad agent -dev" > /dev/null 2>&1; then
        echo "Stopping Nomad dev agent..."
        pkill -f "nomad agent -dev" || true
    else
        echo "No Nomad dev agent running"
    fi

# Show Nomad cluster status
nomad-status:
    nomad status

# Register the batch worker job template
nomad-register-template:
    curl -sf -X POST http://127.0.0.1:4646/v1/jobs -d @infra/nomad/job-template.json > /dev/null
    @echo "Registered petri-batch-worker job template"

# ============================================
# Slurm
# ============================================

# Start local Slurm cluster
slurm-up:
    @chmod 600 infra/slurm/ssh/slurm_test
    docker compose -f infra/slurm/docker-compose.yml up -d --build

# Stop local Slurm cluster
slurm-down:
    docker compose -f infra/slurm/docker-compose.yml down

# Wait for Slurm container to be ready (SSH + slurmctld)
slurm-wait:
    #!/usr/bin/env bash
    echo "Waiting for Slurm to be ready..."
    for i in $(seq 1 30); do
        if ssh -o StrictHostKeyChecking=no -o ConnectTimeout=2 -o IdentitiesOnly=yes \
            -i infra/slurm/ssh/slurm_test -p 2222 testuser@localhost \
            "sinfo -h" >/dev/null 2>&1; then
            echo "Slurm is ready"
            exit 0
        fi
        echo "  Waiting... ($i/30)"
        sleep 2
    done
    echo "ERROR: Slurm did not become ready in time"
    exit 1

# SSH into Slurm login node
slurm-ssh:
    ssh -o StrictHostKeyChecking=no -o IdentitiesOnly=yes -i infra/slurm/ssh/slurm_test -p 2222 testuser@localhost

# Open shell in Slurm container
slurm-shell:
    docker compose -f infra/slurm/docker-compose.yml exec slurm bash

# Show Slurm cluster status
slurm-status:
    docker compose -f infra/slurm/docker-compose.yml exec slurm sinfo
