#!/usr/bin/env bash
# Shared helper functions for Petri-Lab just recipes.
# Source this at the top of any #!/usr/bin/env bash recipe:
#   source just/lib.sh

set -euo pipefail

# --- Process management ---

# Kill any process listening on a given port.
# Usage: stop_on_port PORT LABEL
stop_on_port() {
    local port="$1" label="${2:-process}"
    if lsof -ti :"$port" > /dev/null 2>&1; then
        echo "Stopping $label on port $port..."
        kill $(lsof -ti :"$port") 2>/dev/null || true
    else
        echo "No $label running on port $port"
    fi
}

# Kill stale engine on port 3030 (silent, for use at demo start).
kill_stale_engine() {
    if lsof -ti :3030 > /dev/null 2>&1; then
        echo "Stopping stale engine on port 3030..."
        kill $(lsof -ti :3030) 2>/dev/null || true
        sleep 1
    fi
    kill_stale_trace_exporter
}

# Kill any running trace exporter sidecar.
kill_stale_trace_exporter() {
    if pgrep -f "petri-trace-exporter" > /dev/null 2>&1; then
        pkill -f "petri-trace-exporter" 2>/dev/null || true
    fi
}

# Start Tempo + trace exporter sidecar in the background.
# Sets TRACE_EXPORTER_PID in the caller's scope.
# Usage: start_trace_exporter [OTLP_ENDPOINT]
start_trace_exporter() {
    local otlp_endpoint="${1:-http://localhost:4318}"

    # Ensure Tempo is running (idempotent — skips if already up)
    if ! curl -sf http://localhost:3200/ready > /dev/null 2>&1; then
        echo "   Starting Tempo..."
        just infra tempo-up
    fi

    echo "   Starting trace exporter (OTLP → $otlp_endpoint)..."
    NATS_URL="${NATS_URL:-nats://localhost:4333}" \
    OTLP_ENDPOINT="$otlp_endpoint" \
    RUST_LOG="${RUST_LOG:-info}" \
    cargo run -p petri-trace-exporter > /dev/null 2>&1 &
    TRACE_EXPORTER_PID=$!
}

# Kill stale executor processes.
kill_stale_executor() {
    if pgrep -f "aithericon-executor-service" > /dev/null 2>&1; then
        pkill -f "aithericon-executor-service" 2>/dev/null || true
        sleep 1
    fi
    pkill -f "mock_executor" 2>/dev/null || true
}

# --- Prerequisite checks ---

# Verify the executor repo exists at the given path.
# Usage: check_executor_repo "$EXECUTOR_REPO"
check_executor_repo() {
    local repo="$1"
    if [ ! -f "$repo/Cargo.toml" ]; then
        echo "ERROR: aithericon-executor repo not found at $repo"
        echo "Expected sibling checkout: git clone <executor-repo> $repo"
        exit 1
    fi
}

# Verify cargo-zigbuild is installed (for Slurm cross-compilation).
check_cargo_zigbuild() {
    if ! command -v cargo-zigbuild &> /dev/null; then
        echo "ERROR: cargo-zigbuild not found on PATH"
        echo "Install with: cargo install cargo-zigbuild"
        echo "Also requires: brew install zig"
        exit 1
    fi
}

# --- Human UI lifecycle ---

