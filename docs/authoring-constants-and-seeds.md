# Authoring constants & seed values (no compiler feature needed)

When porting a hand-coded net (e.g. the Bayesian-Optimization campaign) into
the visual editor, you often have net-level constants and an initial campaign
state — `BOOTSTRAP_COUNT`, `MAX_ITERATIONS`, `SIGMA_THRESHOLD`, the bootstrap
grid, the initial `{campaign_id, iteration: 0, ...}` token.

The compiler intentionally has **no** `rhai_const` / compile-time seed
primitive. Both needs are already expressible with existing nodes:

## Constants → Start `initial` port fields (per-instance), or inline literals

The compiler inlines Loop `loopCondition` / Decision `guard` strings as Rhai
literals verbatim, and `parameterize_air` seeds the **Start node's `initial`
port fields** into the first token at instance-creation time. So:

- **Tunable per run** (recommended for `BOOTSTRAP_COUNT`, `MAX_ITERATIONS`,
  `SIGMA_THRESHOLD`): declare them as fields on the Start node's `initial`
  port. Supply values per instance (`start_tokens` on `CreateInstanceRequest`,
  or a Manual `Trigger`'s `payloadMapping`). Reference them in
  Decision/Loop guards via the producer's slug:
  `campaign.max_iterations`, `campaign.sigma_threshold`, etc.
  (Set the Start node's **Slug** in the editor; guards use
  `<slug>.<field>` — the compiler synthesises the read-arc.)
- **Fixed for the template**: just write the literal directly in the guard,
  e.g. Loop condition `input._loop_<id>_count < 30` or Decision guard
  `campaign.sigma > 0.5`. No constant binding required.

## Seed token → Start `initial` port + instance creation

There is no compile-time seeded place. The initial campaign state is the
**Start token**: declare its shape as the Start `initial` port's fields and
provide the concrete values when the instance is created (or via the kicking
Trigger's `payloadMapping`). The first transition reads it like any other
token; loop-carried state then rides the accumulating workflow token.

## Summary

| Hand-coded net | Editor equivalent |
|---|---|
| `ctx.rhai_const("MAX_ITERATIONS", "30")` | Start `initial` field `max_iterations` (per-instance) **or** literal `30` in the Loop condition |
| `ctx.rhai_const("SIGMA_THRESHOLD", "0.5")` | Start field `sigma_threshold` **or** literal in the σ-routing Decision guard |
| `ctx.seed(&campaign_init, vec![CampaignState{..}])` | Start `initial` port fields + per-instance `start_tokens` / Trigger payload |

No `service/src/compiler` change is required — this is an authoring pattern,
documented here as the Phase 4 deliverable of the sub-workflow / scheduled /
catalogue-query stream.
