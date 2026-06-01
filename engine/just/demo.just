# Demo recipes
# Usage: just demo nomad, just demo three-layer, just demo campaign, etc.

# Run recipes from the project root, not the just/ directory
set working-directory := '..'

# Absolute path to lib.sh (for source in bash recipes)
_lib := source_directory() / "lib.sh"

# Path to sibling aithericon-executor repo (relative to petri-lab root)
executor_repo := "../aithericon-executor"

# ============================================
# Nomad Demo
# ============================================

# Stop the engine and Nomad infrastructure
nomad-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    just infra nomad-down

# Run the Nomad batch net demo end-to-end
nomad:
    #!/usr/bin/env bash
    source {{_lib}}

    echo "=== Petri-Lab Nomad Demo ==="
    echo ""

    kill_stale_engine

    echo "1. Starting NATS + Nomad..."
    just infra nats-up
    just infra nomad-up

    echo ""
    echo "2. Registering batch worker job template..."
    just infra nomad-register-template

    echo ""
    echo "3. Building engine + scenario..."
    cargo build -p core-engine --features nomad
    cargo build -p aithericon-sdk --example nomad_batch_net

    echo ""
    echo "4. Starting engine with Nomad backend..."
    set -a; source infra/nomad/nomad.env; set +a

    RUST_LOG=info,petri_nomad=debug \
    cargo run -p core-engine --features nomad &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Nomad watcher"

    echo ""
    echo "5. Deploying nomad_batch_net scenario..."
    deploy_scenario nomad_batch_net -- --deploy
    NET_ID="nomad-batch"
    COMPLETED_ID="completed"

    echo ""
    echo "6. Setting engine to running mode and submitting jobs..."
    set_nets_running "$NET_ID"
    trigger_eval "$NET_ID"

    echo ""
    echo "7. Waiting for Nomad jobs to complete..."
    poll_single_place "$COMPLETED_ID" 3 30 2 "$NET_ID"

    echo ""
    echo "8. Final state:"
    curl -s "http://localhost:3030/api/nets/$NET_ID/state" | python3 -c "
    import json, sys
    s = json.load(sys.stdin)
    tokens = s.get('marking',{}).get('tokens',{}).get('$COMPLETED_ID',[])
    for t in tokens:
        v = t.get('color',{}).get('value',{})
        print(f'   {v.get(\"job_id\",\"?\")} / {v.get(\"task_name\",\"?\")}  (nomad_id={v.get(\"scheduler_job_id\",\"?\")})')
    "
    print_event_summary "$NET_ID"
    print_demo_footer "Nomad Demo" "$ENGINE_PID" "nomad-stop"

# ============================================
# Slurm Demo
# ============================================

# Stop the engine and Slurm infrastructure
slurm-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    just infra slurm-down

# Run the Slurm batch net demo end-to-end
slurm:
    #!/usr/bin/env bash
    source {{_lib}}

    echo "=== Petri-Lab Slurm Demo ==="
    echo ""

    kill_stale_engine
    chmod 600 infra/slurm/ssh/slurm_test

    echo "1. Starting NATS + Slurm..."
    just infra nats-up
    just infra slurm-up
    just infra slurm-wait

    echo ""
    echo "2. Building engine + scenario..."
    cargo build -p core-engine --features slurm
    cargo build -p aithericon-sdk --example slurm_batch_net

    echo ""
    echo "3. Starting engine with Slurm backend..."
    set -a; source infra/slurm/slurm.env; set +a

    RUST_LOG=info,petri_slurm=debug \
    cargo run -p core-engine --features slurm &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter

    echo ""
    echo "4. Deploying slurm_batch_net scenario..."
    deploy_scenario slurm_batch_net -- --deploy
    NET_ID="slurm-batch"
    COMPLETED_ID="completed"

    echo ""
    echo "5. Setting engine to running mode and submitting jobs..."
    set_nets_running "$NET_ID"
    trigger_eval "$NET_ID"

    echo ""
    echo "6. Waiting for Slurm jobs to complete..."
    poll_single_place "$COMPLETED_ID" 3 30 3 "$NET_ID"

    echo ""
    echo "7. Final state:"
    curl -s "http://localhost:3030/api/nets/$NET_ID/state" | python3 -c "
    import json, sys
    s = json.load(sys.stdin)
    tokens = s.get('marking',{}).get('tokens',{}).get('$COMPLETED_ID',[])
    for t in tokens:
        v = t.get('color',{}).get('value',{})
        print(f'   {v.get(\"job_id\",\"?\")} / {v.get(\"task_name\",\"?\")}  (slurm_id={v.get(\"scheduler_job_id\",\"?\")})')
    "
    print_event_summary "$NET_ID"
    print_demo_footer "Slurm Demo" "$ENGINE_PID" "slurm-stop"

# ============================================
# Executor Demo
# ============================================

# Stop the engine and executor
executor-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    kill_stale_executor

# Run the Executor lifecycle demo end-to-end
executor:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"

    echo "=== Petri-Lab Executor Demo ==="
    echo ""

    check_executor_repo "$EXECUTOR_REPO"
    kill_stale_engine
    kill_stale_executor

    echo "1. Starting NATS..."
    just infra nats-up
    clean_nats_streams

    echo ""
    echo "2. Building engine + scenario + executor..."
    cargo build -p core-engine --features executor
    cargo build -p aithericon-sdk --example executor_net
    cargo build --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service

    echo ""
    echo "3. Starting aithericon-executor..."
    set -a; source infra/executor/aithericon-executor.env; set +a
    RUST_LOG=info "$EXECUTOR_REPO/target/debug/aithericon-executor-service" &
    EXECUTOR_PID=$!
    disown

    echo ""
    echo "4. Starting engine with executor backend..."
    set -a; source infra/executor/executor.env; set +a
    RUST_LOG=info,petri_executor=debug \
    cargo run -p core-engine --features executor &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; kill $EXECUTOR_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "executor watcher"

    echo ""
    echo "5. Deploying executor_net scenario..."
    deploy_scenario executor_net -- --deploy
    NET_ID="executor-lifecycle"
    COMPLETED_ID="completed"

    echo ""
    echo "6. Setting engine to running mode and submitting job..."
    set_nets_running "$NET_ID"
    trigger_eval "$NET_ID"

    echo ""
    echo "7. Waiting for executor job to complete..."
    poll_single_place "$COMPLETED_ID" 1 30 2 "$NET_ID"

    echo ""
    echo "8. Final state:"
    curl -s "http://localhost:3030/api/nets/$NET_ID/state" | python3 -c "
    import json, sys
    s = json.load(sys.stdin)
    tokens = s.get('marking',{}).get('tokens',{}).get('$COMPLETED_ID',[])
    for t in tokens:
        v = t.get('color',{}).get('value',{})
        print(f'   {v.get(\"job_id\",\"?\")}  (execution_id={v.get(\"execution_id\",\"?\")})')
    "
    print_event_summary "$NET_ID"
    print_demo_footer "Executor Demo" "$ENGINE_PID" "executor-stop" \
        "Executor running (PID $EXECUTOR_PID)"

# ============================================
# Vault Secrets Demo
# ============================================

# Stop the vault secrets demo
vault-secrets-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    kill_stale_executor
    docker rm -f vault-demo 2>/dev/null || true
    echo "Vault secrets demo stopped."

