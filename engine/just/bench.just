# petri-bench — L1 micro-benchmark sweeps
# ========================================
#
# These are Layer-1 (L1) in-process benchmarks: they run fully in-process via
# petri-simulator + project_marking, with NO infra (no NATS, no Docker, no
# engine HTTP server). Just `cargo run` — nothing to start first.
#
# Each recipe runs a parameter sweep along one scaling axis, emits one JSON
# artifact per measured point into engine/bench/results/, and prints a table.
# CWD here is engine/just; cargo resolves the engine workspace upward, so the
# build target lands in engine/target.
#
#   just bench replay [max] [samples]
#   just bench eval [shape] [max] [samples]   (shape = chain | branches | fanin)
#   just bench selection [max] [samples]
#   just bench match [arity] [max] [samples]  (max = tokens-per-place; combos = max^arity)
#   just bench results                        (list emitted JSON artifacts)

# Rehydration / replay axis: project a synthetic event log into a marking.
replay max="30000" samples="7":
    cargo run --release -p petri-bench -- replay --max-events {{max}} --samples {{samples}}

# Single-net evaluation axis: drive a generated net to quiescence.
eval shape="chain" max="1000" samples="7":
    cargo run --release -p petri-bench -- eval --shape {{shape}} --max-size {{max}} --samples {{samples}}

# Transition-selection axis: evaluate a net with many enabled transitions.
selection max="1000" samples="7":
    cargo run --release -p petri-bench -- selection --max-transitions {{max}} --samples {{samples}}

# Binding-search axis: worst-case m^arity token-combination scan (multi-input
# transition + never-matching correlating guard). `max` = tokens-per-place.
match arity="2" max="100" samples="7":
    cargo run --release -p petri-bench -- match --arity {{arity}} --max-tokens {{max}} --samples {{samples}}

# List the JSON result artifacts emitted so far.
results:
    @ls -1 /Users/milanender/AithericonResearch/aithericon-platform/.claude/worktrees/bench-harness/engine/bench/results/*.json 2>/dev/null || echo "no results yet"
