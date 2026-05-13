# Bridge Validation — Handoff Summary

## What Was Built

Cross-net bridge validation for the petri-lab engine: when nets reference each other via `bridge_out`/`bridge_in` places, the engine now validates that these string-based cross-references are consistent.

### Behavior

- **At deploy time** (`POST /api/nets/{id}/scenario`): bridge issues are logged as tracing warnings. Deployment always succeeds — target nets may not exist yet in a bottom-up deploy sequence.
- **At run-mode transition** (`PUT /api/nets/{id}/run-mode` → Running): bridge issues are validated in strict mode. If any errors exist, the engine returns **422** with an `AnalysisReport` and refuses to enter Running mode.
- **Global check** (`GET /api/bridges/check`): validates all bridges across every deployed net in one shot.
- **CLI** (`aithericon check-bridges`): colored terminal output of the global check, exits non-zero on errors.

### Issue Codes

| Code | Level | Meaning |
|---|---|---|
| `BRIDGE_DYNAMIC_TARGET` | info | `$params`/`$result` in target — skipped |
| `BRIDGE_TARGET_NET_MISSING` | warn or error | Target net not deployed |
| `BRIDGE_TARGET_PLACE_MISSING` | error | Target place doesn't exist (with "did you mean?" via Jaro-Winkler) |
| `BRIDGE_TARGET_NOT_BRIDGE_IN` | error | Target place exists but is wrong kind |
| `BRIDGE_REPLY_PLACE_MISSING` | error | `reply_to`/`reply_channels` references missing local place |
| `BRIDGE_SOURCE_NET_MISSING` | warn or error | `bridge_in_from` source net not deployed |
| `BRIDGE_SOURCE_MISMATCH` | warning | No matching `bridge_out` in annotated source net |

### Files Changed

| File | What |
|---|---|
| `core-engine/crates/application/src/bridge_validation.rs` | **New** — core logic, `NetTopologyResolver` trait, 19 unit tests |
| `core-engine/crates/application/src/analysis.rs` | Added `remote_net_id: Option<String>` to `ValidationIssue` |
| `core-engine/crates/application/src/lib.rs` | Module + re-exports |
| `core-engine/crates/application/Cargo.toml` | Added `strsim` dep |
| `Cargo.toml` (workspace) | Added `strsim = "0.11"` |
| `core-engine/crates/api/src/net_registry.rs` | `impl NetTopologyResolver for NetRegistry` |
| `core-engine/crates/api/src/handlers.rs` | Wired into `net_load_scenario` (warn), `net_set_run_mode` (strict gate), added `check_all_bridges` handler |
| `core-engine/crates/api/src/router.rs` | Added `GET /api/bridges/check` route |
| `cli/src/bridges.rs` | **New** — `check-bridges` subcommand |
| `cli/src/lib.rs` + `cli/src/main.rs` | Wired `CheckBridges` command |

### Test Coverage

19 unit tests in `bridge_validation::tests` covering every code path: happy path, all 7 issue codes, both Warn/Strict modes, fuzzy suggestions for both net and place names, dynamic `$` skipping in all positions, no-op cases, and `validate_all_bridges` with empty/populated registries.

---

## CLI Commands Added

```bash
aithericon deploy . --net-id my-net    # net-scoped deploy
aithericon activate my-net             # activate one net (422 → pretty bridge errors)
aithericon activate --all              # activate all nets (stops on first error)
aithericon check-bridges               # global bridge check
```

All justfiles (petri-lab demos, GaussianProcesses, lib.sh) have been updated to use `aithericon activate` instead of raw curl for run-mode transitions.
