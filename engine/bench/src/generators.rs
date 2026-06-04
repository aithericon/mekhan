//! Parametric net generators for L1 single-net-evaluation benchmarks.
//!
//! Each generator returns an [`aithericon_sdk::ScenarioDefinition`] (built via
//! [`Context::build`]) with its initial tokens **already seeded**, so the
//! [`petri_simulator::Simulator`] has work to do the moment it loads the net.
//! The three shapes exercise distinct evaluation-engine costs:
//!
//! - [`linear_chain`] — sequential firing depth (one token threaded through `n`
//!   transitions, `n` firings).
//! - [`parallel_branches`] — transition **selection** breadth (`k` simultaneously
//!   enabled transitions competing for tokens).
//! - [`token_fanin`] — token volume / repeated firing of a single transition
//!   (`m` tokens funnelled through one transition, `m` firings).

use aithericon_sdk::prelude::*;
use aithericon_sdk::ScenarioDefinition;

/// Passthrough Rhai logic: bind the input port `inp` straight to the output port `out`.
const PASSTHROUGH: &str = "#{ out: inp }";

/// Build an `n`-stage linear chain: `p0 -> t_0 -> p1 -> … -> t_{n-1} -> pn`.
///
/// Creates `n + 1` `UnitToken` places (`p0..=pn`) and `n` transitions, each
/// moving a single token from `p_i` to `p_{i+1}` via the [`PASSTHROUGH`] logic.
/// Exactly **one** token is seeded at `p0`; evaluating the net drives it through
/// all `n` transitions (`n` firings), leaving one token at `pn`.
///
/// Place **names** (used for simulator lookup) are `P{i}`; the terminal place is
/// `P{n}`.
pub fn linear_chain(n: usize) -> ScenarioDefinition {
    let mut ctx = Context::new(format!("linear_chain_{n}"));

    // Create all n+1 places up front so we can wire consecutive pairs.
    let mut places: Vec<PlaceHandle<UnitToken>> = Vec::with_capacity(n + 1);
    for i in 0..=n {
        places.push(ctx.state::<UnitToken>(format!("p{i}"), format!("P{i}")));
    }

    for i in 0..n {
        let (input, output) = (&places[i], &places[i + 1]);
        ctx.transition(format!("t_{i}"), format!("T{i}"))
            .auto_input("inp", input)
            .auto_output("out", output)
            .logic(PASSTHROUGH);
    }

    // Seed exactly one token at the head place.
    ctx.seed_one(&places[0], UnitToken);

    ctx.build()
}

/// Build a `k`-way parallel fan: one `input` place, one `output` place, and `k`
/// transitions each consuming from `input` and producing to `output`.
///
/// `k` `DynamicToken`s (tagged `{"i": idx}`) are seeded at `input`, so all `k`
/// branch transitions are simultaneously enabled. This stresses transition
/// **selection** among many enabled transitions. After evaluation, all `k`
/// tokens have moved to `output`.
///
/// Place **names**: `Input` and `Output`.
pub fn parallel_branches(k: usize) -> ScenarioDefinition {
    let mut ctx = Context::new(format!("parallel_branches_{k}"));

    let input = ctx.state::<DynamicToken>("input", "Input");
    let output = ctx.state::<DynamicToken>("output", "Output");

    for idx in 0..k {
        ctx.transition(format!("branch_{idx}"), format!("Branch {idx}"))
            .auto_input("inp", &input)
            .auto_output("out", &output)
            // Tag the branch index so each transition's logic is distinct.
            .logic(format!("#{{ out: #{{ i: {idx}, payload: inp }} }}"));
    }

    // Seed k tokens so every branch is enabled at once.
    for idx in 0..k {
        ctx.seed_one(&input, DynamicToken::new(serde_json::json!({ "i": idx })));
    }

    ctx.build()
}

/// Build a fan-in: one `src` place, one `sink` place, a single transition
/// `src -> sink`. `m` `UnitToken`s are seeded at `src`, so the lone transition
/// fires `m` times (token volume + repeated firing of one transition). After
/// evaluation, all `m` tokens have moved to `sink`.
///
/// Place **names**: `Src` and `Sink`.
pub fn token_fanin(m: usize) -> ScenarioDefinition {
    let mut ctx = Context::new(format!("token_fanin_{m}"));

    let src = ctx.state::<UnitToken>("src", "Src");
    let sink = ctx.state::<UnitToken>("sink", "Sink");

    ctx.transition("drain", "Drain")
        .auto_input("inp", &src)
        .auto_output("out", &sink)
        .logic(PASSTHROUGH);

    // Seed m tokens at the source.
    for _ in 0..m {
        ctx.seed_one(&src, UnitToken);
    }

    ctx.build()
}