# Start human-ui dev server with admin credentials.
# Sets HUMAN_UI_PID in the caller's scope.
# Usage: start_human_ui REPO_PATH [LOG_FILE] [EXTRA_ENV...]
#   REPO_PATH  — path to aithericon-human-ui checkout
#   LOG_FILE   — where to redirect output (default: /dev/null)
#   EXTRA_ENV  — additional KEY=VAL pairs (e.g. S3_ACCESS_KEY=x S3_SECRET_KEY=y)
start_human_ui() {
    local repo="$1" log="${2:-/dev/null}"; shift 2 || shift $#

    # Export admin credentials + any extra env vars (e.g. S3_ACCESS_KEY=x)
    export ADMIN_EMAIL=admin@test.com
    export ADMIN_PASSWORD=test1234
    export ADMIN_NAME="Test Admin"
    export BETTER_AUTH_SECRET=dev-secret-do-not-use-in-production
    if [ $# -gt 0 ]; then
        local kv
        for kv in "$@"; do export "$kv"; done
    fi

    echo "   Starting Human UI..."
    cd "$repo"
    deno task dev > "$log" 2>&1 &
    HUMAN_UI_PID=$!
    cd - > /dev/null

    for i in $(seq 1 30); do
        if curl -sf http://localhost:5173 > /dev/null 2>&1; then
            echo "   Human UI ready (PID $HUMAN_UI_PID)"
            return 0
        fi
        if [ "$i" -eq 30 ]; then
            echo "WARNING: Human UI did not start in time (PID $HUMAN_UI_PID)"
            return 1
        fi
        sleep 1
    done
}

# --- Engine lifecycle ---

# Wait for the engine API to be ready.
# Usage: wait_for_engine_api [TIMEOUT_SECS] [ENDPOINT]
wait_for_engine_api() {
    local timeout="${1:-30}" endpoint="${2:-http://localhost:3030/api/nets/metadata}"
    for i in $(seq 1 "$timeout"); do
        if curl -sf "$endpoint" > /dev/null 2>&1; then
            echo "   Engine API is ready"
            return 0
        fi
        if [ "$i" -eq "$timeout" ]; then
            echo "ERROR: Engine did not start in time"
            exit 1
        fi
        sleep 1
    done
}

# Wait for watchers to connect (simple sleep).
# Usage: wait_for_watchers [LABEL] [SECS]
wait_for_watchers() {
    local label="${1:-watcher}" secs="${2:-3}"
    echo "   Waiting for $label to connect..."
    sleep "$secs"
}

# --- NATS stream cleanup ---

# Clean all known stale NATS streams from previous runs.
clean_nats_streams() {
    echo "   Cleaning stale NATS streams..."
    for STREAM in \
        EXECUTOR_JOBS_STREAM \
        executor_jobs_high executor_jobs_medium executor_jobs_low executor_jobs_dlq \
        EXECUTOR_STATUS EXECUTOR_EVENTS \
        PETRI_GLOBAL KV_PETRI_WATCHER; do
        nats -s nats://localhost:4333 stream delete "$STREAM" -f 2>/dev/null || true
    done
}

# Purge stale Nomad child jobs from previous runs.
purge_nomad_jobs() {
    echo "   Purging stale Nomad jobs..."
    for JOB_ID in $(curl -sf http://127.0.0.1:4646/v1/jobs 2>/dev/null \
        | python3 -c "import json,sys; [print(j['ID']) for j in json.load(sys.stdin) if j.get('ParentID','')]" 2>/dev/null); do
        curl -sf -X DELETE "http://127.0.0.1:4646/v1/job/$JOB_ID?purge=true" > /dev/null 2>&1 || true
    done
}

# --- Slurm helpers ---

# Copy executor binary into the Slurm container.
# Usage: install_executor_in_slurm BINARY_PATH
install_executor_in_slurm() {
    local bin_path="$1"
    echo "Installing executor binary into Slurm container..."
    local container
    container=$(docker compose -f infra/slurm/docker-compose.yml ps -q slurm)
    docker exec "$container" mkdir -p /opt/petri/bin
    docker cp "$bin_path" "$container:/opt/petri/bin/executor"
    docker exec "$container" chmod +x /opt/petri/bin/executor
}

# Copy executor binary + Python SDK into the Slurm container.
# Usage: install_executor_and_sdk_in_slurm BINARY_PATH SDK_PATH
install_executor_and_sdk_in_slurm() {
    local bin_path="$1" sdk_path="$2"
    install_executor_in_slurm "$bin_path"
    local container
    container=$(docker compose -f infra/slurm/docker-compose.yml ps -q slurm)
    docker cp "$sdk_path" "$container:/opt/petri/aithericon-sdk"
    docker exec "$container" chmod -R a+rwX /opt/petri/aithericon-sdk
}

# --- Scenario deployment ---

# Deploy a scenario and capture the output.
# Usage: DEPLOY_OUTPUT=$(deploy_scenario BINARY [EXTRA_FLAGS...])
deploy_scenario() {
    local binary="$1"; shift
    local output
    output=$("./target/debug/examples/$binary" "$@" 2>&1)
    echo "$output" | head -2 >&2
    echo "$output"
}


# --- Running mode + evaluation ---

# Set nets to running mode via the API.
# Usage: set_nets_running net1 net2 ...
#   For single-net (flat API): set_nets_running
set_nets_running() {
    if [ $# -eq 0 ]; then
        ./target/debug/aithericon activate --all
    else
        for net in "$@"; do
            ./target/debug/aithericon activate "$net"
        done
    fi
}

# Trigger evaluation and print steps fired.
# Usage: trigger_eval [NET_ID]
#   No args = flat API, with arg = scoped API.
trigger_eval() {
    local url steps
    if [ $# -eq 0 ]; then
        url="http://localhost:3030/api/command/evaluate"
    else
        url="http://localhost:3030/api/nets/$1/command/evaluate"
    fi
    local result
    result=$(curl -s -X POST -H "Content-Type: application/json" -d '{}' "$url")
    steps=$(echo "$result" | python3 -c "import json,sys; print(json.load(sys.stdin).get('steps_executed','?'))" 2>/dev/null || echo "?")
    echo "   Fired $steps steps"
}

# --- Polling ---

# Poll a single place for token completion.
# Usage: poll_single_place PLACE_ID TARGET_COUNT MAX_ITERS SLEEP_SECS [NET_ID]
poll_single_place() {
    local place_id="$1" target="$2" max_iters="$3" sleep_secs="$4" net_id="${5:-}"
    local state_url
    if [ -z "$net_id" ]; then
        state_url="http://localhost:3030/api/state"
    else
        state_url="http://localhost:3030/api/nets/$net_id/state"
    fi

    for i in $(seq 1 "$max_iters"); do
        sleep "$sleep_secs"
        local state completed
        state=$(curl -s "$state_url")
        completed=$(echo "$state" | python3 -c "
import json, sys
s = json.load(sys.stdin)
tokens = s.get('marking',{}).get('tokens',{}).get('$place_id',[])
print(len(tokens))
" 2>/dev/null || echo "0")

        echo "   [$i] completed: $completed / $target"

        if [ "$completed" -ge "$target" ]; then
            echo ""
            echo "=== All $target job(s) completed! ==="
            return 0
        fi

        if [ "$i" -eq "$max_iters" ]; then
            echo ""
            echo "WARNING: Timed out ($completed/$target completed)"
        fi
    done
}

# Poll job-net for completed + dead_letter tokens (three-layer+ demos).
# Usage: poll_job_net_terminal TARGET MAX_ITERS SLEEP_SECS [NET_ID]
poll_job_net_terminal() {
    local target="$1" max_iters="$2" sleep_secs="$3" net_id="${4:-job-net}"
    local state_url="http://localhost:3030/api/nets/$net_id/state"

    for i in $(seq 1 "$max_iters"); do
        sleep "$sleep_secs"
        local state terminal completed dead
        state=$(curl -s "$state_url")
        terminal=$(echo "$state" | python3 -c "
import json, sys
s = json.load(sys.stdin)
tokens = s.get('marking',{}).get('tokens',{})
completed = 0
dead = 0
for pid, toks in tokens.items():
    for t in toks:
        data = t.get('color', {}).get('value', {})
        if 'detail' in data and 'model_name' in data and 'job_id' in data:
            completed += 1
        elif 'reason' in data and 'retries_exhausted' in data:
            dead += 1
print(f'{completed},{dead}')
" 2>/dev/null || echo "0,0")
        completed=$(echo "$terminal" | cut -d, -f1)
        dead=$(echo "$terminal" | cut -d, -f2)

        echo "   [$i] $net_id: completed=$completed  dead_letter=$dead  (target: $target terminal)"

        local total=$((completed + dead))
        if [ "$total" -ge "$target" ]; then
            echo ""
            echo "=== All jobs reached terminal state! ==="
            return 0
        fi

        if [ "$i" -eq "$max_iters" ]; then
            echo ""
            echo "WARNING: Timed out ($total/$target terminal)"
        fi
    done
}

# Poll workflow-net for completed + failed tokens (four-layer+ demos).
# Usage: poll_workflow_terminal MAX_ITERS SLEEP_SECS [WF_NET] [JOB_NET] [JOB_TARGET]
poll_workflow_terminal() {
    local max_iters="$1" sleep_secs="$2"
    local wf_net="${3:-workflow-net}" job_net="${4:-job-net}" job_target="${5:-4}"

    for i in $(seq 1 "$max_iters"); do
        sleep "$sleep_secs"

        local wf_state terminal completed failed
        wf_state=$(curl -s "http://localhost:3030/api/nets/$wf_net/state")
        terminal=$(echo "$wf_state" | python3 -c "
import json, sys
s = json.load(sys.stdin)
tokens = s.get('marking',{}).get('tokens',{})
completed = 0
failed = 0
for pid, toks in tokens.items():
    for t in toks:
        data = t.get('color', {}).get('value', {})
        if 'final_detail' in data and 'pipeline_name' in data:
            completed += 1
        elif 'failed_step' in data and 'reason' in data:
            failed += 1
print(f'{completed},{failed}')
" 2>/dev/null || echo "0,0")
        completed=$(echo "$terminal" | cut -d, -f1)
        failed=$(echo "$terminal" | cut -d, -f2)

        local job_state job_completed
        job_state=$(curl -s "http://localhost:3030/api/nets/$job_net/state" 2>/dev/null || echo "{}")
        job_completed=$(echo "$job_state" | python3 -c "
import json, sys
s = json.load(sys.stdin)
tokens = s.get('marking',{}).get('tokens',{})
count = 0
for pid, toks in tokens.items():
    for t in toks:
        data = t.get('color', {}).get('value', {})
        if 'detail' in data and 'model_name' in data and 'job_id' in data:
            count += 1
print(count)
" 2>/dev/null || echo "?")

        echo "   [$i] workflow: completed=$completed failed=$failed | $job_net completed=$job_completed/$job_target"

        if [ "$completed" -ge 1 ] || [ "$failed" -ge 1 ]; then
            echo ""
            if [ "$completed" -ge 1 ]; then
                echo "=== Workflow completed successfully! ==="
            else
                echo "=== Workflow failed ==="
            fi
            return 0
        fi

        if [ "$i" -eq "$max_iters" ]; then
            echo ""
            echo "WARNING: Timed out waiting for workflow completion"
        fi
    done
}

# Poll campaign-net for completed + failed tokens (five-layer demos).
# Usage: poll_campaign_terminal MAX_ITERS SLEEP_SECS
poll_campaign_terminal() {
    local max_iters="$1" sleep_secs="$2"

    for i in $(seq 1 "$max_iters"); do
        sleep "$sleep_secs"

        local camp_state terminal completed failed
        camp_state=$(curl -s http://localhost:3030/api/nets/campaign-net/state)
        terminal=$(echo "$camp_state" | python3 -c "
import json, sys
s = json.load(sys.stdin)
tokens = s.get('marking',{}).get('tokens',{})
completed = 0
failed = 0
for pid, toks in tokens.items():
    for t in toks:
        data = t.get('color', {}).get('value', {})
        if 'config_a_detail' in data and 'config_b_detail' in data:
            completed += 1
        elif 'failed_workflow_id' in data and 'reason' in data:
            failed += 1
print(f'{completed},{failed}')
" 2>/dev/null || echo "0,0")
        completed=$(echo "$terminal" | cut -d, -f1)
        failed=$(echo "$terminal" | cut -d, -f2)

        local wf_state wf_completed job_state job_completed
        wf_state=$(curl -s http://localhost:3030/api/nets/workflow-net/state 2>/dev/null || echo "{}")
        wf_completed=$(echo "$wf_state" | python3 -c "
import json, sys
s = json.load(sys.stdin)
tokens = s.get('marking',{}).get('tokens',{})
count = 0
for pid, toks in tokens.items():
    for t in toks:
        data = t.get('color', {}).get('value', {})
        if 'final_detail' in data and 'pipeline_name' in data:
            count += 1
print(count)
" 2>/dev/null || echo "?")

        job_state=$(curl -s http://localhost:3030/api/nets/job-net/state 2>/dev/null || echo "{}")
        job_completed=$(echo "$job_state" | python3 -c "
import json, sys
s = json.load(sys.stdin)
tokens = s.get('marking',{}).get('tokens',{})
count = 0
for pid, toks in tokens.items():
    for t in toks:
        data = t.get('color', {}).get('value', {})
        if 'detail' in data and 'model_name' in data and 'job_id' in data:
            count += 1
print(count)
" 2>/dev/null || echo "?")

        echo "   [$i] campaign: completed=$completed failed=$failed | workflows=$wf_completed/2 | jobs=$job_completed/8"

        if [ "$completed" -ge 1 ] || [ "$failed" -ge 1 ]; then
            echo ""
            if [ "$completed" -ge 1 ]; then
                echo "=== Campaign completed successfully! ==="
            else
                echo "=== Campaign failed ==="
            fi
            return 0
        fi

        if [ "$i" -eq "$max_iters" ]; then
            echo ""
            echo "WARNING: Timed out waiting for campaign completion"
        fi
    done
}

# --- Reporting ---

# Print token distribution per place for one or more nets.
# Usage: print_net_states net1 net2 ...
print_net_states() {
    for net in "$@"; do
        local state topo
        state=$(curl -s "http://localhost:3030/api/nets/$net/state")
        topo=$(curl -s "http://localhost:3030/api/nets/$net/topology")
        echo "   --- $net ---"

        python3 -c "
import json, sys
state = json.loads(sys.argv[1])
topo = json.loads(sys.argv[2])
names = {}
for p in topo.get('topology', {}).get('places', []):
    names[p['id']] = p.get('name', p['id'][:8])
for pid, toks in state.get('marking', {}).get('tokens', {}).items():
    if len(toks) > 0:
        name = names.get(pid, pid[:8])
        print(f'      {name}: {len(toks)} token(s)')
" "$state" "$topo" 2>/dev/null || true
    done
}

# Print token distribution for a single net using the flat API.
# Usage: print_single_net_state
print_single_net_state() {
    local state
    state=$(curl -s http://localhost:3030/api/state)
    python3 -c "
import json, sys
s = json.load(sys.stdin)
for pid, toks in s.get('marking',{}).get('tokens',{}).items():
    if len(toks) > 0:
        print(f'      {pid[:12]}: {len(toks)} token(s)')
" <<< "$state" 2>/dev/null || true
}

# Print event chain summary for one or more nets.
# Usage: print_event_summary net1 net2 ...
print_event_summary() {
    for net in "$@"; do
        local events event_count chain_valid
        events=$(curl -s "http://localhost:3030/api/nets/$net/events")
        event_count=$(echo "$events" | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('events',[])))" 2>/dev/null || echo "?")
        chain_valid=$(echo "$events" | python3 -c "import json,sys; print(json.load(sys.stdin).get('chain_valid','?'))" 2>/dev/null || echo "?")
        echo "   $net: $event_count events (chain valid: $chain_valid)"
    done
}

# Print event chain summary for a single net using the flat API.
# Usage: print_single_event_summary
print_single_event_summary() {
    local events event_count chain_valid
    events=$(curl -s http://localhost:3030/api/events)
    event_count=$(echo "$events" | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('events',[])))" 2>/dev/null || echo "?")
    chain_valid=$(echo "$events" | python3 -c "import json,sys; print(json.load(sys.stdin).get('chain_valid','?'))" 2>/dev/null || echo "?")
    echo "   Events: $event_count (chain valid: $chain_valid)"
}

# Print campaign result details.
print_campaign_results() {
    local camp_state
    camp_state=$(curl -s http://localhost:3030/api/nets/campaign-net/state)
    python3 -c "
import json, sys
s = json.load(sys.stdin)
tokens = s.get('marking',{}).get('tokens',{})
for pid, toks in tokens.items():
    for t in toks:
        data = t.get('color', {}).get('value', {})
        if 'config_a_detail' in data:
            print(f'      Config A ({data.get(\"config_a_workflow_id\",\"?\")}):', json.dumps(data.get('config_a_detail',{})))
            print(f'      Config B ({data.get(\"config_b_workflow_id\",\"?\")}):', json.dumps(data.get('config_b_detail',{})))
" <<< "$camp_state" 2>/dev/null || true
}

# Print IPC event capture summary for executor-net.
print_ipc_summary() {
    echo "   === IPC Event Capture Summary (executor-net) ==="
    local state topo
    state=$(curl -s "http://localhost:3030/api/nets/executor-net/state")
    topo=$(curl -s "http://localhost:3030/api/nets/executor-net/topology")
    python3 -c "
import json, sys
state = json.loads(sys.argv[1])
topo = json.loads(sys.argv[2])
names = {}
for p in topo.get('topology', {}).get('places', []):
    names[p['id']] = p.get('name', p['id'][:8])
event_places = ['Progress Log', 'Artifact Log', 'Metric Log', 'Phase Log', 'Output Log', 'Message Log']
for pid, toks in state.get('marking', {}).get('tokens', {}).items():
    name = names.get(pid, pid[:8])
    if name in event_places and len(toks) > 0:
        print(f'      {name}: {len(toks)} event(s) captured')
" "$state" "$topo" 2>/dev/null || true
}

# --- Demo footer ---

# Print the standard demo footer.
# Usage: print_demo_footer DEMO_NAME ENGINE_PID STOP_CMD [EXTRA_LINES...]
print_demo_footer() {
    local name="$1" pid="$2" stop_cmd="$3"; shift 3
    echo ""
    echo "=== $name complete ==="
    echo ""
    echo "Engine is running on http://localhost:3030 (PID $pid)"
    for line in "$@"; do
        echo "  $line"
    done
    echo "  Swagger UI: http://localhost:3030/swagger-ui"
    echo "  Stop with:  just demo $stop_cmd"
}

# Print the multi-net demo footer with additional links.
# Usage: print_multi_net_footer DEMO_NAME ENGINE_PID STOP_CMD [EXTRA_LINES...]
print_multi_net_footer() {
    local name="$1" pid="$2" stop_cmd="$3"; shift 3
    echo ""
    echo "=== $name complete ==="
    echo ""
    echo "Engine is running on http://localhost:3030 (PID $pid)"
    for line in "$@"; do
        echo "  $line"
    done
    echo "  Swagger UI:  http://localhost:3030/swagger-ui"
    echo "  Lab UI:      http://localhost:3030"
    echo "  List nets:   curl http://localhost:3030/api/nets"
    echo "  Net state:   curl http://localhost:3030/api/nets/{net_id}/state"
    echo "  Stop with:   just demo $stop_cmd"
}
