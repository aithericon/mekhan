# Round 2 refactor — overnight orchestration log

Started 2026-05-28 ~23:55. Integration branch: `refactor/code-smell-round-2` (off `main` @ f2c5df3).
Main stays pristine. Each lane = own worktree + branch, single-workspace. Merge sequentially into the integration branch after verifying each compiles. Never push.

## Lanes (Wave 1) — agents running in background

| Lane | Worktree | Branch | Agent ID | Scope | Status |
|---|---|---|---|---|---|
| EXEC | (merged, wt force-removed) | refactor/r2-executor | a61b0fd9a436d000b | X1,X2,X3,X4,X5,X7,X8,X10(+X9) | ✅ MERGED — all 9 done, executor+engine check green |
| SVC | .claude/worktrees/r2-svc | refactor/r2-service | ac5ad58ad9661cc2f | S2,S4,S5,S6,S9,S10,S11,S13,B1,B3 | ✅ MERGED — 8 done, S2 no-op (prior sweeps), S11 partial-by-design, S6 SKIPPED (150+ caller surface); merged service check green |
| ENG | (merged, wt removed) | refactor/r2-engine | a19ef721464b9a36f | E2,E5,E6,E9,E11,B5 | ✅ MERGED — all 6 done (incl full E5), check+clippy green |
| FE | (merged, wt removed) | refactor/r2-frontend | ad765f0d55556119a | P1,P2,P4,P5,P6,P7 | ✅ MERGED — all 6 done, check 0 errors, 92 vitest pass |
| BUILD | (merged, wt removed) | refactor/r2-build | ad5a13c2ce80ac95d | BR1,BR2 | ✅ MERGED — both done, justfiles parse, build scopes preserved |

## Merge protocol (per lane, as it completes)
1. Read the agent's report.
2. `git -C <wt> log --oneline main..HEAD` to see commits.
3. From the MAIN checkout on `refactor/code-smell-round-2`: `git merge --no-ff <branch>`.
4. CI-parity verify on integration branch:
   - service/umbrella: `direnv exec . cargo check --workspace --lib --bins` + `cargo clippy --workspace -- -D warnings`
   - engine: `(cd engine && direnv exec . cargo check --workspace && cargo clippy --workspace -- -D warnings)`
   - executor: `(cd executor && direnv exec . cargo check --workspace && cargo clippy --workspace -- -D warnings)`
   - frontend: `(cd app && pnpm run check)`
5. If green, keep. If broken, fix forward or revert the merge; note it here.
6. `git worktree remove <wt>` to reclaim disk after a clean merge.

## Wave 2 (orchestrator, after lanes merge)
- B2 cancel_subject() — cross-workspace (executor-domain + engine consumer + executor). Add helper, route all 3.
- Batch 4 (A1/A2/A3 wire-types) — ONLY if it can be made fully green across all 3 binaries. Otherwise leave a writeup; do NOT merge half-done wire-type changes.
- Final: full CI-parity pass, update the audit doc statuses, summarize for the user.

## Pre-existing issues surfaced (NOT introduced by this round — verify on main, fix separately)
- **executor clippy**: `collapsible_match` lint in `executor-domain/src/event.rs:251` blocks `clippy --workspace -- -D warnings` locally. Pre-existing + local-toolchain drift from nix-pinned CI. `cargo check` is clean.
- **executor test compile**: `executor-service/tests/conformance_smtp.rs:333` calls `executor-smtp/src/template.rs:30` with 5 args, fn takes 4. Pre-existing (reproduces on main with round-2 changes stashed). Unrelated to X4 (which only touched `executor-backend-configs/src/smtp.rs`).

## Incident (resolved): stray `git stash pop` in r2-exec
The EXEC agent ran `git stash pop` while diagnosing the smtp test, which popped the user's pre-existing `stash@{0}: WIP on main` into the r2-exec worktree and left 4 app/ files conflicted. Resolution: verified `stash@{0}` fully intact (all 10 files), confirmed the r2-executor branch commits are executor-only (no contamination), merged the clean branch, force-removed the worktree (discarding only the redundant popped copy), re-confirmed `stash@{0}` still present. **No user WIP lost.**

## ⚠️ IMPORTANT — concurrent session detected; integration moved to a new branch
Mid-run, the MAIN checkout's working tree picked up uncommitted WIP that was NOT there at
session start: a coherent **`stepsRef` / dynamic human-task form** feature across 7 service
files + `service/tests/dynamic_human_task_e2e.rs`. The `ps` table showed **multiple live
`claude` sessions** and an external `cargo test -p mekhan-service`, and the dirty file set
changed between two consecutive git commands → **another session is actively developing in the
main checkout**, on top of the `refactor/code-smell-round-2` branch I had checked out there.

Decision: do NOT touch the main checkout's working tree (would corrupt the other session's
in-flight work). Round-2 integration was moved into an isolated worktree:
- Worktree: `.claude/worktrees/r2-integrate`
- **Final integrated branch: `refactor/code-smell-round-2-int`** (off the committed
  `refactor/code-smell-round-2` ref = doc + 4 merged lanes). SVC + Wave 2 land here.
- Backup of the other session's WIP saved to `/tmp/rescued-steps-ref-wip-tracked.patch` +
  `/tmp/rescued-dynamic_human_task_e2e.rs` (safety net; their working tree was left untouched).

Reconcile (for the user): `refactor/code-smell-round-2-int` is the complete round 2.
`refactor/code-smell-round-2` (in the main checkout) is the same minus SVC/Wave-2, and is
currently carrying another session's unrelated `stepsRef` WIP. Untangle the two branches when
convenient — there is no shared file overlap between round-2 and the stepsRef feature except
`service/src/models/template.rs` (round-2 added `chain_root_id`; stepsRef added a `steps_ref` field).

## Notes / decisions
- bypassPermissions for lane agents (unattended overnight).
- No `cargo fmt` to fix CI (local toolchain drifts from nix-pinned CI).
- Integration tests needing live stack (just dev) are compile-checked only, not run.