# Run the Vault Secrets demo end-to-end
vault-secrets:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"

    echo "=== Vault Secrets Demo ==="
    echo ""
    echo "  Demonstrates Vault response wrapping for secure secret delivery."
    echo "  Secrets flow: Vault KV → petri-lab wraps → NATS → executor unwraps → process env"
    echo ""

    check_executor_repo "$EXECUTOR_REPO"
    kill_stale_engine
    kill_stale_executor

    echo "1. Starting NATS..."
    just infra nats-up

    echo ""
    echo "2. Starting Vault dev server..."
    docker rm -f vault-demo 2>/dev/null || true
    docker run -d --name vault-demo \
        -p 8200:8200 \
        -e VAULT_DEV_ROOT_TOKEN_ID=demo-root-token \
        -e VAULT_DEV_LISTEN_ADDRESS=0.0.0.0:8200 \
        hashicorp/vault:1.15 > /dev/null
    for i in $(seq 1 30); do
        if curl -sf http://localhost:8200/v1/sys/health > /dev/null 2>&1; then
            echo "   Vault is ready"
            break
        fi
        if [ "$i" -eq 30 ]; then echo "ERROR: Vault did not start"; exit 1; fi
        sleep 0.5
    done

    echo ""
    echo "3. Seeding secrets into Vault..."
    curl -s -X POST http://localhost:8200/v1/secret/data/demo/api \
        -H "X-Vault-Token: demo-root-token" \
        -H "Content-Type: application/json" \
        -d '{"data": {"key": "sk-live-demo-12345"}}' > /dev/null
    curl -s -X POST http://localhost:8200/v1/secret/data/demo/db \
        -H "X-Vault-Token: demo-root-token" \
        -H "Content-Type: application/json" \
        -d '{"data": {"password": "super-secret-db-pass"}}' > /dev/null
    echo "   Stored: demo/api#key = sk-live-demo-12345"
    echo "   Stored: demo/db#password = super-secret-db-pass"

    clean_nats_streams

    echo ""
    echo "4. Building engine + scenario + executor..."
    cargo build -p core-engine --features executor,executor-vault-secrets
    cargo build -p aithericon-sdk --example vault_secrets_demo
    cargo build --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service --features vault

    echo ""
    echo "5. Starting aithericon-executor..."
    VAULT_ADDR=http://localhost:8200 \
    EXECUTOR_NATS_URL=nats://localhost:4333 \
    EXECUTOR_NAMESPACE=executor_jobs \
    EXECUTOR_NAME=vault-demo-executor \
    EXECUTOR_BASE_DIR=/tmp/vault-demo-executor \
    EXECUTOR_CONCURRENCY=4 \
    EXECUTOR_DEFAULT_TIMEOUT_SECS=30 \
    EXECUTOR_SOURCE=nats_queue \
    EXECUTOR_LIFETIME=daemon \
    RUST_LOG=info \
    "$EXECUTOR_REPO/target/debug/aithericon-executor-service" &
    EXECUTOR_PID=$!
    disown

    echo ""
    echo "6. Starting engine with Vault secret wrapping..."
    set -a; source infra/executor/vault-secrets-demo.env; set +a
    RUST_LOG=info,petri_executor=debug \
    cargo run -p core-engine --features executor,executor-vault-secrets &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; kill $EXECUTOR_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "executor watcher"

    echo ""
    echo "7. Deploying vault_secrets_demo scenario..."
    deploy_scenario vault_secrets_demo -- --deploy
    NET_ID="vault-secrets-demo"
    COMPLETED_ID="completed"
    DEAD_LETTER_ID="dead_letter"

    echo ""
    echo "8. Setting engine to running mode and submitting job..."
    set_nets_running "$NET_ID"
    trigger_eval "$NET_ID"

    echo ""
    echo "9. Waiting for executor job to complete..."
    for i in $(seq 1 30); do
        sleep 2
        STATE=$(curl -s "http://localhost:3030/api/nets/$NET_ID/state")
        COMPLETED=$(echo "$STATE" | python3 -c "
        import json, sys
        s = json.load(sys.stdin)
        tokens = s.get('marking',{}).get('tokens',{}).get('$COMPLETED_ID',[])
        print(len(tokens))
        " 2>/dev/null || echo "0")
        DEAD=$(echo "$STATE" | python3 -c "
        import json, sys
        s = json.load(sys.stdin)
        tokens = s.get('marking',{}).get('tokens',{}).get('$DEAD_LETTER_ID',[])
        print(len(tokens))
        " 2>/dev/null || echo "0")

        echo "   [$i] completed=$COMPLETED  dead_letter=$DEAD  (target: 1 terminal)"
        if [ "$COMPLETED" -ge 1 ] || [ "$DEAD" -ge 1 ]; then break; fi
        if [ "$i" -eq 30 ]; then echo "WARNING: Timed out waiting for job completion"; fi
    done

    echo ""
    echo "10. Validating results..."
    STATE=$(curl -s "http://localhost:3030/api/nets/$NET_ID/state")
    RESULT=$(echo "$STATE" | python3 -c "
    import json, sys
    s = json.load(sys.stdin)
    tokens = s.get('marking',{}).get('tokens',{}).get('$COMPLETED_ID',[])
    if tokens:
        v = tokens[0].get('color',{}).get('value',{})
        detail = v.get('detail',{})
        output = detail.get('output','')
        print(output)
    else:
        print('NO_COMPLETED_TOKENS')
    " 2>/dev/null || echo "PARSE_ERROR")

    echo ""
    if echo "$RESULT" | grep -q "sk-live-demo-12345" && echo "$RESULT" | grep -q "super-secret-db-pass"; then
        echo "   PASS: Secrets resolved correctly in process output"
        echo "   Output: $RESULT"
    elif echo "$RESULT" | grep -q "SECRETS_RESOLVED"; then
        echo "   PARTIAL: Process ran but secrets may not have resolved"
        echo "   Output: $RESULT"
    elif [ "$DEAD" -ge 1 ]; then
        echo "   FAIL: Job ended in dead_letter (check executor logs)"
    else
        echo "   FAIL: Unexpected result"
        echo "   Output: $RESULT"
    fi

    print_single_event_summary
    print_demo_footer "Vault Secrets Demo" "$ENGINE_PID" "vault-secrets-stop" \
        "Executor running (PID $EXECUTOR_PID)" \
        "Vault running (container: vault-demo)"

# ============================================
# Three-Layer Bridged Net Demo (Nomad)
# ============================================

# Stop the three-layer demo
three-layer-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    just infra nomad-down

# Register the executor job template (injects binary path)
[private]
three-layer-register-template:
    #!/usr/bin/env bash
    set -euo pipefail
    EXECUTOR_REPO="{{executor_repo}}"
    EXECUTOR_BIN="$(cd "$EXECUTOR_REPO" && pwd)/target/debug/aithericon-executor-service"
    if [ ! -f "$EXECUTOR_BIN" ]; then
        echo "ERROR: executor binary not found at $EXECUTOR_BIN"
        echo "Build it first: cargo build --manifest-path $EXECUTOR_REPO/Cargo.toml -p aithericon-executor-service"
        exit 1
    fi
    jq --arg bin "$EXECUTOR_BIN" '.Job.TaskGroups[0].Tasks[0].Config.command = $bin' \
        infra/nomad/executor-job-template.json \
        | curl -sf -X POST http://127.0.0.1:4646/v1/jobs -d @- > /dev/null
    echo "Registered petri-executor-worker job template (binary: $EXECUTOR_BIN)"

# Run the three-layer bridged net demo (job → Nomad → executor)
three-layer:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"

    echo "=== Petri-Lab Three-Layer Bridged Net Demo ==="
    echo ""
    echo "  Layer 1: job-net        (job orchestration + retry)"
    echo "  Layer 2: scheduler relay net  (real Nomad via scheduler_submit)"
    echo "  Layer 3: executor-net   (real executor via executor_submit)"
    echo "  Nomad starts the executor process per job (run_to_completion)"
    echo ""

    check_executor_repo "$EXECUTOR_REPO"
    kill_stale_engine

    echo "1. Starting NATS + Nomad..."
    just infra nats-up
    just infra nomad-up
    purge_nomad_jobs
    clean_nats_streams

    echo ""
    echo "2. Building engine + all three scenarios + executor..."
    cargo build -p core-engine --features nomad,executor
    cargo build -p aithericon-sdk --example job_net
    cargo build -p aithericon-sdk --example scheduler_net
    cargo build -p aithericon-sdk --example executor_net
    cargo build --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service

    echo ""
    echo "3. Registering executor job template..."
    just demo three-layer-register-template

    echo ""
    echo "4. Starting engine with Nomad scheduler + executor backend..."
    set -a; source infra/nomad/nomad.env; source infra/executor/executor.env; set +a
    SCHEDULER_JOB_TEMPLATE=petri-executor-worker \
    RUST_LOG=info,petri_nomad=debug,petri_nats=debug \
    cargo run -p core-engine --features nomad,executor &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Nomad + executor watchers"

    echo ""
    echo "5. Deploying three-layer nets..."
    echo "   Deploying executor-net (Layer 3)..."
    deploy_scenario executor_net -- --bridged --deploy --net-id executor-net > /dev/null
    echo "   Deploying scheduler relay net (Layer 2)..."
    deploy_scenario scheduler_net -- --deploy --net-id scheduler-net > /dev/null
    echo "   Deploying job-net (Layer 1)..."
    deploy_scenario job_net -- --deploy --net-id job-net > /dev/null

    echo ""
    echo "6. Setting all nets to running mode..."
    set_nets_running executor-net scheduler-net job-net
    echo ""
    echo "   Triggering initial evaluation on job-net..."
    trigger_eval job-net

    echo ""
    echo "7. Waiting for jobs to traverse all three layers..."
    echo "   (job-net → Nomad → executor → back through bridges)"
    poll_job_net_terminal 2 60 2

    echo ""
    echo "8. Final state across all three nets:"
    print_net_states job-net scheduler-net executor-net
    echo ""
    print_event_summary job-net scheduler-net executor-net

    print_multi_net_footer "Three-Layer Demo" "$ENGINE_PID" "three-layer-stop" \
        "Nomad UI:    http://localhost:4646/ui"

# ============================================
# Three-Layer Bridged Net Demo (Slurm)
# ============================================

# Stop the Slurm three-layer demo
slurm-three-layer-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    just infra slurm-down

# Run the three-layer bridged net demo with Slurm (job → Slurm → executor)
slurm-three-layer:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"

    echo "=== Petri-Lab Three-Layer Slurm Demo ==="
    echo ""
    echo "  Layer 1: job-net        (job orchestration + retry)"
    echo "  Layer 2: scheduler relay net  (real Slurm via scheduler_submit)"
    echo "  Layer 3: executor-net   (real executor via executor_submit)"
    echo "  Slurm starts the executor process per job (run_to_completion)"
    echo ""

    check_executor_repo "$EXECUTOR_REPO"
    check_cargo_zigbuild
    kill_stale_engine
    chmod 600 infra/slurm/ssh/slurm_test

    echo "1. Starting NATS..."
    just infra nats-up

    echo ""
    echo "2. Building engine + scenarios + cross-compiling executor..."
    cargo build -p core-engine --features slurm,executor
    cargo build -p aithericon-sdk --example job_net
    cargo build -p aithericon-sdk --example scheduler_net
    cargo build -p aithericon-sdk --example executor_net
    cargo zigbuild --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service --target x86_64-unknown-linux-gnu

    EXECUTOR_LINUX_BIN="$EXECUTOR_REPO/target/x86_64-unknown-linux-gnu/debug/aithericon-executor-service"
    if [ ! -f "$EXECUTOR_LINUX_BIN" ]; then
        echo "ERROR: Linux executor binary not found at $EXECUTOR_LINUX_BIN"
        exit 1
    fi

    echo ""
    echo "3. Starting Slurm..."
    just infra slurm-up
    just infra slurm-wait

    echo ""
    echo "4. Installing executor binary into Slurm container..."
    install_executor_in_slurm "$EXECUTOR_LINUX_BIN"
    clean_nats_streams

    echo ""
    echo "5. Starting engine with Slurm scheduler + executor backend..."
    set -a; source infra/slurm/slurm.env; source infra/executor/executor.env; set +a
    SCHEDULER_JOB_TEMPLATE=executor \
    RUST_LOG=info,petri_slurm=debug,petri_nats=debug \
    cargo run -p core-engine --features slurm,executor &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Slurm + executor watchers"

    echo ""
    echo "6. Deploying three-layer nets..."
    echo "   Deploying executor-net (Layer 3)..."
    deploy_scenario executor_net -- --bridged --deploy --net-id executor-net > /dev/null
    echo "   Deploying scheduler relay net (Layer 2)..."
    deploy_scenario scheduler_net -- --deploy --net-id scheduler-net > /dev/null
    echo "   Deploying job-net (Layer 1)..."
    deploy_scenario job_net -- --deploy --net-id job-net > /dev/null

    echo ""
    echo "7. Setting all nets to running mode..."
    set_nets_running executor-net scheduler-net job-net
    echo ""
    echo "   Triggering initial evaluation on job-net..."
    trigger_eval job-net

    echo ""
    echo "8. Waiting for jobs to traverse all three layers..."
    echo "   (job-net → Slurm → executor → back through bridges)"
    poll_job_net_terminal 2 60 2

    echo ""
    echo "9. Final state across all three nets:"
    print_net_states job-net scheduler-net executor-net
    echo ""
    print_event_summary job-net scheduler-net executor-net

    print_multi_net_footer "Three-Layer Slurm Demo" "$ENGINE_PID" "slurm-three-layer-stop" \
        "Slurm:       just infra slurm-ssh"

# ============================================
# Four-Layer Workflow Demo (Nomad)
# ============================================

# Stop the workflow demo
workflow-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    just infra nomad-down

# Run the four-layer workflow demo (workflow → job → Nomad → executor)
workflow:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"

    echo "=== Petri-Lab Four-Layer Workflow Demo ==="
    echo ""
    echo "  Layer 0: workflow-net    (ML pipeline: fan-out, fan-in, chaining)"
    echo "  Layer 1: job-net         (job orchestration + retry)"
    echo "  Layer 2: scheduler relay net   (real Nomad via scheduler_submit)"
    echo "  Layer 3: executor-net    (real executor via executor_submit)"
    echo ""
    echo "  Pipeline: preprocess-A + preprocess-B (parallel)"
    echo "            → train (fan-in gate)  → evaluate (sequential)"
    echo ""

    check_executor_repo "$EXECUTOR_REPO"
    kill_stale_engine

    echo "1. Starting NATS + Nomad..."
    just infra nats-up
    just infra nomad-up
    purge_nomad_jobs
    clean_nats_streams

    echo ""
    echo "2. Building engine + all four scenarios + executor..."
    cargo build -p core-engine --features nomad,executor
    cargo build -p aithericon-sdk --example four_layer_workflow_net
    cargo build -p aithericon-sdk --example job_net
    cargo build -p aithericon-sdk --example scheduler_net
    cargo build -p aithericon-sdk --example executor_net
    cargo build --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service

    echo ""
    echo "3. Registering executor job template..."
    just demo three-layer-register-template

    echo ""
    echo "4. Starting engine with Nomad scheduler + executor backend..."
    set -a; source infra/nomad/nomad.env; source infra/executor/executor.env; set +a
    SCHEDULER_JOB_TEMPLATE=petri-executor-worker \
    RUST_LOG=info,petri_nomad=debug,petri_nats=debug \
    cargo run -p core-engine --features nomad,executor &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Nomad + executor watchers"

    echo ""
    echo "5. Deploying four-layer nets..."
    echo "   Deploying executor-net (Layer 3)..."
    deploy_scenario executor_net -- --bridged --deploy --net-id executor-net > /dev/null
    echo "   Deploying scheduler relay net (Layer 2)..."
    deploy_scenario scheduler_net -- --deploy --net-id scheduler-net > /dev/null
    echo "   Deploying job-net (Layer 1)..."
    deploy_scenario job_net -- --bridged --deploy --net-id job-net > /dev/null
    echo "   Deploying workflow-net (Layer 0)..."
    deploy_scenario four_layer_workflow_net -- --deploy --net-id workflow-net > /dev/null

    echo ""
    echo "6. Setting all nets to running mode..."
    set_nets_running executor-net scheduler-net job-net workflow-net

    echo ""
    echo "7. Triggering initial evaluation on workflow-net..."
    trigger_eval workflow-net

    echo ""
    echo "8. Waiting for workflow to complete across all four layers..."
    echo "   Pipeline: preprocess-A + preprocess-B → train → evaluate"
    echo "   (each step: workflow → job → scheduler/Nomad → executor → back)"
    poll_workflow_terminal 120 2

    echo ""
    echo "9. Final state across all four nets:"
    print_net_states workflow-net job-net scheduler-net executor-net
    echo ""
    print_event_summary workflow-net job-net scheduler-net executor-net

    print_multi_net_footer "Four-Layer Workflow Demo" "$ENGINE_PID" "workflow-stop" \
        "Nomad UI:    http://localhost:4646/ui"

# ============================================
# Four-Layer Workflow Demo (Slurm)
# ============================================

# Stop the Slurm workflow demo
slurm-workflow-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    just infra slurm-down

# Run the four-layer workflow demo with Slurm (workflow → job → Slurm → executor)
slurm-workflow:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"

    echo "=== Petri-Lab Four-Layer Slurm Workflow Demo ==="
    echo ""
    echo "  Layer 0: workflow-net    (ML pipeline: fan-out, fan-in, chaining)"
    echo "  Layer 1: job-net         (job orchestration + retry)"
    echo "  Layer 2: scheduler relay net   (real Slurm via scheduler_submit)"
    echo "  Layer 3: executor-net    (real executor via executor_submit)"
    echo ""
    echo "  Pipeline: preprocess-A + preprocess-B (parallel)"
    echo "            → train (fan-in gate)  → evaluate (sequential)"
    echo ""

    check_executor_repo "$EXECUTOR_REPO"
    check_cargo_zigbuild
    kill_stale_engine
    chmod 600 infra/slurm/ssh/slurm_test

    echo "1. Starting NATS..."
    just infra nats-up

    echo ""
    echo "2. Building engine + scenarios + cross-compiling executor..."
    cargo build -p core-engine --features slurm,executor
    cargo build -p aithericon-sdk --example four_layer_workflow_net
    cargo build -p aithericon-sdk --example job_net
    cargo build -p aithericon-sdk --example scheduler_net
    cargo build -p aithericon-sdk --example executor_net
    cargo zigbuild --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service --target x86_64-unknown-linux-gnu

    EXECUTOR_LINUX_BIN="$EXECUTOR_REPO/target/x86_64-unknown-linux-gnu/debug/aithericon-executor-service"
    if [ ! -f "$EXECUTOR_LINUX_BIN" ]; then
        echo "ERROR: Linux executor binary not found at $EXECUTOR_LINUX_BIN"
        exit 1
    fi

    echo ""
    echo "3. Starting Slurm..."
    just infra slurm-up
    just infra slurm-wait

    echo ""
    echo "4. Installing executor binary into Slurm container..."
    install_executor_in_slurm "$EXECUTOR_LINUX_BIN"
    clean_nats_streams

    echo ""
    echo "5. Starting engine with Slurm scheduler + executor backend..."
    set -a; source infra/slurm/slurm.env; source infra/executor/executor.env; set +a
    SCHEDULER_JOB_TEMPLATE=executor \
    RUST_LOG=info,petri_slurm=debug,petri_nats=debug \
    cargo run -p core-engine --features slurm,executor &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Slurm + executor watchers"

    echo ""
    echo "6. Deploying four-layer nets..."
    echo "   Deploying executor-net (Layer 3)..."
    deploy_scenario executor_net -- --bridged --deploy --net-id executor-net > /dev/null
    echo "   Deploying scheduler relay net (Layer 2)..."
    deploy_scenario scheduler_net -- --deploy --net-id scheduler-net > /dev/null
    echo "   Deploying job-net (Layer 1)..."
    deploy_scenario job_net -- --bridged --deploy --net-id job-net > /dev/null
    echo "   Deploying workflow-net (Layer 0)..."
    deploy_scenario four_layer_workflow_net -- --deploy --net-id workflow-net > /dev/null

    echo ""
    echo "7. Setting all nets to running mode..."
    set_nets_running executor-net scheduler-net job-net workflow-net

    echo ""
    echo "8. Triggering initial evaluation on workflow-net..."
    trigger_eval workflow-net

    echo ""
    echo "9. Waiting for workflow to complete across all four layers..."
    echo "   Pipeline: preprocess-A + preprocess-B → train → evaluate"
    echo "   (each step: workflow → job → scheduler/Slurm → executor → back)"
    poll_workflow_terminal 120 2

    echo ""
    echo "10. Final state across all four nets:"
    print_net_states workflow-net job-net scheduler-net executor-net
    echo ""
    print_event_summary workflow-net job-net scheduler-net executor-net

    print_multi_net_footer "Four-Layer Slurm Workflow Demo" "$ENGINE_PID" "slurm-workflow-stop" \
        "Slurm:       just infra slurm-ssh"

# ============================================
# Five-Layer Campaign Demo (Nomad)
# ============================================

# Stop the campaign demo
campaign-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    just infra nomad-down

# Run the five-layer campaign demo (campaign → workflow → job → Nomad → executor)
campaign:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"

    echo "=== Petri-Lab Five-Layer Campaign Demo ==="
    echo ""
    echo "  Layer 0: campaign-net    (hyperparameter sweep: fan-out 2 configs, fan-in select)"
    echo "  Layer 1: workflow-net    (parameterized ML pipeline: fan-out, fan-in, chaining)"
    echo "  Layer 2: job-net         (job orchestration + retry)"
    echo "  Layer 3: scheduler relay net   (real Nomad via scheduler_submit)"
    echo "  Layer 4: executor-net    (real executor via executor_submit)"
    echo ""
    echo "  Sweep: lr=0.01 (batch=32)  vs  lr=0.001 (batch=64)"
    echo "  Each config runs: preprocess-A + preprocess-B → train → evaluate"
    echo "  Total: 8 real jobs through the scheduler (2 configs × 4 steps)"
    echo ""

    check_executor_repo "$EXECUTOR_REPO"
    kill_stale_engine

    echo "1. Starting NATS + Nomad..."
    just infra nats-up
    just infra nomad-up
    purge_nomad_jobs
    clean_nats_streams

    echo ""
    echo "2. Building engine + all five scenarios + executor..."
    cargo build -p core-engine --features nomad,executor
    cargo build -p aithericon-sdk --example five_layer_campaign_net
    cargo build -p aithericon-sdk --example five_layer_workflow_net
    cargo build -p aithericon-sdk --example job_net
    cargo build -p aithericon-sdk --example scheduler_net
    cargo build -p aithericon-sdk --example executor_net
    cargo build --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service

    echo ""
    echo "3. Registering executor job template..."
    just demo three-layer-register-template

    echo ""
    echo "4. Starting engine with Nomad scheduler + executor backend..."
    set -a; source infra/nomad/nomad.env; source infra/executor/executor.env; set +a
    SCHEDULER_JOB_TEMPLATE=petri-executor-worker \
    RUST_LOG=info,petri_nomad=debug,petri_nats=debug \
    cargo run -p core-engine --features nomad,executor &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Nomad + executor watchers"

    echo ""
    echo "5. Deploying five-layer nets..."
    echo "   Deploying executor-net (Layer 4)..."
    deploy_scenario executor_net -- --bridged --deploy --net-id executor-net > /dev/null
    echo "   Deploying scheduler relay net (Layer 3)..."
    deploy_scenario scheduler_net -- --deploy --net-id scheduler-net > /dev/null
    echo "   Deploying job-net (Layer 2)..."
    deploy_scenario job_net -- --bridged --deploy --net-id job-net > /dev/null
    echo "   Deploying workflow-net (Layer 1)..."
    deploy_scenario five_layer_workflow_net -- --deploy --net-id workflow-net > /dev/null
    echo "   Deploying campaign-net (Layer 0)..."
    deploy_scenario five_layer_campaign_net -- --deploy --net-id campaign-net > /dev/null

    echo ""
    echo "6. Setting all nets to running mode..."
    set_nets_running executor-net scheduler-net job-net workflow-net campaign-net

    echo ""
    echo "7. Triggering initial evaluation on campaign-net..."
    trigger_eval campaign-net

    echo ""
    echo "8. Waiting for campaign to complete across all five layers..."
    echo "   Config A (lr=0.01): preprocess-A/B → train → evaluate"
    echo "   Config B (lr=0.001): preprocess-A/B → train → evaluate"
    echo "   (each step: campaign → workflow → job → scheduler/Nomad → executor → back)"
    poll_campaign_terminal 180 2

    echo ""
    echo "9. Final state across all five nets:"
    print_net_states campaign-net workflow-net job-net scheduler-net executor-net
    echo ""
    echo "   Campaign results:"
    print_campaign_results
    echo ""
    print_event_summary campaign-net workflow-net job-net scheduler-net executor-net

    print_multi_net_footer "Five-Layer Campaign Demo" "$ENGINE_PID" "campaign-stop" \
        "Nomad UI:    http://localhost:4646/ui"

# ============================================
# Five-Layer Campaign Demo (Slurm)
# ============================================

# Stop the Slurm campaign demo
slurm-campaign-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    just infra slurm-down

# Run the five-layer campaign demo with Slurm (campaign → workflow → job → Slurm → executor)
slurm-campaign:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"

    echo "=== Petri-Lab Five-Layer Slurm Campaign Demo ==="
    echo ""
    echo "  Layer 0: campaign-net    (hyperparameter sweep: fan-out 2 configs, fan-in select)"
    echo "  Layer 1: workflow-net    (parameterized ML pipeline: fan-out, fan-in, chaining)"
    echo "  Layer 2: job-net         (job orchestration + retry)"
    echo "  Layer 3: scheduler relay net   (real Slurm via scheduler_submit)"
    echo "  Layer 4: executor-net    (real executor via executor_submit)"
    echo ""
    echo "  Sweep: lr=0.01 (batch=32)  vs  lr=0.001 (batch=64)"
    echo "  Each config runs: preprocess-A + preprocess-B → train → evaluate"
    echo "  Total: 8 real jobs through the scheduler (2 configs × 4 steps)"
    echo ""

    check_executor_repo "$EXECUTOR_REPO"
    check_cargo_zigbuild
    kill_stale_engine
    chmod 600 infra/slurm/ssh/slurm_test

    echo "1. Starting NATS..."
    just infra nats-up

    echo ""
    echo "2. Building engine + scenarios + cross-compiling executor..."
    cargo build -p core-engine --features slurm,executor
    cargo build -p aithericon-sdk --example five_layer_campaign_net
    cargo build -p aithericon-sdk --example five_layer_workflow_net
    cargo build -p aithericon-sdk --example job_net
    cargo build -p aithericon-sdk --example scheduler_net
    cargo build -p aithericon-sdk --example executor_net
    cargo zigbuild --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service --target x86_64-unknown-linux-gnu

    EXECUTOR_LINUX_BIN="$EXECUTOR_REPO/target/x86_64-unknown-linux-gnu/debug/aithericon-executor-service"
    if [ ! -f "$EXECUTOR_LINUX_BIN" ]; then
        echo "ERROR: Linux executor binary not found at $EXECUTOR_LINUX_BIN"
        exit 1
    fi

    echo ""
    echo "3. Starting Slurm..."
    just infra slurm-up
    just infra slurm-wait

    echo ""
    echo "4. Installing executor binary into Slurm container..."
    install_executor_in_slurm "$EXECUTOR_LINUX_BIN"
    clean_nats_streams

    echo ""
    echo "5. Starting engine with Slurm scheduler + executor backend..."
    set -a; source infra/slurm/slurm.env; source infra/executor/executor.env; set +a
    SCHEDULER_JOB_TEMPLATE=executor \
    RUST_LOG=info,petri_slurm=debug,petri_nats=debug \
    cargo run -p core-engine --features slurm,executor &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Slurm + executor watchers"

    echo ""
    echo "6. Deploying five-layer nets..."
    echo "   Deploying executor-net (Layer 4)..."
    deploy_scenario executor_net -- --bridged --deploy --net-id executor-net > /dev/null
    echo "   Deploying scheduler relay net (Layer 3)..."
    deploy_scenario scheduler_net -- --deploy --net-id scheduler-net > /dev/null
    echo "   Deploying job-net (Layer 2)..."
    deploy_scenario job_net -- --bridged --deploy --net-id job-net > /dev/null
    echo "   Deploying workflow-net (Layer 1)..."
    deploy_scenario five_layer_workflow_net -- --deploy --net-id workflow-net > /dev/null
    echo "   Deploying campaign-net (Layer 0)..."
    deploy_scenario five_layer_campaign_net -- --deploy --net-id campaign-net > /dev/null

    echo ""
    echo "7. Setting all nets to running mode..."
    set_nets_running executor-net scheduler-net job-net workflow-net campaign-net

    echo ""
    echo "8. Triggering initial evaluation on campaign-net..."
    trigger_eval campaign-net

    echo ""
    echo "9. Waiting for campaign to complete across all five layers..."
    echo "   Config A (lr=0.01): preprocess-A/B → train → evaluate"
    echo "   Config B (lr=0.001): preprocess-A/B → train → evaluate"
    echo "   (each step: campaign → workflow → job → scheduler/Slurm → executor → back)"
    poll_campaign_terminal 180 2

    echo ""
    echo "10. Final state across all five nets:"
    print_net_states campaign-net workflow-net job-net scheduler-net executor-net
    echo ""
    echo "   Campaign results:"
    print_campaign_results
    echo ""
    print_event_summary campaign-net workflow-net job-net scheduler-net executor-net

    print_multi_net_footer "Five-Layer Slurm Campaign Demo" "$ENGINE_PID" "slurm-campaign-stop" \
        "Slurm:       just infra slurm-ssh"

# ============================================
# Python IPC Demo (Nomad)
# ============================================

# Stop the Python IPC demo
python-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    just infra nomad-down

# Run the Python IPC demo (job → Nomad → executor with full IPC)
python:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"

    echo "=== Petri-Lab Python IPC Demo ==="
    echo ""
    echo "  Layer 1: job-net        (seeds Python training job)"
    echo "  Layer 2: scheduler relay net  (Nomad relay via scheduler_submit)"
    echo "  Layer 3: executor-net   (full IPC: progress, artifact, metric, phase, output, log)"
    echo "  Python script exercises the entire aithericon IPC protocol"
    echo ""

    check_executor_repo "$EXECUTOR_REPO"
    kill_stale_engine

    echo "1. Starting NATS + Nomad..."
    just infra nats-up
    just infra nomad-up
    purge_nomad_jobs
    clean_nats_streams

    echo ""
    echo "2. Building engine + scenarios + executor (with Python backend)..."
    cargo build -p core-engine --features nomad,executor
    cargo build -p aithericon-sdk --example job_net
    cargo build -p aithericon-sdk --example scheduler_net
    cargo build -p aithericon-sdk --example executor_net
    cargo build --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service --features python,file-metadata

    echo ""
    echo "3. Registering executor job template..."
    just demo three-layer-register-template

    echo ""
    echo "4. Starting engine with Nomad scheduler + executor backend..."
    set -a; source infra/nomad/nomad.env; source infra/executor/python-ipc-demo.env; set +a
    SCHEDULER_JOB_TEMPLATE=petri-executor-worker \
    RUST_LOG=info,petri_nomad=debug,petri_nats=debug,petri_executor=debug \
    cargo run -p core-engine --features nomad,executor &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Nomad + executor watchers"

    echo ""
    echo "5. Deploying three-layer nets..."
    echo "   Deploying executor-net (Layer 3)..."
    deploy_scenario executor_net -- --bridged --deploy --net-id executor-net > /dev/null
    echo "   Deploying scheduler relay net (Layer 2)..."
    deploy_scenario scheduler_net -- --deploy --net-id scheduler-net > /dev/null
    echo "   Deploying job-net (Layer 1)..."
    deploy_scenario job_net -- --deploy --net-id job-net > /dev/null

    echo ""
    echo "6. Setting all nets to running mode..."
    set_nets_running executor-net scheduler-net job-net
    echo ""
    echo "   Triggering initial evaluation on job-net..."
    trigger_eval job-net

    echo ""
    echo "7. Waiting for Python job to traverse all three layers..."
    echo "   (job-net -> Nomad -> executor -> Python IPC -> bridges back)"
    echo "   First run creates a virtualenv + installs aithericon SDK — this may take a moment."
    poll_job_net_terminal 1 90 2

    echo ""
    echo "8. Final state across all three nets:"
    print_net_states job-net scheduler-net executor-net
    echo ""
    print_ipc_summary
    echo ""
    print_event_summary job-net scheduler-net executor-net

    print_multi_net_footer "Python IPC Demo" "$ENGINE_PID" "python-stop" \
        "Nomad UI:    http://localhost:4646/ui"

# ============================================
# Slurm Python IPC Demo
# ============================================

# Stop the Slurm Python IPC demo
slurm-python-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    just infra slurm-down

# Run the Python IPC demo on Slurm (job → Slurm → executor with full IPC)
slurm-python:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"

    echo "=== Petri-Lab Python IPC Demo (Slurm) ==="
    echo ""
    echo "  Layer 1: job-net        (seeds Python training job)"
    echo "  Layer 2: scheduler relay net  (Slurm relay via scheduler_submit)"
    echo "  Layer 3: executor-net   (full IPC: progress, artifact, metric, phase, output, log)"
    echo "  Python script exercises the entire aithericon IPC protocol"
    echo ""

    check_executor_repo "$EXECUTOR_REPO"
    check_cargo_zigbuild
    kill_stale_engine
    chmod 600 infra/slurm/ssh/slurm_test

    echo "1. Starting NATS..."
    just infra nats-up

    echo ""
    echo "2. Building engine + scenarios + cross-compiling executor (with Python backend)..."
    cargo build -p core-engine --features slurm,executor
    cargo build -p aithericon-sdk --example job_net
    cargo build -p aithericon-sdk --example scheduler_net
    cargo build -p aithericon-sdk --example executor_net
    cargo zigbuild --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service --features python,file-metadata --target x86_64-unknown-linux-gnu

    EXECUTOR_LINUX_BIN="$EXECUTOR_REPO/target/x86_64-unknown-linux-gnu/debug/aithericon-executor-service"
    if [ ! -f "$EXECUTOR_LINUX_BIN" ]; then
        echo "ERROR: Linux executor binary not found at $EXECUTOR_LINUX_BIN"
        exit 1
    fi

    echo ""
    echo "3. Starting Slurm..."
    just infra slurm-up
    just infra slurm-wait

    echo ""
    echo "4. Installing executor binary + Python SDK into Slurm container..."
    install_executor_and_sdk_in_slurm "$EXECUTOR_LINUX_BIN" "$EXECUTOR_REPO/packages/aithericon-sdk"
    clean_nats_streams

    echo ""
    echo "5. Starting engine with Slurm scheduler + executor backend..."
    set -a; source infra/slurm/slurm.env; source infra/executor/python-ipc-demo.env; set +a
    SCHEDULER_JOB_TEMPLATE=executor \
    RUST_LOG=info,petri_slurm=debug,petri_nats=debug,petri_executor=debug \
    cargo run -p core-engine --features slurm,executor &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Slurm + executor watchers"

    echo ""
    echo "6. Deploying three-layer nets..."
    echo "   Deploying executor-net (Layer 3)..."
    deploy_scenario executor_net -- --bridged --deploy --net-id executor-net > /dev/null
    echo "   Deploying scheduler relay net (Layer 2)..."
    deploy_scenario scheduler_net -- --deploy --net-id scheduler-net > /dev/null
    echo "   Deploying job-net (Layer 1)..."
    deploy_scenario job_net -- --deploy --net-id job-net > /dev/null

    echo ""
    echo "7. Setting all nets to running mode..."
    set_nets_running executor-net scheduler-net job-net
    echo ""
    echo "   Triggering initial evaluation on job-net..."
    trigger_eval job-net

    echo ""
    echo "8. Waiting for Python job to traverse all three layers..."
    echo "   (job-net -> Slurm -> executor -> Python IPC -> bridges back)"
    echo "   First run creates a virtualenv + installs aithericon SDK — this may take a moment."
    poll_job_net_terminal 1 120 2

    echo ""
    echo "9. Final state across all three nets:"
    print_net_states job-net scheduler-net executor-net
    echo ""
    print_ipc_summary
    echo ""
    print_event_summary job-net scheduler-net executor-net

    print_multi_net_footer "Python IPC Demo (Slurm)" "$ENGINE_PID" "slurm-python-stop" \
        "Slurm:       just infra slurm-ssh"

# ============================================
# Clockmaster / Durable Timer Demo
# ============================================

# Stop the timer demo
timer-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter

# Run the durable timer demo (tests engine restart + timer survival)
timer:
    #!/usr/bin/env bash
    source {{_lib}}

    echo "=== Petri-Lab Durable Timer Demo ==="
    kill_stale_engine
    just infra nats-up
    clean_nats_streams
    nats -s nats://localhost:4333 kv del PETRI_TIMERS --force 2>/dev/null || true
    cargo build -p core-engine
    cargo build -p aithericon-sdk --example durable_timer
    RUST_LOG=info cargo run -p core-engine &
    EPID=$!
    trap 'kill $EPID 2>/dev/null; wait $EPID 2>/dev/null' EXIT
    wait_for_engine_api
    start_trace_exporter
    ./target/debug/examples/durable_timer -- --deploy
    NET_ID="durable-timer"

    WAITING_ID="waiting"
    COMPLETED_ID="completed"

    ./target/debug/aithericon activate "$NET_ID"
    curl -s -X POST -H "Content-Type: application/json" -d '{}' "http://localhost:3030/api/nets/$NET_ID/command/evaluate" > /dev/null

    echo "   Engine started (PID $EPID). Timer scheduled."
    sleep 2

    echo "   Killing engine now..."
    kill $EPID
    sleep 2

    echo "   Restarting engine..."
    RUST_LOG=info cargo run -p core-engine &
    EPID=$!
    trap 'kill $EPID 2>/dev/null; wait $EPID 2>/dev/null' EXIT
    wait_for_engine_api
    start_trace_exporter

    # DO NOT REDEPLOY. The topology should be hydrated from NATS.

    echo "   Restoring RunMode to 'Running'..."
    ./target/debug/aithericon activate "$NET_ID"

    echo "   Waiting for timer to fire..."
    for i in $(seq 1 20); do
        sleep 1
        STATE=$(curl -s "http://localhost:3030/api/nets/$NET_ID/state")
        COMPLETED=$(echo "$STATE" | python3 -c "import json,sys; s=json.load(sys.stdin); print(len(s.get('marking',{}).get('tokens',{}).get('$COMPLETED_ID',[])))")
        echo "   [${i}s] completed: $COMPLETED"
        if [ "$COMPLETED" -ge 1 ]; then
            echo "=== SUCCESS: Timer fired! ==="

            TOKEN_DATA=$(echo "$STATE" | python3 -c "import json,sys; s=json.load(sys.stdin); print(json.dumps(s.get('marking',{}).get('tokens',{}).get('$COMPLETED_ID',[])[0].get('color',{}).get('value', {})))")
            echo "   Token Data: $TOKEN_DATA"
            DRIFT=$(echo "$TOKEN_DATA" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('drift_ms', -1) if d else -1)")
            SCHEDULED=$(echo "$TOKEN_DATA" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('scheduled_at', 'MISSING') if d else 'MISSING')")
            TRIGGERED=$(echo "$TOKEN_DATA" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('triggered_at', 'MISSING') if d else 'MISSING')")

            echo "   Drift: $DRIFT ms"
            echo "   Scheduled: $SCHEDULED"
            echo "   Triggered: $TRIGGERED"

            if [ "$DRIFT" -ge 0 ] && [ "$SCHEDULED" != "MISSING" ] && [ "$TRIGGERED" != "MISSING" ]; then
                echo "   Metadata confirmed!"
                kill $EPID
                exit 0
            else
                echo "   ERROR: Metadata missing!"
                kill $EPID
                exit 1
            fi
        fi
    done
    kill $EPID
    echo "ERROR: Timeout"
    exit 1

# ============================================
# Human Task Demo
# ============================================

# Stop the engine and human-ui
human-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    stop_on_port 5173 "human-ui"

# Run the human task demo
human:
    #!/usr/bin/env bash
    source {{_lib}}

    HUMAN_UI_REPO="../aithericon-human-ui"

    echo "=== Petri-Lab Human Task Demo ==="
    echo ""

    kill_stale_engine

    echo "1. Starting NATS..."
    just infra nats-up
    echo "   Cleaning global stream..."
    nats -s nats://localhost:4333 stream delete PETRI_GLOBAL -f 2>/dev/null || true

    echo ""
    echo "2. Building engine + scenario..."
    cargo build -p core-engine
    cargo build -p aithericon-sdk --example human_task_net

    echo ""
    echo "3. Starting human-ui..."
    start_human_ui "$HUMAN_UI_REPO" || exit 1

    echo ""
    echo "4. Starting engine with human integration..."
    RUST_LOG=info,petri_nats=debug \
    cargo run -p core-engine &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter

    echo ""
    echo "5. Deploying human_task_net scenario..."
    ./target/debug/examples/human_task_net --deploy --net-id human-demo

    echo ""
    echo "6. Setting engine to running mode and triggering task..."
    ./target/debug/aithericon activate human-demo
    EVAL=$(curl -s -X POST -H "Content-Type: application/json" -d '{}' http://localhost:3030/api/nets/human-demo/command/evaluate)
    STEPS=$(echo "$EVAL" | python3 -c "import json,sys; print(json.load(sys.stdin).get('steps_executed','?'))" 2>/dev/null || echo "?")
    echo "   Fired $STEPS steps (task request published to NATS)"

    echo ""
    echo "=== Demo Setup Complete ==="
    echo ""
    echo "1. Open Human UI: http://localhost:5173"
    echo "2. You should see 'Petri Verification Task' in the queue."
    echo "3. Fill the form and Submit."
    echo "4. The engine will receive the signal and move the token to 'completed'."
    echo ""
    echo "Engine running on http://localhost:3030 (PID $ENGINE_PID)"
    echo "Stop with: just demo human-stop"

# ============================================
# Expense Approval Demo (Python + Human + Timer)
# ============================================

# Stop the expense approval demo
expense-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    stop_on_port 5173 "human-ui"
    kill_stale_executor

# Run the expense approval demo (Python executor, Human-in-the-loop, Timer)
expense:
    #!/usr/bin/env bash
    source {{_lib}}

    HUMAN_UI_REPO="../aithericon-human-ui"
    EXECUTOR_REPO="../aithericon-executor"

    echo "=== Expense Approval Demo ==="
    echo "Combines: Python executor, Human-in-the-loop, Timer"
    echo ""

    echo "1. Checking prerequisites..."
    if [ ! -d "$HUMAN_UI_REPO" ]; then
        echo "ERROR: Human UI repo not found at $HUMAN_UI_REPO"
        exit 1
    fi
    check_executor_repo "$EXECUTOR_REPO"
    if ! command -v deno &> /dev/null; then
        echo "ERROR: deno not found. Install from https://deno.land"
        exit 1
    fi
    echo "   Prerequisites OK"

    echo ""
    echo "2. Cleaning up stale processes..."
    lsof -ti :3030 | xargs kill 2>/dev/null || true
    lsof -ti :5173 | xargs kill 2>/dev/null || true
    pkill -f "aithericon-executor-service" 2>/dev/null || true

    echo ""
    echo "3. Starting NATS..."
    just infra nats-up
    echo "   Cleaning stale executor streams..."
    for STREAM in \
        executor_jobs_high executor_jobs_medium executor_jobs_low executor_jobs_dlq \
        EXECUTOR_STATUS EXECUTOR_EVENTS; do
        nats -s nats://localhost:4333 stream delete "$STREAM" -f 2>/dev/null || true
    done
    echo "   Cleaning stale human task streams..."
    nats -s nats://localhost:4333 consumer del HUMAN_REQUESTS human-ui -f 2>/dev/null || true
    nats -s nats://localhost:4333 stream purge HUMAN_REQUESTS -f 2>/dev/null || true
    nats -s nats://localhost:4333 consumer del HUMAN_COMPLETED human-ui-completed -f 2>/dev/null || true
    nats -s nats://localhost:4333 stream purge HUMAN_COMPLETED -f 2>/dev/null || true

    echo ""
    echo "4. Building engine, executor (with Python), and example..."
    cargo build -p core-engine --features executor
    cargo build -p aithericon-sdk --example expense_approval_net
    cargo build --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service --features python

    echo ""
    echo "5. Starting aithericon-executor..."
    set -a; source infra/executor/aithericon-executor.env; set +a
    RUST_LOG=info \
    "$EXECUTOR_REPO/target/debug/aithericon-executor-service" > /tmp/expense-executor.log 2>&1 &
    EXECUTOR_PID=$!
    disown
    sleep 2
    echo "   Executor started (PID $EXECUTOR_PID)"

    echo ""
    echo "6. Starting Human UI..."
    start_human_ui "$HUMAN_UI_REPO" /tmp/human-ui.log || exit 1

    echo ""
    echo "7. Starting engine..."
    set -a; source infra/executor/executor.env; set +a
    CLOCKMASTER_ENABLED=true \
    EXECUTOR_SIGNAL_ROUTES=accepted:sig_accepted,running:sig_running,completed:sig_analysis_complete,failed:sig_analysis_failed \
    RUST_LOG=info,petri_application=debug,petri_executor=debug \
    ./target/debug/core-engine > /tmp/expense-engine.log 2>&1 &
    ENGINE_PID=$!
    for i in $(seq 1 30); do
        if curl -s http://localhost:3030/health > /dev/null 2>&1; then
            echo "   Engine ready (PID $ENGINE_PID)"
            break
        fi
        if [ $i -eq 30 ]; then
            echo "ERROR: Engine did not start in time"
            exit 1
        fi
        sleep 1
    done
    sleep 3

    echo ""
    echo "8. Deploying expense-approval scenario..."
    ./target/debug/examples/expense_approval_net --deploy --net-id expense-approval

    echo ""
    echo "9. Setting engine to running mode..."
    ./target/debug/aithericon activate expense-approval
    EVAL=$(curl -s -X POST -H "Content-Type: application/json" -d '{}' http://localhost:3030/api/nets/expense-approval/command/evaluate)
    STEPS=$(echo "$EVAL" | python3 -c "import json,sys; print(json.load(sys.stdin).get('steps_executed','?'))" 2>/dev/null || echo "?")
    echo "   Fired $STEPS steps"

    echo ""
    echo "=== Demo Setup Complete ==="
    echo ""
    echo "Two test expenses have been seeded:"
    echo "  - EXP-001: \$45 meals (low risk, should auto-approve)"
    echo "  - EXP-002: \$2500 equipment (high risk, needs human review)"
    echo ""
    echo "The Python analyzer will run and route expenses based on risk."
    echo ""
    echo "1. Open Human UI: http://localhost:5173"
    echo "2. High-risk expenses will appear for manager approval"
    echo "3. If no response within 5 minutes, the SLA timer escalates"
    echo ""
    echo "View logs:"
    echo "  Engine:   tail -f /tmp/expense-engine.log"
    echo "  Executor: tail -f /tmp/expense-executor.log"
    echo "  Human UI: tail -f /tmp/human-ui.log"
    echo ""
    echo "Stop with: just demo expense-stop"

# ============================================
# Research Brief Generator Demo (Full Feature)
# ============================================

# Stop the research brief demo
research-brief-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    stop_on_port 5173 "human-ui"
    kill_stale_executor
    just infra nomad-down
    docker rm -f vault-demo 2>/dev/null || true
    docker rm -f petri-rustfs 2>/dev/null || true
    echo "Research brief demo stopped."

# Run the Research Brief Generator demo (all 7 features)
research-brief:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"
    HUMAN_UI_REPO="../aithericon-human-ui"

    echo "=== Research Brief Generator Demo ==="
    echo ""
    echo "  Demonstrates ALL major Petri-Lab features in one workflow:"
    echo "    1. Nomad scheduler        (job dispatch via scheduler relay net)"
    echo "    2. Python executor         (data gathering with IPC)"
    echo "    3. Vault secret injection  (API keys wrapped + delivered)"
    echo "    4. File transfer + RustFS   (copy to S3-compatible object storage)"
    echo "    5. LLM/Ollama              (structured report generation)"
    echo "    6. Human UI (data entry)   (researcher enters parameters)"
    echo "    7. Human UI (review)       (analyst approves/rejects report)"
    echo ""
    echo "  Architecture: 4-layer bridged nets"
    echo "    Layer 0: orchestrator-net  (human tasks + sequencing)"
    echo "    Layer 1: job-net           (retry + dead-letter)"
    echo "    Layer 2: scheduler relay net     (Nomad dispatch)"
    echo "    Layer 3: executor-net      (executor lifecycle)"
    echo ""

    # -- Prerequisites --
    echo "1. Checking prerequisites..."
    check_executor_repo "$EXECUTOR_REPO"
    if [ ! -d "$HUMAN_UI_REPO" ]; then
        echo "ERROR: Human UI repo not found at $HUMAN_UI_REPO"
        exit 1
    fi
    if ! command -v deno &> /dev/null; then
        echo "ERROR: deno not found. Install from https://deno.land"
        exit 1
    fi
    if ! curl -sf --max-time 2 http://localhost:11434/api/tags > /dev/null 2>&1; then
        echo "WARNING: Ollama not running at localhost:11434"
        echo "  Start it with: ollama serve"
        echo "  The LLM step will fail without Ollama."
    fi
    echo "   Prerequisites OK"

    # -- Clean up --
    echo ""
    echo "2. Cleaning up stale processes..."
    kill_stale_engine
    kill_stale_executor
    lsof -ti :5173 | xargs kill 2>/dev/null || true

    # -- NATS + Nomad + RustFS --
    echo ""
    echo "3. Starting NATS + Nomad + RustFS..."
    just infra nats-up
    just infra nomad-up
    just infra rustfs-up
    purge_nomad_jobs
    clean_nats_streams
    # Clean stale executor + human task streams
    for STREAM in \
        executor_jobs_high executor_jobs_medium executor_jobs_low executor_jobs_dlq \
        EXECUTOR_STATUS EXECUTOR_EVENTS; do
        nats -s nats://localhost:4333 stream delete "$STREAM" -f 2>/dev/null || true
    done
    nats -s nats://localhost:4333 consumer del HUMAN_REQUESTS human-ui -f 2>/dev/null || true
    nats -s nats://localhost:4333 stream purge HUMAN_REQUESTS -f 2>/dev/null || true
    nats -s nats://localhost:4333 consumer del HUMAN_COMPLETED human-ui-completed -f 2>/dev/null || true
    nats -s nats://localhost:4333 stream purge HUMAN_COMPLETED -f 2>/dev/null || true

    # -- Vault --
    echo ""
    echo "4. Starting Vault dev server..."
    docker rm -f vault-demo 2>/dev/null || true
    docker run -d --name vault-demo \
        -p 8200:8200 \
        -e VAULT_DEV_ROOT_TOKEN_ID=demo-root-token \
        -e VAULT_DEV_LISTEN_ADDRESS=0.0.0.0:8200 \
        hashicorp/vault:1.15 > /dev/null
    for i in $(seq 1 30); do
        if curl -sf http://localhost:8200/v1/sys/health > /dev/null 2>&1; then
            echo "   Vault is ready"
            break
        fi
        if [ "$i" -eq 30 ]; then echo "ERROR: Vault did not start"; exit 1; fi
        sleep 0.5
    done

    echo "   Seeding secrets into Vault..."
    curl -s -X POST http://localhost:8200/v1/secret/data/demo/api \
        -H "X-Vault-Token: demo-root-token" \
        -H "Content-Type: application/json" \
        -d '{"data": {"key": "sk-live-research-demo-key"}}' > /dev/null
    curl -s -X POST http://localhost:8200/v1/secret/data/demo/datasource \
        -H "X-Vault-Token: demo-root-token" \
        -H "Content-Type: application/json" \
        -d '{"data": {"token": "ds-live-research-token-42"}}' > /dev/null
    echo "   Stored: demo/api#key, demo/datasource#token"

    # -- Pull Ollama model --
    echo ""
    echo "5. Checking Ollama model..."
    if curl -sf --max-time 2 http://localhost:11434/api/tags > /dev/null 2>&1; then
        if ! curl -s http://localhost:11434/api/tags | python3 -c "
    import json, sys
    tags = json.load(sys.stdin)
    names = [m['name'] for m in tags.get('models', [])]
    sys.exit(0 if any('gpt-oss' in n for n in names) else 1)
    " 2>/dev/null; then
            echo "   Pulling gpt-oss:20b model (this may take a while)..."
            curl -s http://localhost:11434/api/pull -d '{"name":"gpt-oss:20b"}' > /dev/null
        else
            echo "   gpt-oss model already available"
        fi
    else
        echo "   Ollama not running — skipping model check"
    fi

    # -- Build --
    echo ""
    echo "6. Building engine + all scenarios + executor..."
    cargo build -p core-engine --features nomad,executor,executor-vault-secrets
    cargo build -p aithericon-sdk --example research_brief_orchestrator
    cargo build -p aithericon-sdk --example job_net
    cargo build -p aithericon-sdk --example scheduler_net
    cargo build -p aithericon-sdk --example executor_net
    cargo build --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service \
        --features python,file-ops,opendal-s3,llm,vault

    # -- Register Nomad executor template (with VAULT_ADDR for secret unwrapping) --
    echo ""
    echo "7. Registering executor job template..."
    EXECUTOR_BIN="$(cd "$EXECUTOR_REPO" && pwd)/target/debug/aithericon-executor-service"
    jq --arg bin "$EXECUTOR_BIN" \
       '.Job.TaskGroups[0].Tasks[0].Config.command = $bin
        | .Job.TaskGroups[0].Tasks[0].Env.VAULT_ADDR = "http://localhost:8200"
        | .Job.TaskGroups[0].Tasks[0].Env.RUST_LOG = "info"' \
        infra/nomad/executor-job-template.json \
        | curl -sf -X POST http://127.0.0.1:4646/v1/jobs -d @- > /dev/null
    echo "Registered petri-executor-worker job template (with VAULT_ADDR)"

    # -- Start executor --
    echo ""
    echo "8. Starting aithericon-executor..."
    VAULT_ADDR=http://localhost:8200 \
    EXECUTOR_NATS_URL=nats://localhost:4333 \
    EXECUTOR_NAMESPACE=executor_jobs \
    EXECUTOR_NAME=research-brief-executor \
    EXECUTOR_BASE_DIR=/tmp/research-brief-executor \
    EXECUTOR_CONCURRENCY=4 \
    EXECUTOR_DEFAULT_TIMEOUT_SECS=120 \
    EXECUTOR_SOURCE=nats_queue \
    EXECUTOR_LIFETIME=daemon \
    RUST_LOG=info \
    "$EXECUTOR_REPO/target/debug/aithericon-executor-service" > /tmp/research-executor.log 2>&1 &
    EXECUTOR_PID=$!
    disown
    sleep 2
    echo "   Executor started (PID $EXECUTOR_PID)"

    # -- Start Human UI --
    echo ""
    echo "9. Starting Human UI..."
    start_human_ui "$HUMAN_UI_REPO" /tmp/research-human-ui.log || true

    # -- Start engine --
    echo ""
    echo "10. Starting engine (Nomad + executor + Vault + human tasks)..."
    set -a
    source infra/nomad/nomad.env
    source infra/executor/research-brief-demo.env
    set +a
    SCHEDULER_JOB_TEMPLATE=petri-executor-worker \
    RUST_LOG=info,petri_nomad=debug,petri_executor=debug \
    cargo run -p core-engine --features nomad,executor,executor-vault-secrets > /tmp/research-engine.log 2>&1 &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; kill $EXECUTOR_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Nomad + executor watchers"

    # -- Deploy all 4 nets (bottom-up) --
    echo ""
    echo "11. Deploying four-layer nets..."
    echo "   Deploying executor-net (Layer 3)..."
    deploy_scenario executor_net -- --bridged --deploy --net-id executor-net > /dev/null
    echo "   Deploying scheduler relay net (Layer 2)..."
    deploy_scenario scheduler_net -- --deploy --net-id scheduler-net > /dev/null
    echo "   Deploying job-net (Layer 1)..."
    deploy_scenario job_net -- --bridged --upstream orchestrator-net --deploy --net-id job-net > /dev/null
    echo "   Deploying orchestrator-net (Layer 0)..."
    deploy_scenario research_brief_orchestrator -- --deploy --net-id orchestrator-net > /dev/null

    echo ""
    echo "12. Setting all nets to running mode..."
    set_nets_running executor-net scheduler-net job-net orchestrator-net
    # The deploy auto-evaluates the orchestrator seed token (request_entry
    # human_task effect). Do NOT call trigger_eval here — it races with the
    # async effect handler and causes double-firing.
    sleep 2

    # -- Instructions --
    echo ""
    echo "=== Research Brief Generator — Ready ==="
    echo ""
    echo "  The workflow is running. Here's what to do:"
    echo ""
    echo "  1. Open the Human UI: http://localhost:5173"
    echo "     Fill in a research topic, questions, and priority."
    echo ""
    echo "  2. After submission, the system will:"
    echo "     a) Run Python data gathering (with Vault-injected secrets)"
    echo "     b) Transfer files between two local storage backends"
    echo "     c) Generate a structured report via Ollama LLM"
    echo ""
    echo "  3. The generated report will appear in the UI for review."
    echo "     Approve or reject it as a senior analyst."
    echo ""
    echo "  Infrastructure:"
    echo "    Engine:    PID $ENGINE_PID (http://localhost:3030)"
    echo "    Executor:  PID $EXECUTOR_PID"
    echo "    Human UI:  PID $HUMAN_UI_PID (http://localhost:5173)"
    echo "    Nomad UI:  http://localhost:4646/ui"
    echo "    Vault:     http://localhost:8200 (token: demo-root-token)"
    echo "    RustFS:    http://localhost:9001 (rustfsadmin / rustfsadmin)"
    echo ""
    echo "  Logs:"
    echo "    Engine:   tail -f /tmp/research-engine.log"
    echo "    Executor: tail -f /tmp/research-executor.log"
    echo "    Human UI: tail -f /tmp/research-human-ui.log"
    echo ""
    echo "  Stop with: just demo research-brief-stop"

# ============================================
# Invoice Processing Pipeline Demo (Vision OCR)
# ============================================

# Stop the invoice processing demo
invoice-processing-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    stop_on_port 5173 "human-ui"
    stop_on_port 8080 "image-server"
    kill_stale_executor
    just infra nomad-down
    docker rm -f vault-demo 2>/dev/null || true
    docker rm -f petri-rustfs 2>/dev/null || true
    rm -rf /tmp/invoice_processing 2>/dev/null || true
    echo "Invoice processing demo stopped."

# Run the Invoice Processing Pipeline demo (Vision OCR / Kreuzberg + Python + File-ops + LLM + Human + Timer)
invoice-processing:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"
    HUMAN_UI_REPO="../aithericon-human-ui"

    echo "=== Invoice Processing Pipeline Demo ==="
    echo ""
    echo "  Demonstrates 11 capabilities in a single workflow:"
    echo "    1. Vision LLM (OCR)        — invoice image → structured data (GLM-OCR)"
    echo "    2. Kreuzberg extraction     — PDF, Office, text → raw text"
    echo "    3. Text LLM (parsing)      — extracted text → structured JSON"
    echo "    4. Python executor          — validation with Vault secrets"
    echo "    5. Vault secret injection   — API keys for validation"
    echo "    6. File transfer + RustFS   — archive to S3"
    echo "    7. Text LLM (summary)      — approval recommendation"
    echo "    8. Human UI (data entry)    — submit invoice + metadata"
    echo "    9. Human UI (review)        — all block types + e-signature"
    echo "   10. Timer (SLA escalation)   — 5-minute deadline"
    echo "   11. Nomad scheduler          — all jobs via Nomad"
    echo ""
    echo "  Architecture: 4-layer bridged nets"
    echo "    Layer 0: orchestrator-net   (invoice pipeline + human + timer)"
    echo "    Layer 1: job-net            (retry + dead-letter)"
    echo "    Layer 2: scheduler relay net      (Nomad dispatch)"
    echo "    Layer 3: executor-net       (executor lifecycle)"
    echo ""

    # -- Prerequisites --
    echo "1. Checking prerequisites..."
    check_executor_repo "$EXECUTOR_REPO"
    if [ ! -d "$HUMAN_UI_REPO" ]; then
        echo "ERROR: Human UI repo not found at $HUMAN_UI_REPO"
        exit 1
    fi
    if ! command -v deno &> /dev/null; then
        echo "ERROR: deno not found. Install from https://deno.land"
        exit 1
    fi
    if ! curl -sf --max-time 2 http://localhost:11434/api/tags > /dev/null 2>&1; then
        echo "WARNING: Ollama not running at localhost:11434"
        echo "  Start it with: ollama serve"
        echo "  The Vision OCR and LLM steps will fail without Ollama."
    fi
    echo "   Prerequisites OK"

    # -- Clean up --
    echo ""
    echo "2. Cleaning up stale processes..."
    kill_stale_engine
    kill_stale_executor
    lsof -ti :5173 | xargs kill 2>/dev/null || true
    lsof -ti :8080 | xargs kill 2>/dev/null || true

    # -- NATS + Nomad + RustFS --
    echo ""
    echo "3. Starting NATS + Nomad + RustFS..."
    just infra nats-up
    just infra nomad-up
    just infra rustfs-up
    echo "Creating 'invoices' bucket..."
    AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
        aws --endpoint-url http://localhost:9005 --region us-east-1 \
        s3 mb s3://invoices 2>/dev/null || true
    echo "Setting public-read policy on 'invoices' bucket..."
    AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
        aws --endpoint-url http://localhost:9005 --region us-east-1 \
        s3api put-bucket-policy --bucket invoices \
        --policy '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["*"]},"Action":["s3:GetObject"],"Resource":["arn:aws:s3:::invoices/*"]}]}'
    echo "RustFS bucket 'invoices' ready (public-read)"
    echo "Setting public-read policy on 'human-tasks' bucket..."
    AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
        aws --endpoint-url http://localhost:9005 --region us-east-1 \
        s3api put-bucket-policy --bucket human-tasks \
        --policy '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["*"]},"Action":["s3:GetObject"],"Resource":["arn:aws:s3:::human-tasks/*"]}]}' 2>/dev/null || true
    echo "RustFS bucket 'human-tasks' ready (public-read)"
    purge_nomad_jobs
    clean_nats_streams
    # Clean stale executor + human task streams
    for STREAM in \
        executor_jobs_high executor_jobs_medium executor_jobs_low executor_jobs_dlq \
        EXECUTOR_STATUS EXECUTOR_EVENTS; do
        nats -s nats://localhost:4333 stream delete "$STREAM" -f 2>/dev/null || true
    done
    nats -s nats://localhost:4333 consumer del HUMAN_REQUESTS human-ui -f 2>/dev/null || true
    nats -s nats://localhost:4333 stream purge HUMAN_REQUESTS -f 2>/dev/null || true
    nats -s nats://localhost:4333 consumer del HUMAN_COMPLETED human-ui-completed -f 2>/dev/null || true
    nats -s nats://localhost:4333 stream purge HUMAN_COMPLETED -f 2>/dev/null || true

    # -- Vault --
    echo ""
    echo "4. Starting Vault dev server..."
    docker rm -f vault-demo 2>/dev/null || true
    docker run -d --name vault-demo \
        -p 8200:8200 \
        -e VAULT_DEV_ROOT_TOKEN_ID=demo-root-token \
        -e VAULT_DEV_LISTEN_ADDRESS=0.0.0.0:8200 \
        hashicorp/vault:1.15 > /dev/null
    for i in $(seq 1 30); do
        if curl -sf http://localhost:8200/v1/sys/health > /dev/null 2>&1; then
            echo "   Vault is ready"
            break
        fi
        if [ "$i" -eq 30 ]; then echo "ERROR: Vault did not start"; exit 1; fi
        sleep 0.5
    done

    echo "   Seeding secrets into Vault..."
    curl -s -X POST http://localhost:8200/v1/secret/data/demo/validation \
        -H "X-Vault-Token: demo-root-token" \
        -H "Content-Type: application/json" \
        -d '{"data": {"api_key": "sk-live-invoice-validation-key"}}' > /dev/null
    echo "   Stored: demo/validation#api_key"
    curl -s -X POST http://localhost:8200/v1/secret/data/demo/rustfs \
        -H "X-Vault-Token: demo-root-token" \
        -H "Content-Type: application/json" \
        -d '{"data": {"access_key": "rustfsadmin", "secret_key": "rustfsadmin"}}' > /dev/null
    echo "   Stored: demo/rustfs#access_key, demo/rustfs#secret_key"

    # -- Pull Ollama models --
    echo ""
    echo "5. Checking Ollama models..."
    if curl -sf --max-time 2 http://localhost:11434/api/tags > /dev/null 2>&1; then
        # Vision model (for OCR — GLM-OCR is small, fast, and trained for document extraction)
        if ! curl -s http://localhost:11434/api/tags | python3 -c "
    import json, sys
    tags = json.load(sys.stdin)
    names = [m['name'] for m in tags.get('models', [])]
    sys.exit(0 if any('glm-ocr' in n for n in names) else 1)
    " 2>/dev/null; then
            echo "   Pulling glm-ocr:q8_0 model (this may take a while)..."
            curl -s http://localhost:11434/api/pull -d '{"name":"glm-ocr:q8_0"}' > /dev/null
        else
            echo "   glm-ocr model already available"
        fi
        # Text model (for summary)
        if ! curl -s http://localhost:11434/api/tags | python3 -c "
    import json, sys
    tags = json.load(sys.stdin)
    names = [m['name'] for m in tags.get('models', [])]
    sys.exit(0 if any('gpt-oss' in n for n in names) else 1)
    " 2>/dev/null; then
            echo "   Pulling gpt-oss:20b model (this may take a while)..."
            curl -s http://localhost:11434/api/pull -d '{"name":"gpt-oss:20b"}' > /dev/null
        else
            echo "   gpt-oss model already available"
        fi
    else
        echo "   Ollama not running — skipping model check"
    fi

    # -- Pre-stage invoice image --
    echo ""
    echo "6. Generating sample invoice image..."
    mkdir -p /tmp/invoice_processing/validated
    if command -v magick &> /dev/null; then
        CONVERT_CMD="magick"
    elif command -v convert &> /dev/null; then
        CONVERT_CMD="convert"
    else
        CONVERT_CMD=""
    fi

    if [ -n "$CONVERT_CMD" ]; then
        $CONVERT_CMD -size 800x1100 xc:white \
            -font Courier -pointsize 28 -fill '#222222' \
            -gravity NorthWest -annotate +50+40 "INVOICE" \
            -pointsize 14 -fill '#555555' \
            -annotate +50+80 "Acme Corporation" \
            -annotate +50+98 "123 Business Ave, Suite 400" \
            -annotate +50+116 "San Francisco, CA 94105" \
            -pointsize 14 -fill '#222222' \
            -annotate +500+80 "Invoice #: INV-2025-0042" \
            -annotate +500+98 "Date: 2025-01-15" \
            -annotate +500+116 "Due: 2025-02-14" \
            -pointsize 16 -fill '#222222' \
            -annotate +50+170 "Bill To:" \
            -pointsize 14 -fill '#555555' \
            -annotate +50+190 "TechStart Inc." \
            -annotate +50+208 "456 Innovation Drive" \
            -annotate +50+226 "Austin, TX 78701" \
            -fill '#cccccc' -draw "line 50,260 750,260" \
            -pointsize 12 -fill '#222222' \
            -annotate +50+280 "Description" \
            -annotate +420+280 "Qty" \
            -annotate +500+280 "Unit Price" \
            -annotate +650+280 "Amount" \
            -fill '#cccccc' -draw "line 50,295 750,295" \
            -pointsize 12 -fill '#333333' \
            -annotate +50+315 "Cloud Infrastructure Setup" \
            -annotate +420+315 "1" \
            -annotate +500+315 "4,500.00" \
            -annotate +650+315 "4,500.00" \
            -annotate +50+340 'Software License - Annual' \
            -annotate +420+340 "5" \
            -annotate +500+340 "1,200.00" \
            -annotate +650+340 "6,000.00" \
            -annotate +50+365 'Technical Consulting - hrs' \
            -annotate +420+365 "20" \
            -annotate +500+365 "150.00" \
            -annotate +650+365 "3,000.00" \
            -annotate +50+390 "Data Migration Service" \
            -annotate +420+390 "1" \
            -annotate +500+390 "2,500.00" \
            -annotate +650+390 "2,500.00" \
            -fill '#cccccc' -draw "line 50,415 750,415" \
            -pointsize 12 -fill '#333333' \
            -annotate +500+435 "Subtotal:" \
            -annotate +650+435 "16,000.00" \
            -annotate +500+455 'Tax 8.25%:' \
            -annotate +650+455 "1,320.00" \
            -fill '#cccccc' -draw "line 500,470 750,470" \
            -pointsize 14 -fill '#222222' \
            -annotate +500+490 "TOTAL:" \
            -annotate +650+490 "17,320.00" \
            -pointsize 11 -fill '#555555' \
            -annotate +50+540 "Payment Terms: Net 30" \
            -annotate +50+558 "Please remit to: Acme Corp, Acct 1234567890" \
            /tmp/invoice_processing/sample_invoice.png
        echo "   Generated sample invoice PNG with ImageMagick"
    else
        echo "   ERROR: ImageMagick not found. Install with: brew install imagemagick"
        echo "   The Vision OCR step requires a real invoice image."
        exit 1
    fi

    # -- Build --
    echo ""
    echo "7. Building engine + all scenarios + executor..."
    cargo build -p core-engine --features nomad,executor,executor-vault-secrets
    cargo build -p aithericon-sdk --example invoice_processing_orchestrator
    cargo build -p aithericon-sdk --example job_net
    cargo build -p aithericon-sdk --example scheduler_net
    cargo build -p aithericon-sdk --example executor_net
    cargo build --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service \
        --features python,file-ops,opendal-s3,llm,vault,kreuzberg

    # -- Register Nomad executor template (with VAULT_ADDR for secret unwrapping) --
    echo ""
    echo "8. Registering executor job template..."
    EXECUTOR_BIN="$(cd "$EXECUTOR_REPO" && pwd)/target/debug/aithericon-executor-service"
    jq --arg bin "$EXECUTOR_BIN" \
       '.Job.TaskGroups[0].Tasks[0].Config.command = $bin
        | .Job.TaskGroups[0].Tasks[0].Env.VAULT_ADDR = "http://localhost:8200"
        | .Job.TaskGroups[0].Tasks[0].Env.RUST_LOG = "info"' \
        infra/nomad/executor-job-template.json \
        | curl -sf -X POST http://127.0.0.1:4646/v1/jobs -d @- > /dev/null
    echo "   Registered petri-executor-worker job template (with VAULT_ADDR)"

    # -- Start executor --
    echo ""
    echo "9. Starting aithericon-executor..."
    VAULT_ADDR=http://localhost:8200 \
    EXECUTOR_NATS_URL=nats://localhost:4333 \
    EXECUTOR_NAMESPACE=executor_jobs \
    EXECUTOR_NAME=invoice-processing-executor \
    EXECUTOR_BASE_DIR=/tmp/invoice-processing-executor \
    EXECUTOR_CONCURRENCY=4 \
    EXECUTOR_DEFAULT_TIMEOUT_SECS=120 \
    EXECUTOR_SOURCE=nats_queue \
    EXECUTOR_LIFETIME=daemon \
    RUST_LOG=info \
    "$EXECUTOR_REPO/target/debug/aithericon-executor-service" > /tmp/invoice-executor.log 2>&1 &
    EXECUTOR_PID=$!
    disown
    sleep 2
    echo "   Executor started (PID $EXECUTOR_PID)"

    # -- Start image server --
    echo ""
    echo "10. Starting image server for Human UI..."
    python3 -m http.server 8080 --directory /tmp/invoice_processing > /tmp/invoice-image-server.log 2>&1 &
    IMAGE_SERVER_PID=$!
    disown
    echo "   Image server started on port 8080 (PID $IMAGE_SERVER_PID)"

    # -- Start Human UI --
    echo ""
    echo "11. Starting Human UI (with S3 credentials for file upload)..."
    start_human_ui "$HUMAN_UI_REPO" /tmp/invoice-human-ui.log S3_ACCESS_KEY=rustfsadmin S3_SECRET_KEY=rustfsadmin || true

    # -- Start engine --
    echo ""
    echo "12. Starting engine (Nomad + executor + Vault + human tasks + timer)..."
    set -a
    source infra/nomad/nomad.env
    source infra/executor/invoice-processing-demo.env
    set +a
    SCHEDULER_JOB_TEMPLATE=petri-executor-worker \
    RUST_LOG=info,petri_nomad=debug,petri_executor=debug \
    cargo run -p core-engine --features nomad,executor,executor-vault-secrets > /tmp/invoice-engine.log 2>&1 &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; kill $EXECUTOR_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "Nomad + executor watchers"

    # -- Deploy all 4 nets (bottom-up) --
    echo ""
    echo "13. Deploying four-layer nets..."
    echo "   Deploying executor-net (Layer 3)..."
    deploy_scenario executor_net -- --bridged --deploy --net-id executor-net > /dev/null
    echo "   Deploying scheduler relay net (Layer 2)..."
    deploy_scenario scheduler_net -- --deploy --net-id scheduler-net > /dev/null
    echo "   Deploying job-net (Layer 1)..."
    deploy_scenario job_net -- --bridged --upstream orchestrator-net --deploy --net-id job-net > /dev/null
    echo "   Deploying orchestrator-net (Layer 0)..."
    deploy_scenario invoice_processing_orchestrator -- --deploy --net-id orchestrator-net > /dev/null

    echo ""
    echo "14. Setting all nets to running mode..."
    set_nets_running executor-net scheduler-net job-net orchestrator-net
    # Deploy auto-evaluates the orchestrator seed token (request_entry human_task effect).
    # Do NOT call trigger_eval here — it races with the async effect handler.
    sleep 2

    # -- Instructions --
    echo ""
    echo "=== Invoice Processing Pipeline — Ready ==="
    echo ""
    echo "  The workflow is running. Here's what to do:"
    echo ""
    echo "  1. Open the Human UI: http://localhost:5173"
    echo "     You'll see the invoice image. Select a department, urgency, and submit."
    echo ""
    echo "  2. After submission, the system will:"
    echo "     a) Extract data: Vision OCR for images, Kreuzberg + Text LLM for PDFs/docs"
    echo "     b) Validate extracted data via Python (with Vault secrets)"
    echo "     c) Archive validated data to S3 via file-ops"
    echo "     d) Generate an approval recommendation via Text LLM"
    echo ""
    echo "  3. The review form will appear with all 7 block types:"
    echo "     image, markdown, table, callout, download, divider, inputs"
    echo "     Approve or reject within 5 minutes or it auto-escalates."
    echo ""
    echo "  Infrastructure:"
    echo "    Engine:       PID $ENGINE_PID (http://localhost:3030)"
    echo "    Executor:     PID $EXECUTOR_PID"
    echo "    Human UI:     PID $HUMAN_UI_PID (http://localhost:5173)"
    echo "    Image Server: PID $IMAGE_SERVER_PID (http://localhost:8080)"
    echo "    Nomad UI:     http://localhost:4646/ui"
    echo "    Vault:        http://localhost:8200 (token: demo-root-token)"
    echo "    RustFS:       http://localhost:9001 (rustfsadmin / rustfsadmin)"
    echo ""
    echo "  Logs:"
    echo "    Engine:       tail -f /tmp/invoice-engine.log"
    echo "    Executor:     tail -f /tmp/invoice-executor.log"
    echo "    Human UI:     tail -f /tmp/invoice-human-ui.log"
    echo "    Image Server: tail -f /tmp/invoice-image-server.log"
    echo ""
    echo "  Stop with: just demo invoice-processing-stop"

# ============================================
# Invoice Processing (Spawn) — dynamic child nets
# ============================================

# Stop the invoice processing spawn demo
invoice-processing-spawn-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    stop_on_port 5173 "human-ui"
    stop_on_port 8080 "image-server"
    kill_stale_executor
    docker rm -f vault-demo 2>/dev/null || true
    docker rm -f petri-rustfs 2>/dev/null || true
    rm -rf /tmp/invoice_processing 2>/dev/null || true
    echo "Invoice processing spawn demo stopped."

# Run the Invoice Processing Pipeline demo with dynamic child net spawning (no Nomad)
invoice-processing-spawn:
    #!/usr/bin/env bash
    source {{_lib}}

    EXECUTOR_REPO="{{executor_repo}}"
    HUMAN_UI_REPO="../aithericon-human-ui"

    echo "=== Invoice Processing Pipeline (Spawn) ==="
    echo ""
    echo "  Same 11 capabilities as invoice-processing, but uses ctx.spawn()"
    echo "  for dynamic child nets instead of static 4-layer bridges."
    echo ""
    echo "  No Nomad required — each execution step spawns its own child net."
    echo ""

    # -- Prerequisites --
    echo "1. Checking prerequisites..."
    check_executor_repo "$EXECUTOR_REPO"
    if [ ! -d "$HUMAN_UI_REPO" ]; then
        echo "ERROR: Human UI repo not found at $HUMAN_UI_REPO"
        exit 1
    fi
    if ! command -v deno &> /dev/null; then
        echo "ERROR: deno not found. Install from https://deno.land"
        exit 1
    fi
    if ! curl -sf --max-time 2 http://localhost:11434/api/tags > /dev/null 2>&1; then
        echo "WARNING: Ollama not running at localhost:11434"
        echo "  Start it with: ollama serve"
        echo "  The Vision OCR and LLM steps will fail without Ollama."
    fi
    echo "   Prerequisites OK"

    # -- Clean up --
    echo ""
    echo "2. Cleaning up stale processes..."
    kill_stale_engine
    kill_stale_executor
    lsof -ti :5173 | xargs kill 2>/dev/null || true
    lsof -ti :8080 | xargs kill 2>/dev/null || true

    # -- NATS + RustFS (no Nomad) --
    echo ""
    echo "3. Starting NATS + RustFS..."
    just infra nats-up
    just infra rustfs-up
    echo "Creating 'invoices' bucket..."
    AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
        aws --endpoint-url http://localhost:9005 --region us-east-1 \
        s3 mb s3://invoices 2>/dev/null || true
    echo "Setting public-read policy on 'invoices' bucket..."
    AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
        aws --endpoint-url http://localhost:9005 --region us-east-1 \
        s3api put-bucket-policy --bucket invoices \
        --policy '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["*"]},"Action":["s3:GetObject"],"Resource":["arn:aws:s3:::invoices/*"]}]}'
    echo "RustFS bucket 'invoices' ready (public-read)"
    echo "Setting public-read policy on 'human-tasks' bucket..."
    AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
        aws --endpoint-url http://localhost:9005 --region us-east-1 \
        s3api put-bucket-policy --bucket human-tasks \
        --policy '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["*"]},"Action":["s3:GetObject"],"Resource":["arn:aws:s3:::human-tasks/*"]}]}' 2>/dev/null || true
    echo "RustFS bucket 'human-tasks' ready (public-read)"
    clean_nats_streams
    # Clean stale executor + human task streams
    for STREAM in \
        executor_jobs_high executor_jobs_medium executor_jobs_low executor_jobs_dlq \
        EXECUTOR_STATUS EXECUTOR_EVENTS; do
        nats -s nats://localhost:4333 stream delete "$STREAM" -f 2>/dev/null || true
    done
    nats -s nats://localhost:4333 consumer del HUMAN_REQUESTS human-ui -f 2>/dev/null || true
    nats -s nats://localhost:4333 stream purge HUMAN_REQUESTS -f 2>/dev/null || true
    nats -s nats://localhost:4333 consumer del HUMAN_COMPLETED human-ui-completed -f 2>/dev/null || true
    nats -s nats://localhost:4333 stream purge HUMAN_COMPLETED -f 2>/dev/null || true

    # -- Vault --
    echo ""
    echo "4. Starting Vault dev server..."
    docker rm -f vault-demo 2>/dev/null || true
    docker run -d --name vault-demo \
        -p 8200:8200 \
        -e VAULT_DEV_ROOT_TOKEN_ID=demo-root-token \
        -e VAULT_DEV_LISTEN_ADDRESS=0.0.0.0:8200 \
        hashicorp/vault:1.15 > /dev/null
    for i in $(seq 1 30); do
        if curl -sf http://localhost:8200/v1/sys/health > /dev/null 2>&1; then
            echo "   Vault is ready"
            break
        fi
        if [ "$i" -eq 30 ]; then echo "ERROR: Vault did not start"; exit 1; fi
        sleep 0.5
    done

    echo "   Seeding secrets into Vault..."
    curl -s -X POST http://localhost:8200/v1/secret/data/demo/validation \
        -H "X-Vault-Token: demo-root-token" \
        -H "Content-Type: application/json" \
        -d '{"data": {"api_key": "sk-live-invoice-validation-key"}}' > /dev/null
    echo "   Stored: demo/validation#api_key"
    curl -s -X POST http://localhost:8200/v1/secret/data/demo/rustfs \
        -H "X-Vault-Token: demo-root-token" \
        -H "Content-Type: application/json" \
        -d '{"data": {"access_key": "rustfsadmin", "secret_key": "rustfsadmin"}}' > /dev/null
    echo "   Stored: demo/rustfs#access_key, demo/rustfs#secret_key"

    # -- Pull Ollama models --
    echo ""
    echo "5. Checking Ollama models..."
    if curl -sf --max-time 2 http://localhost:11434/api/tags > /dev/null 2>&1; then
        # Vision model (for OCR — GLM-OCR is small, fast, and trained for document extraction)
        if ! curl -s http://localhost:11434/api/tags | python3 -c "
    import json, sys
    tags = json.load(sys.stdin)
    names = [m['name'] for m in tags.get('models', [])]
    sys.exit(0 if any('glm-ocr' in n for n in names) else 1)
    " 2>/dev/null; then
            echo "   Pulling glm-ocr:q8_0 model (this may take a while)..."
            curl -s http://localhost:11434/api/pull -d '{"name":"glm-ocr:q8_0"}' > /dev/null
        else
            echo "   glm-ocr model already available"
        fi
        # Text model (for summary)
        if ! curl -s http://localhost:11434/api/tags | python3 -c "
    import json, sys
    tags = json.load(sys.stdin)
    names = [m['name'] for m in tags.get('models', [])]
    sys.exit(0 if any('gpt-oss' in n for n in names) else 1)
    " 2>/dev/null; then
            echo "   Pulling gpt-oss:20b model (this may take a while)..."
            curl -s http://localhost:11434/api/pull -d '{"name":"gpt-oss:20b"}' > /dev/null
        else
            echo "   gpt-oss model already available"
        fi
    else
        echo "   Ollama not running — skipping model check"
    fi

    # -- Pre-stage invoice image --
    echo ""
    echo "6. Generating sample invoice image..."
    mkdir -p /tmp/invoice_processing/validated
    if command -v magick &> /dev/null; then
        CONVERT_CMD="magick"
    elif command -v convert &> /dev/null; then
        CONVERT_CMD="convert"
    else
        CONVERT_CMD=""
    fi

    if [ -n "$CONVERT_CMD" ]; then
        $CONVERT_CMD -size 800x1100 xc:white \
            -font Courier -pointsize 28 -fill '#222222' \
            -gravity NorthWest -annotate +50+40 "INVOICE" \
            -pointsize 14 -fill '#555555' \
            -annotate +50+80 "Acme Corporation" \
            -annotate +50+98 "123 Business Ave, Suite 400" \
            -annotate +50+116 "San Francisco, CA 94105" \
            -pointsize 14 -fill '#222222' \
            -annotate +500+80 "Invoice #: INV-2025-0042" \
            -annotate +500+98 "Date: 2025-01-15" \
            -annotate +500+116 "Due: 2025-02-14" \
            -pointsize 16 -fill '#222222' \
            -annotate +50+170 "Bill To:" \
            -pointsize 14 -fill '#555555' \
            -annotate +50+190 "TechStart Inc." \
            -annotate +50+208 "456 Innovation Drive" \
            -annotate +50+226 "Austin, TX 78701" \
            -fill '#cccccc' -draw "line 50,260 750,260" \
            -pointsize 12 -fill '#222222' \
            -annotate +50+280 "Description" \
            -annotate +420+280 "Qty" \
            -annotate +500+280 "Unit Price" \
            -annotate +650+280 "Amount" \
            -fill '#cccccc' -draw "line 50,295 750,295" \
            -pointsize 12 -fill '#333333' \
            -annotate +50+315 "Cloud Infrastructure Setup" \
            -annotate +420+315 "1" \
            -annotate +500+315 "4,500.00" \
            -annotate +650+315 "4,500.00" \
            -annotate +50+340 'Software License - Annual' \
            -annotate +420+340 "5" \
            -annotate +500+340 "1,200.00" \
            -annotate +650+340 "6,000.00" \
            -annotate +50+365 'Technical Consulting - hrs' \
            -annotate +420+365 "20" \
            -annotate +500+365 "150.00" \
            -annotate +650+365 "3,000.00" \
            -annotate +50+390 "Data Migration Service" \
            -annotate +420+390 "1" \
            -annotate +500+390 "2,500.00" \
            -annotate +650+390 "2,500.00" \
            -fill '#cccccc' -draw "line 50,415 750,415" \
            -pointsize 12 -fill '#333333' \
            -annotate +500+435 "Subtotal:" \
            -annotate +650+435 "16,000.00" \
            -annotate +500+455 'Tax 8.25%:' \
            -annotate +650+455 "1,320.00" \
            -fill '#cccccc' -draw "line 500,470 750,470" \
            -pointsize 14 -fill '#222222' \
            -annotate +500+490 "TOTAL:" \
            -annotate +650+490 "17,320.00" \
            -pointsize 11 -fill '#555555' \
            -annotate +50+540 "Payment Terms: Net 30" \
            -annotate +50+558 "Please remit to: Acme Corp, Acct 1234567890" \
            /tmp/invoice_processing/sample_invoice.png
        echo "   Generated sample invoice PNG with ImageMagick"
    else
        echo "   ERROR: ImageMagick not found. Install with: brew install imagemagick"
        echo "   The Vision OCR step requires a real invoice image."
        exit 1
    fi

    # -- Build (no Nomad — spawn handles child nets) --
    echo ""
    echo "7. Building engine + scenario + executor..."
    cargo build -p core-engine --features executor,executor-vault-secrets
    cargo build -p aithericon-sdk --example invoice_processing_spawn_demo
    cargo build --manifest-path "$EXECUTOR_REPO/Cargo.toml" -p aithericon-executor-service \
        --features python,file-ops,opendal-s3,llm,vault,kreuzberg

    # -- Start executor --
    echo ""
    echo "8. Starting aithericon-executor..."
    VAULT_ADDR=http://localhost:8200 \
    EXECUTOR_NATS_URL=nats://localhost:4333 \
    EXECUTOR_NAMESPACE=executor_jobs \
    EXECUTOR_NAME=invoice-processing-executor \
    EXECUTOR_BASE_DIR=/tmp/invoice-processing-executor \
    EXECUTOR_CONCURRENCY=4 \
    EXECUTOR_DEFAULT_TIMEOUT_SECS=120 \
    EXECUTOR_SOURCE=nats_queue \
    EXECUTOR_LIFETIME=daemon \
    RUST_LOG=info \
    "$EXECUTOR_REPO/target/debug/aithericon-executor-service" > /tmp/invoice-executor.log 2>&1 &
    EXECUTOR_PID=$!
    disown
    sleep 2
    echo "   Executor started (PID $EXECUTOR_PID)"

    # -- Start image server --
    echo ""
    echo "9. Starting image server for Human UI..."
    python3 -m http.server 8080 --directory /tmp/invoice_processing > /tmp/invoice-image-server.log 2>&1 &
    IMAGE_SERVER_PID=$!
    disown
    echo "   Image server started on port 8080 (PID $IMAGE_SERVER_PID)"

    # -- Start Human UI --
    echo ""
    echo "10. Starting Human UI (with S3 credentials for file upload)..."
    start_human_ui "$HUMAN_UI_REPO" /tmp/invoice-human-ui.log S3_ACCESS_KEY=rustfsadmin S3_SECRET_KEY=rustfsadmin || true

    # -- Start engine (no Nomad features) --
    echo ""
    echo "11. Starting engine (executor + Vault + human tasks + timer)..."
    NATS_URL=nats://localhost:4333 \
    EXECUTOR_ENABLED=true \
    EXECUTOR_SIGNAL_ROUTES=accepted:sig_accepted,running:sig_running,completed:sig_completed,failed:sig_failed,cancelled:sig_cancelled,timed_out:sig_timed_out \
    EXECUTOR_EVENT_ROUTES=progress:sig_progress,artifact:sig_artifact \
    EXECUTOR_NAMESPACE=executor_jobs \
    VAULT_ADDR=http://localhost:8200 \
    VAULT_TOKEN=demo-root-token \
    CLOCKMASTER_ENABLED=true \
    RUST_LOG=info,petri_executor=debug \
    cargo run -p core-engine --features executor,executor-vault-secrets > /tmp/invoice-spawn-engine.log 2>&1 &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; kill $EXECUTOR_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter
    wait_for_watchers "executor watchers"

    # -- Deploy single orchestrator net (spawn handles child nets dynamically) --
    echo ""
    echo "12. Deploying orchestrator-net (single net — spawn creates child nets at runtime)..."
    deploy_scenario invoice_processing_spawn_demo -- --deploy --net-id orchestrator-net > /dev/null

    echo ""
    echo "13. Setting orchestrator to running mode..."
    set_nets_running orchestrator-net
    sleep 2

    # -- Instructions --
    echo ""
    echo "=== Invoice Processing Pipeline (Spawn) — Ready ==="
    echo ""
    echo "  The workflow is running. Here's what to do:"
    echo ""
    echo "  1. Open the Human UI: http://localhost:5173"
    echo "     You'll see the invoice image. Select a department, urgency, and submit."
    echo ""
    echo "  2. After submission, the system will:"
    echo "     a) Spawn child nets for each execution step (OCR/Kreuzberg, Parse, Validate, Summary)"
    echo "     b) Each child net encapsulates the full executor lifecycle"
    echo "     c) Results bridge back to the parent orchestrator automatically"
    echo ""
    echo "  3. The review form will appear with all block types."
    echo "     Approve or reject within 5 minutes or it auto-escalates."
    echo ""
    echo "  Infrastructure:"
    echo "    Engine:       PID $ENGINE_PID (http://localhost:3030)"
    echo "    Executor:     PID $EXECUTOR_PID"
    echo "    Human UI:     PID $HUMAN_UI_PID (http://localhost:5173)"
    echo "    Image Server: PID $IMAGE_SERVER_PID (http://localhost:8080)"
    echo "    Vault:        http://localhost:8200 (token: demo-root-token)"
    echo "    RustFS:       http://localhost:9001 (rustfsadmin / rustfsadmin)"
    echo ""
    echo "  Logs:"
    echo "    Engine:       tail -f /tmp/invoice-spawn-engine.log"
    echo "    Executor:     tail -f /tmp/invoice-executor.log"
    echo "    Human UI:     tail -f /tmp/invoice-human-ui.log"
    echo "    Image Server: tail -f /tmp/invoice-image-server.log"
    echo ""
    echo "  Stop with: just demo invoice-processing-spawn-stop"

# ============================================
# Spawn Demo (minimal ctx.spawn() end-to-end)
# ============================================

# Stop the spawn demo
spawn-stop:
    #!/usr/bin/env bash
    source {{_lib}}
    stop_on_port 3030 "engine"
    kill_stale_trace_exporter
    echo "Spawn demo stopped."

# Run the minimal spawn demo end-to-end
spawn:
    #!/usr/bin/env bash
    source {{_lib}}

    echo "=== Spawn Demo (ctx.spawn() end-to-end) ==="
    echo ""
    echo "  Parent orchestrator spawns a child net that doubles a number."
    echo "  Child bridges the result back. Parent reaches terminal."
    echo ""

    kill_stale_engine

    echo "1. Starting NATS..."
    just infra nats-up

    echo ""
    echo "2. Building engine + spawn_demo scenario..."
    cargo build -p core-engine
    cargo build -p aithericon-sdk --example spawn_demo

    echo ""
    echo "3. Starting engine..."
    RUST_LOG=info \
    cargo run -p core-engine > /tmp/spawn-demo-engine.log 2>&1 &
    ENGINE_PID=$!
    trap 'kill $ENGINE_PID 2>/dev/null; wait $ENGINE_PID 2>/dev/null' ERR

    wait_for_engine_api
    start_trace_exporter

    echo ""
    echo "4. Deploying spawn-demo scenario..."
    deploy_scenario spawn_demo -- --deploy --net-id spawn-demo > /dev/null

    echo ""
    echo "5. Setting parent to running mode + triggering eval..."
    set_nets_running spawn-demo
    trigger_eval spawn-demo

    # The spawn effect creates a child net. Wait for it, then set it running too.
    echo ""
    echo "6. Waiting for child net to be created..."
    CHILD_NET_ID=""
    for i in $(seq 1 15); do
        sleep 1
        CHILD_NET_ID=$(curl -s http://localhost:3030/api/nets/spawn-demo/state | python3 -c "
    import json, sys
    s = json.load(sys.stdin)
    tokens = s.get('marking',{}).get('tokens',{}).get('worker_spawned',[])
    if tokens:
        print(tokens[0].get('color',{}).get('value',{}).get('child_net_id',''))
    " 2>/dev/null || true)
        if [ -n "$CHILD_NET_ID" ]; then
            echo "   Child net created: $CHILD_NET_ID"
            break
        fi
        echo "   [$i] waiting..."
    done

    if [ -z "$CHILD_NET_ID" ]; then
        echo "ERROR: Child net was never created"
        exit 1
    fi

    echo ""
    echo "7. Setting child net to running + triggering eval..."
    set_nets_running "$CHILD_NET_ID"
    trigger_eval "$CHILD_NET_ID"

    echo ""
    echo "8. Waiting for result to bridge back to parent..."
    poll_single_place "completed_terminal" 1 15 2 spawn-demo

    echo ""
    echo "9. Final state:"
    curl -s "http://localhost:3030/api/nets/spawn-demo/state" | python3 -c "
    import json, sys
    s = json.load(sys.stdin)
    tokens = s.get('marking',{}).get('tokens',{})
    for pid, tlist in tokens.items():
        if tlist:
            for t in tlist:
                v = t.get('color',{}).get('value',{})
                if v:
                    print(f'   {pid}: {json.dumps(v)}')
    "

    echo ""
    echo "   Child net ($CHILD_NET_ID):"
    curl -s "http://localhost:3030/api/nets/$CHILD_NET_ID/state" | python3 -c "
    import json, sys
    s = json.load(sys.stdin)
    tokens = s.get('marking',{}).get('tokens',{})
    count = sum(len(v) for v in tokens.values())
    print(f'     Marking: {count} token(s) remaining')
    " 2>/dev/null || echo "     (not found)"

    echo ""
    print_demo_footer "Spawn Demo" "$ENGINE_PID" "spawn-stop"