/// Build a **binding-search** stress net: a *single* transition with `arity`
/// input places, each seeded with `tokens_per_place` `DynamicToken`s, plus a
/// correlating guard that **never matches**.
///
/// This isolates the cost the engine pays *inside* `find_valid_binding`: with a
/// guard present, the binder enumerates the full cross-product of one token per
/// input place — `tokens_per_place ^ arity` combinations — running the Rhai
/// guard on each. The guard correlates every port on a `key` field
/// (`p0.key == p1.key && …`), but each place's keys are namespaced by the place
/// index (`"{place}-{j}"`), so **no** cross-place tuple is ever equal. The
/// search therefore exhausts every combination, the transition never fires, and
/// the net goes quiescent after exactly **one** worst-case binding scan.
///
/// Because there is a single transition and zero firings, this measurement is
/// free of both transition-**selection** cost (the [`parallel_branches`] /
/// [`linear_chain`] axis) and marking churn — it is purely the combinatorial
/// `m^arity` binding search. `arity = 1` is the linear baseline (`m` guard
/// evals); `arity = 2` is quadratic; `arity = 3` cubic.
///
/// Place **names**: `In0..In{arity-1}` and `Out`. Topology: `(arity + 1, 1)`.
pub fn binding(arity: usize, tokens_per_place: usize) -> ScenarioDefinition {
    assert!(arity >= 1, "binding arity must be >= 1");
    let mut ctx = Context::new(format!("binding_a{arity}_m{tokens_per_place}"));

    let mut inputs: Vec<PlaceHandle<DynamicToken>> = Vec::with_capacity(arity);
    for p in 0..arity {
        inputs.push(ctx.state::<DynamicToken>(format!("in{p}"), format!("In{p}")));
    }
    let out = ctx.state::<DynamicToken>("out", "Out");

    // One transition, one input port `p{i}` per place. The guard correlates all
    // ports on `.key`; with per-place-disjoint keys it is never satisfiable, so
    // the binder scans the entire m^arity cross-product every enabledness check.
    let guard = if arity == 1 {
        // Single port: an always-false predicate that still reads the field, so
        // all `m` tokens are scanned (linear baseline).
        "p0.key == \"__never__\"".to_string()
    } else {
        (0..arity - 1)
            .map(|p| format!("p{p}.key == p{}.key", p + 1))
            .collect::<Vec<_>>()
            .join(" && ")
    };

    let mut tb = ctx.transition("match", "Match");
    for (p, place) in inputs.iter().enumerate() {
        tb = tb.auto_input(format!("p{p}"), place);
    }
    tb.guard(guard)
        .auto_output("out", &out)
        // Never executed (guard never passes); present only to make the net well-formed.
        .logic("#{ out: p0 }");

    // Seed each place with `tokens_per_place` tokens whose keys are disjoint
    // across places (place index baked in) — guaranteeing the guard never holds.
    for (p, place) in inputs.iter().enumerate() {
        for j in 0..tokens_per_place {
            ctx.seed_one(
                place,
                DynamicToken::new(serde_json::json!({ "key": format!("{p}-{j}") })),
            );
        }
    }

    ctx.build()
}

/// Return `(place_count, transition_count)` for a built scenario.
pub fn topology_counts(def: &ScenarioDefinition) -> (usize, usize) {
    (def.places.len(), def.transitions.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_simulator::{FinalState, Simulator};

    #[tokio::test]
    async fn linear_chain_drives_token_to_terminal() {
        let def = linear_chain(5);
        assert_eq!(topology_counts(&def), (6, 5));

        let sim = Simulator::from_sdk(def).await;
        let result = sim.evaluate_with_limit(10_000).await.unwrap();
        assert_eq!(result.final_state, FinalState::Quiescent);

        // Token threaded all the way to the last place.
        assert_eq!(sim.token_count("P5").await, 1);
        assert_eq!(sim.token_count("P0").await, 0);
    }

    #[tokio::test]
    async fn parallel_branches_route_all_tokens() {
        let def = parallel_branches(4);
        assert_eq!(topology_counts(&def), (2, 4));

        let sim = Simulator::from_sdk(def).await;
        let result = sim.evaluate_with_limit(10_000).await.unwrap();
        assert_eq!(result.final_state, FinalState::Quiescent);

        assert_eq!(sim.token_count("Output").await, 4);
        assert_eq!(sim.token_count("Input").await, 0);
    }

    #[tokio::test]
    async fn token_fanin_fires_once_per_token() {
        let def = token_fanin(8);
        assert_eq!(topology_counts(&def), (2, 1));

        let sim = Simulator::from_sdk(def).await;
        let result = sim.evaluate_with_limit(10_000).await.unwrap();
        assert_eq!(result.final_state, FinalState::Quiescent);

        assert_eq!(sim.token_count("Sink").await, 8);
        assert_eq!(sim.token_count("Src").await, 0);
    }

    #[tokio::test]
    async fn binding_scans_full_crossproduct_but_never_fires() {
        // 2 input places (In0, In1) + Out; one transition.
        let def = binding(2, 4);
        assert_eq!(topology_counts(&def), (3, 1));

        let sim = Simulator::from_sdk(def).await;
        let result = sim.evaluate_with_limit(10_000).await.unwrap();

        // The unsatisfiable correlating guard means the binder exhausts all
        // m^arity combinations and finds nothing enabled: zero firings, net
        // quiescent, every seeded token still in place. (If this ever fires,
        // the guard is matching and the worst-case scan is NOT being measured.)
        assert_eq!(result.final_state, FinalState::Quiescent);
        assert_eq!(result.steps, 0, "unsatisfiable guard must never fire");
        assert_eq!(sim.token_count("Out").await, 0);
        assert_eq!(sim.token_count("In0").await, 4);
        assert_eq!(sim.token_count("In1").await, 4);
    }
}
