# BO demo — live-e2e status & the Map/AutomatedStep-body blocker

**Status (2026-05-29, overnight run):** offline-verified + committed; live e2e **blocked**
by a genuine Map-node compiler limitation, root-caused precisely below.

## What works (proven live)

- The demo compiles + borrow-checks offline (3 `demos.rs` tests green; full path:
  Map scatter/gather lowering + Loop accumulator lowering + read-arc synthesis).
- Live: the engine (rebuilt to current `main`, with the scatter/gather primitives)
  **scatters correctly** — firing the `bo-converges` test, the Map dispatched exactly
  K=4 `branin` jobs, each with correct per-element data
  (`input.json` = `{__map_id:"mp", __map_idx:0..3, cand:{a,d}}`). The compiler/engine
  scatter path is correct.

## The blocker — `collect` cannot read a token-stripping AutomatedStep body

Engine marking after the run (the smoking gun):

```
p_mp_count   : 1 token  {__map_id:"mp", expected:4}        # coordinator OK
p_branin_data: 4 tokens                                    # branin obs PARKED here
p_mp_results : 4 tokens  {__map_id:null, __map_idx:null, value:null}   # ALL NULL
p_mp_body_out: 0 tokens
```

All 4 branin ran and `collect` fired 4 times — but every result token is **all-null**.

`lower_map`'s `t_<id>_collect` does:

```
result = #{ value: body.<resultVar>, __map_idx: body.__map_idx, __map_id: body.__map_id }
```

reading those three fields off the **body control token** (`p_mp_body_out`). For an
**AutomatedStep body** (the common case), none of them are there:

1. `body.<resultVar>` (`body.obs`) → **null**. AutomatedStep output is **parked
   write-once** in `p_branin_data` (control-data model); only a slim control token
   flows to `body_out`. The business output never rides the token.
2. `body.__map_idx` / `body.__map_id` → **null**. The scatter stamps these on the
   *input* token, but the AutomatedStep's output token is rebuilt from the executor's
   declared outputs (`obs`) — input control leaves are **dropped across the executor
   boundary** (they live in `input.json`, not in the job result).

With `__map_id=null`, the gather's `correlate_on:"__map_id"` never matches the
coordinator's `"mp"` → the barrier never assembles K → Map stuck `running` → the Loop
wedges → 60s test-runner timeout. (Even without `correlate_on`, `value:null` would make
the gather produce garbage.)

This is the v1 limitation flagged in the Map commit message
("a body that strips the workflow token must re-promote __map_idx/__map_id").
**Confirmed: the namespace-on-token design is incompatible with AutomatedStep bodies.**

## What was ruled out

- **NOT the executor IPC-sidecar race.** A separate real bug exists (K concurrent
  `sdk:true` children → `IPC sidecar connection error NotConnected`, `output_count=0`),
  which is why `branin` is set `sdk:false` here (stdlib-only step; emits `obs` via the
  runner's file-based output sweep, no sidecar). But flipping to `sdk:false` did **not**
  clear the wedge — proving the primary blocker is the `collect` null-read above, in the
  compiler, not the executor. (The sidecar race is worth fixing separately for any
  future K-parallel `sdk:true` Map body — see executor `ipc_sidecar.rs`.)

## The fix (morning work — keystone, not a quick patch)

`service/src/compiler/lower/map.rs` `lower_map` + wiring/borrow. `collect` must source
the gathered element from the body producer's **parked** output, not the control token:

- Resolve the Map body's terminal producer (the child whose edge enters the `body_out`
  handle) and **borrow `<body_producer>.<resultVar>`** via the standard read-arc
  synthesis (read-arc on `p_<body>_data`), instead of `body.<resultVar>`.
- Correlation: since loop iterations run sequentially (propose→map→gather), a single
  iteration's `p_results` holds only that iteration's K, so `count_from` alone suffices
  — consider dropping `correlate_on` for the in-loop case, or carry `__map_idx`/`__map_id`
  through a path that survives the executor (e.g. declare them as branin outputs, or
  thread engine token-lineage). Ordering by `__map_idx` is then best-effort.
- Re-gate: the ~96 `map_*` compiler_tests + `bo_loop_demo_compiles`, then re-fire the
  `bo-converges` live test (template `00000000-0000-0000-0000-0000000000c0`).

Until then, Map works end-to-end only for bodies that carry the result on the control
token (not AutomatedStep). The offline contract (scatter/gather/borrow lowering) is
correct and merged.
