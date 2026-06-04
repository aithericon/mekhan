use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use petri_domain::{
    Arc as PetriArc, Marking, PlaceId, PortCardinality, ReplyRouting, Token, TokenId, Transition,
};
use serde_json::Value as JsonValue;

use crate::join_index::extract_join_constraints;
use crate::rhai_runtime::token_color_to_json;
use crate::schema_registry::SchemaRegistry;
use crate::TransitionExecutor;

/// A valid binding of tokens to input ports for a transition.
#[derive(Clone, Debug)]
pub(crate) struct TokenBinding {
    /// The port inputs (port_name -> JSON data) for this binding
    pub port_inputs: HashMap<String, JsonValue>,
    /// The tokens to consume: (place_id, token_id)
    pub consumed_tokens: Vec<(PlaceId, TokenId)>,
    /// Tokens read via read arcs: (place_id, token). These are NOT removed from marking.
    pub read_tokens: Vec<(PlaceId, petri_domain::Token)>,
    /// The maximum creation time among bound tokens (for enabling time)
    pub max_created_at: Option<DateTime<Utc>>,
    /// Reply routing from consumed tokens (for propagation and bridge_reply resolution)
    pub consumed_reply_routing: Option<ReplyRouting>,
    /// Port names that came from read arcs (subset of port_inputs keys).
    pub read_port_names: Vec<String>,
}

/// Iterator over all combinations of token indices.
/// Given sizes [2, 3], generates: [0,0], [0,1], [0,2], [1,0], [1,1], [1,2]
struct CombinationIterator {
    sizes: Vec<usize>,
    current: Vec<usize>,
    done: bool,
}

impl CombinationIterator {
    fn new(sizes: Vec<usize>) -> Self {
        let done = sizes.contains(&0);
        let current = vec![0; sizes.len()];
        Self {
            sizes,
            current,
            done,
        }
    }
}

impl Iterator for CombinationIterator {
    type Item = Vec<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        let result = self.current.clone();

        // Increment with carry
        let mut carry = true;
        for i in (0..self.sizes.len()).rev() {
            if carry {
                self.current[i] += 1;
                if self.current[i] >= self.sizes[i] {
                    self.current[i] = 0;
                } else {
                    carry = false;
                }
            }
        }

        if carry {
            self.done = true;
        }

        Some(result)
    }
}

/// Find a valid token binding for a transition.
///
/// Searches through all combinations of tokens from input places to find
/// a binding that satisfies the guard. Returns None if no valid binding exists.
///
/// For transitions without guards, returns the first available binding (FIFO).
pub(crate) fn find_valid_binding(
    executor: &TransitionExecutor,
    transition: &Transition,
    input_arcs: &[&PetriArc],
    marking: &Marking,
    schema_registry: Option<&SchemaRegistry>,
) -> Option<TokenBinding> {
    // Collect tokens from each input place
    let mut arc_tokens: Vec<Vec<&Token>> = Vec::new();
    let mut arc_sizes: Vec<usize> = Vec::new();

    for arc in input_arcs {
        let tokens = marking.tokens_at(&arc.place_id);
        // count_from arcs are gather barriers: the required count K depends on a
        // coordinator token that is not yet bound here, so the real count check is
        // deferred to build_binding_for_indices. Skip the weight-based early return.
        if arc.count_from.is_none() && tokens.len() < arc.weight {
            return None; // Not enough tokens
        }
        arc_sizes.push(tokens.len());
        arc_tokens.push(tokens.iter().collect());
    }

    // If no input arcs, return empty binding
    if input_arcs.is_empty() {
        return Some(TokenBinding {
            port_inputs: HashMap::new(),
            consumed_tokens: vec![],
            read_tokens: vec![],
            read_port_names: vec![],
            max_created_at: None,
            consumed_reply_routing: None,
        });
    }

    // If no guard, use FIFO (first token from each place)
    if transition.guard.is_none() && schema_registry.is_none() {
        return build_binding_for_indices(
            transition,
            input_arcs,
            &arc_tokens,
            &vec![0; input_arcs.len()],
            schema_registry,
        );
    }

    // If no guard but schema validation is active, still try FIFO first
    if transition.guard.is_none() {
        if let Some(binding) = build_binding_for_indices(
            transition,
            input_arcs,
            &arc_tokens,
            &vec![0; input_arcs.len()],
            schema_registry,
        ) {
            return Some(binding);
        }
        // FIFO failed schema validation — fall through to search all combinations
    }

    // Indexed equi-join fast path. When the guard declares cross-port equality
    // correlations (e.g. `.correlate()` → `a.k == b.k`), prune the m^k
    // cross-product to only the key-agreeing combinations. The guard is still
    // evaluated on every survivor, so this removes only provably-failing
    // combinations — see `crate::join_index`. `JoinPlan::build` returns `None`
    // (→ fall through to the full cross-product) whenever the structure is not
    // safely indexable.
    if let Some(guard_script) = &transition.guard {
        if input_arcs.len() >= 2 {
            let constraints = extract_join_constraints(guard_script);
            if let Some(plan) =
                JoinPlan::build(transition, input_arcs, &arc_tokens, &constraints)
            {
                let ctx = JoinCtx {
                    executor,
                    transition,
                    input_arcs,
                    arc_tokens: &arc_tokens,
                    schema_registry,
                    guard_script,
                    plan: &plan,
                };
                let mut indices = vec![0usize; input_arcs.len()];
                return ctx.search(0, &mut indices);
            }
        }
    }

    // Search all combinations for one that satisfies the guard (and schema)
    let combo_iter = CombinationIterator::new(arc_sizes);

    for indices in combo_iter {
        if let Some(binding) = build_binding_for_indices(
            transition,
            input_arcs,
            &arc_tokens,
            &indices,
            schema_registry,
        ) {
            // Check if guard passes
            if let Some(guard_script) = &transition.guard {
                match executor.evaluate_guard(guard_script, &binding.port_inputs) {
                    Ok(true) => return Some(binding),
                    Ok(false) => continue,
                    Err(_) => continue,
                }
            } else {
                return Some(binding);
            }
        }
    }

    None
}

/// Build a TokenBinding for a specific set of token indices.
fn build_binding_for_indices(
    transition: &Transition,
    input_arcs: &[&PetriArc],
    arc_tokens: &[Vec<&Token>],
    indices: &[usize],
    schema_registry: Option<&SchemaRegistry>,
) -> Option<TokenBinding> {
    let mut port_inputs: HashMap<String, JsonValue> = HashMap::new();
    let mut consumed_tokens: Vec<(PlaceId, TokenId)> = Vec::new();
    let mut read_tokens: Vec<(PlaceId, Token)> = Vec::new();
    let mut read_port_names: Vec<String> = Vec::new();
    let mut max_created_at: Option<DateTime<Utc>> = None;
    let mut consumed_reply_routing: Option<ReplyRouting> = None;

    // Two passes: count_from (gather-barrier) arcs depend on a coordinator token
    // bound by another arc, so they must be processed AFTER the non-count_from
    // arcs have populated port_inputs. Non-count_from arcs keep today's behavior
    // exactly; count_from arcs run the counted-gather path below.
    for pass in 0..2 {
        for (arc_idx, arc) in input_arcs.iter().enumerate() {
            let is_gather = arc.count_from.is_some();
            // pass 0: non-count_from arcs; pass 1: count_from arcs.
            if (pass == 0) == is_gather {
                continue;
            }

            // Get cardinality
            let port = transition.input_port(&arc.port_name);
            let cardinality = port
                .map(|p| &p.cardinality)
                .unwrap_or(&PortCardinality::Single);

            // ── Gather barrier: count-gated, correlated Batch input ──────────
            if let Some(count_ref) = &arc.count_from {
                let tokens = &arc_tokens[arc_idx];

                // Resolve K from the referenced coordinator port: "expected.k"
                // → port_inputs["expected"]["k"]. The coordinator must already be
                // bound (it is a non-count_from arc resolved in pass 0).
                let (coord_port, count_field) = match count_ref.split_once('.') {
                    Some((p, f)) => (p, f),
                    None => return None, // malformed reference
                };
                let coord_value = port_inputs.get(coord_port)?;
                let k = coord_value
                    .get(count_field)
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize)?;

                // Filter the place's tokens to the matching subset.
                let matching: Vec<&&Token> = if let Some(field) = &arc.correlate_on {
                    // Eligible tokens are those whose correlate field equals the
                    // coordinator token's same-named field.
                    let want = coord_value.get(field);
                    tokens
                        .iter()
                        .filter(|t| {
                            let tc = token_color_to_json(&t.color);
                            tc.get(field) == want
                        })
                        .collect()
                } else {
                    tokens.iter().collect()
                };

                // Barrier: fire only when at least K matching tokens are present.
                if matching.len() < k {
                    return None;
                }

                // Take exactly the first K matching tokens (deterministic order =
                // marking vector order) as BOTH the script-visible array and the
                // consumed set.
                let selected: Vec<&&Token> = matching.into_iter().take(k).collect();
                let token_data = JsonValue::Array(
                    selected
                        .iter()
                        .map(|t| token_color_to_json(&t.color))
                        .collect(),
                );

                for t in &selected {
                    if !arc.read {
                        if let Some(incoming) = &t.reply_routing {
                            consumed_reply_routing = match consumed_reply_routing {
                                None => Some(incoming.clone()),
                                Some(existing) => match merge_reply_routing(existing, incoming) {
                                    Some(merged) => Some(merged),
                                    None => {
                                        tracing::debug!(
                                            arc_port = %arc.port_name,
                                            "reply_routing merge conflict — skipping binding"
                                        );
                                        return None;
                                    }
                                },
                            };
                        }
                    }
                    if arc.read {
                        read_tokens.push((arc.place_id.clone(), (***t).clone()));
                    } else {
                        consumed_tokens.push((arc.place_id.clone(), t.id.clone()));
                    }
                    max_created_at =
                        Some(max_created_at.map_or(t.created_at, |m| m.max(t.created_at)));
                }
                if arc.read {
                    read_port_names.push(arc.port_name.clone());
                }

                // Validate each element against the port schema (item shape) if present.
                if let Some(registry) = schema_registry {
                    if let Some(schema_ref) = port.and_then(|p| p.schema_ref.as_ref()) {
                        for el in selected.iter() {
                            let ev = token_color_to_json(&el.color);
                            if registry.validate(schema_ref, &ev).is_err() {
                                return None;
                            }
                        }
                    }
                }

                port_inputs.insert(arc.port_name.clone(), token_data);
                continue;
            }

            // ── Non-count_from arc: exactly today's behavior ─────────────────
            let token_idx = indices[arc_idx];
            let tokens = &arc_tokens[arc_idx];

            if token_idx >= tokens.len() {
                return None;
            }

            // Read arcs are non-consuming and borrow a parked value. When a
            // place accumulates tokens across loop iterations (a loop-body
            // producer re-parks each pass — e.g. a Map's `itemsRef` producer or
            // a loop accumulator's body-child output), the borrow must see the
            // MOST RECENT parked value, not the stale oldest one plain FIFO
            // indexing picks. The marking Vec is append-ordered, so the last
            // token is the newest park (deterministic → replay-safe). Single-
            // token places (the common, non-loop case) are unaffected.
            let token = if arc.read {
                tokens.last().copied().unwrap_or(tokens[token_idx])
            } else {
                tokens[token_idx]
            };

            // Merge reply_routing from consumed tokens (skip read arcs)
            if !arc.read {
                if let Some(incoming) = &token.reply_routing {
                    consumed_reply_routing = match consumed_reply_routing {
                        None => Some(incoming.clone()),
                        Some(existing) => match merge_reply_routing(existing, incoming) {
                            Some(merged) => Some(merged),
                            None => {
                                tracing::debug!(
                                    arc_port = %arc.port_name,
                                    "reply_routing merge conflict — skipping binding"
                                );
                                return None;
                            }
                        },
                    };
                }
            }

            // For Single cardinality, we just use the one token at this index
            // For Batch, we'd need different logic (not changing that behavior)
            let token_data: JsonValue = match cardinality {
                PortCardinality::Single => token_color_to_json(&token.color),
                PortCardinality::Batch => {
                    // For batch, collect ALL tokens from this place
                    let batch_tokens: Vec<JsonValue> = tokens
                        .iter()
                        .map(|t| token_color_to_json(&t.color))
                        .collect();
                    JsonValue::Array(batch_tokens)
                }
            };

            // Track consumed or read tokens
            if arc.read {
                // Read arc: token is available to script but NOT removed from marking
                read_port_names.push(arc.port_name.clone());
                match cardinality {
                    PortCardinality::Single => {
                        read_tokens.push((arc.place_id.clone(), token.clone()));
                        max_created_at = Some(
                            max_created_at.map_or(token.created_at, |t| t.max(token.created_at)),
                        );
                    }
                    PortCardinality::Batch => {
                        for t in tokens.iter() {
                            read_tokens.push((arc.place_id.clone(), (*t).clone()));
                            max_created_at =
                                Some(max_created_at.map_or(t.created_at, |m| m.max(t.created_at)));
                        }
                    }
                }
            } else {
                // Normal arc: token is consumed
                match cardinality {
                    PortCardinality::Single => {
                        consumed_tokens.push((arc.place_id.clone(), token.id.clone()));
                        max_created_at = Some(
                            max_created_at.map_or(token.created_at, |t| t.max(token.created_at)),
                        );
                    }
                    PortCardinality::Batch => {
                        for t in tokens.iter().skip(token_idx).take(arc.weight) {
                            consumed_tokens.push((arc.place_id.clone(), t.id.clone()));
                            max_created_at =
                                Some(max_created_at.map_or(t.created_at, |m| m.max(t.created_at)));
                        }
                    }
                }
            }

            // Validate token data against port schema if registry is present
            if let Some(registry) = schema_registry {
                if let Some(port) = transition.input_port(&arc.port_name) {
                    if let Some(ref schema_ref) = port.schema_ref {
                        if registry.validate(schema_ref, &token_data).is_err() {
                            return None; // Wrong-shaped token — skip this binding
                        }
                    }
                }
            }

            port_inputs.insert(arc.port_name.clone(), token_data);
        }
    }

    Some(TokenBinding {
        port_inputs,
        consumed_tokens,
        read_tokens,
        read_port_names,
        max_created_at,
        consumed_reply_routing,
    })
}

/// Merge two `ReplyRouting` values. Returns `None` on conflict.
///
/// - `reply_to`: must be identical if both are `Some`
/// - `reply_channels`: maps are merged; conflicting keys (same name, different address) → `None`
fn merge_reply_routing(existing: ReplyRouting, incoming: &ReplyRouting) -> Option<ReplyRouting> {
    // Merge reply_to: if both present, they must match
    let reply_to = match (&existing.reply_to, &incoming.reply_to) {
        (Some(a), Some(b)) if a != b => return None,
        (Some(_), _) => existing.reply_to,
        (None, other) => other.clone(),
    };

    // Merge reply_channels maps
    let reply_channels = match (existing.reply_channels, &incoming.reply_channels) {
        (Some(mut a), Some(b)) => {
            for (key, addr) in b {
                if let Some(existing_addr) = a.get(key) {
                    if existing_addr != addr {
                        return None; // Conflicting channel key
                    }
                } else {
                    a.insert(key.clone(), addr.clone());
                }
            }
            Some(a)
        }
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    };

    Some(ReplyRouting {
        reply_to,
        reply_channels,
    })
}

// ── Indexed equi-join binding ──────────────────────────────────────────────

/// How each input arc is enumerated under an indexed join plan.
enum ArcRole {
    /// Index is irrelevant to the built binding (read arcs use the newest
    /// token; `count_from` gather arcs do their own filtering) — pin to 0.
    Pinned,
    /// No join constraint applies: enumerate the full `0..len` range, exactly
    /// as the cross-product would (normal Single unjoined, or normal Batch).
    Free,
    /// Joined: candidate indices are pruned to those whose key matches the
    /// already-fixed partner tokens via `back_edges`.
    Joined(Vec<BackEdge>),
}

/// A pruning edge from a later arc to an earlier (already-fixed) partner arc:
/// `self_token[self_path] == partner_token[partner_path]` is necessary.
struct BackEdge {
    partner: usize,
    self_path: Vec<String>,
    partner_path: Vec<String>,
}

/// A plan for enumerating only key-agreeing token combinations of a guarded
/// multi-input transition, built from the guard's equi-join constraints.
struct JoinPlan {
    roles: Vec<ArcRole>,
    /// `colors[arc_idx][token_idx]` — token color JSON, precomputed once.
    colors: Vec<Vec<JsonValue>>,
    /// `(arc_idx, self_path) → (scalar key → ascending token indices)`.
    maps: HashMap<(usize, Vec<String>), HashMap<String, Vec<usize>>>,
}

impl JoinPlan {
    /// Build an indexed plan, or `None` to signal "not safely indexable — use
    /// the full cross-product". Returns `None` when there are no usable
    /// constraints (both sides must map to indexable arcs) or when any join
    /// value is a compound (array/object), which cannot be safely bucketed.
    fn build(
        transition: &Transition,
        input_arcs: &[&PetriArc],
        arc_tokens: &[Vec<&Token>],
        constraints: &[crate::join_index::JoinConstraint],
    ) -> Option<Self> {
        if constraints.is_empty() {
            return None;
        }
        let k = input_arcs.len();

        let mut port_to_arc: HashMap<&str, usize> = HashMap::with_capacity(k);
        for (i, arc) in input_arcs.iter().enumerate() {
            port_to_arc.insert(arc.port_name.as_str(), i);
        }

        // An arc is indexable only if its index selects exactly one token whose
        // value we can key on: a normal (non-read, non-gather) Single arc.
        let indexable: Vec<bool> = input_arcs
            .iter()
            .map(|arc| {
                if arc.read || arc.count_from.is_some() {
                    return false;
                }
                let card = transition
                    .input_port(&arc.port_name)
                    .map(|p| &p.cardinality)
                    .unwrap_or(&PortCardinality::Single);
                matches!(card, PortCardinality::Single)
            })
            .collect();

        // Collect back-edges (always attached to the LATER arc, so its partner
        // is already fixed when we reach it) from usable constraints.
        let mut back: Vec<Vec<BackEdge>> = (0..k).map(|_| Vec::new()).collect();
        let mut needed_maps: HashSet<(usize, Vec<String>)> = HashSet::new();
        let mut any_usable = false;

        for con in constraints {
            let (ia, ib) = match (
                port_to_arc.get(con.port_a.as_str()),
                port_to_arc.get(con.port_b.as_str()),
            ) {
                (Some(&a), Some(&b)) => (a, b),
                _ => continue,
            };
            if ia == ib || !indexable[ia] || !indexable[ib] {
                continue;
            }
            // Normalize so `lo < hi`; the back-edge hangs on `hi`.
            let (lo, lo_path, hi, hi_path) = if ia < ib {
                (ia, &con.path_a, ib, &con.path_b)
            } else {
                (ib, &con.path_b, ia, &con.path_a)
            };
            back[hi].push(BackEdge {
                partner: lo,
                self_path: hi_path.clone(),
                partner_path: lo_path.clone(),
            });
            needed_maps.insert((hi, hi_path.clone()));
            any_usable = true;
        }

        if !any_usable {
            return None;
        }

        // Precompute token colors once (m·k total, vs m^k guard evals saved).
        let colors: Vec<Vec<JsonValue>> = arc_tokens
            .iter()
            .map(|toks| toks.iter().map(|t| token_color_to_json(&t.color)).collect())
            .collect();

        // Every join value (map keys AND probe values) must be a scalar so the
        // canonical key faithfully mirrors guard equality; bail otherwise.
        let mut key_paths: HashSet<(usize, Vec<String>)> = needed_maps.clone();
        for edges in &back {
            for be in edges {
                key_paths.insert((be.partner, be.partner_path.clone()));
            }
        }
        for (arc_i, path) in &key_paths {
            for color in &colors[*arc_i] {
                scalar_key(pluck_path(color, path))?;
            }
        }

        // Build the index maps for arcs probed by a back-edge.
        let mut maps: HashMap<(usize, Vec<String>), HashMap<String, Vec<usize>>> = HashMap::new();
        for (arc_i, path) in needed_maps {
            let mut m: HashMap<String, Vec<usize>> = HashMap::new();
            for (idx, color) in colors[arc_i].iter().enumerate() {
                let key = scalar_key(pluck_path(color, &path)).expect("validated scalar above");
                m.entry(key).or_default().push(idx); // pushed in ascending idx order
            }
            maps.insert((arc_i, path), m);
        }

        let roles = input_arcs
            .iter()
            .enumerate()
            .map(|(i, arc)| {
                if arc.read || arc.count_from.is_some() {
                    ArcRole::Pinned
                } else if !back[i].is_empty() {
                    ArcRole::Joined(std::mem::take(&mut back[i]))
                } else {
                    ArcRole::Free
                }
            })
            .collect();

        Some(JoinPlan {
            roles,
            colors,
            maps,
        })
    }
}

/// Shared context for the recursive pruned search.
struct JoinCtx<'a> {
    executor: &'a TransitionExecutor,
    transition: &'a Transition,
    input_arcs: &'a [&'a PetriArc],
    arc_tokens: &'a [Vec<&'a Token>],
    schema_registry: Option<&'a SchemaRegistry>,
    guard_script: &'a str,
    plan: &'a JoinPlan,
}

impl JoinCtx<'_> {
    /// Depth-first search over arcs in order, enumerating only key-agreeing
    /// combinations in the same lexicographic order the cross-product would,
    /// returning the first binding that passes the guard. Deterministic ⇒
    /// replay-safe.
    fn search(&self, level: usize, indices: &mut Vec<usize>) -> Option<TokenBinding> {
        if level == self.input_arcs.len() {
            let binding = build_binding_for_indices(
                self.transition,
                self.input_arcs,
                self.arc_tokens,
                indices,
                self.schema_registry,
            )?;
            return match self.executor.evaluate_guard(self.guard_script, &binding.port_inputs) {
                Ok(true) => Some(binding),
                _ => None,
            };
        }

        for cand in self.candidates(level, indices) {
            indices[level] = cand;
            if let Some(binding) = self.search(level + 1, indices) {
                return Some(binding);
            }
        }
        None
    }

    /// Ascending candidate indices for `level`, given the indices already fixed
    /// for earlier arcs.
    fn candidates(&self, level: usize, indices: &[usize]) -> Vec<usize> {
        let size = self.arc_tokens[level].len();
        match &self.plan.roles[level] {
            ArcRole::Pinned => vec![0],
            ArcRole::Free => (0..size).collect(),
            ArcRole::Joined(back_edges) => {
                let mut cand: Option<Vec<usize>> = None;
                for be in back_edges {
                    let partner_color = &self.plan.colors[be.partner][indices[be.partner]];
                    let key = scalar_key(pluck_path(partner_color, &be.partner_path))
                        .expect("validated scalar above");
                    let matches = self
                        .plan
                        .maps
                        .get(&(level, be.self_path.clone()))
                        .and_then(|m| m.get(&key))
                        .cloned()
                        .unwrap_or_default();
                    cand = Some(match cand {
                        None => matches,
                        Some(prev) => intersect_sorted(&prev, &matches),
                    });
                }
                cand.unwrap_or_else(|| (0..size).collect())
            }
        }
    }
}

/// Walk a dotted field path into a JSON value.
fn pluck_path<'a>(value: &'a JsonValue, path: &[String]) -> Option<&'a JsonValue> {
    let mut cur = value;
    for seg in path {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Canonical bucket key for a join value, mirroring the runtime's `==`
/// equality classes (numbers coerced to f64, ±0 normalized). Returns `None`
/// for compound values (array/object), which we refuse to index. A missing
/// field and JSON null share the null bucket (both become Rhai `()`).
fn scalar_key(value: Option<&JsonValue>) -> Option<String> {
    match value {
        None | Some(JsonValue::Null) => Some("u".to_string()),
        Some(JsonValue::Bool(b)) => Some(format!("b{b}")),
        Some(JsonValue::Number(n)) => {
            let mut f = n.as_f64().unwrap_or(f64::NAN);
            if f == 0.0 {
                f = 0.0; // normalize -0.0 → +0.0 (Rhai treats them equal)
            }
            Some(format!("n{:016x}", f.to_bits()))
        }
        Some(JsonValue::String(s)) => Some(format!("s{s}")),
        Some(JsonValue::Array(_)) | Some(JsonValue::Object(_)) => None,
    }
}

/// Intersect two ascending index vectors, preserving ascending order.
fn intersect_sorted(a: &[usize], b: &[usize]) -> Vec<usize> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::{
        Arc as PetriArc, BridgeReplyAddress, Marking, PlaceId, Port, Token, TokenColor, Transition,
        TransitionId,
    };
    use serde_json::json;

    use crate::TransitionExecutor;

    /// Helper: create a Token with JSON data.
    fn data_token(value: serde_json::Value) -> Token {
        Token::new(TokenColor::Data(value))
    }

    /// Helper: build a minimal Transition with given input ports.
    fn transition_with_ports(input_ports: Vec<Port>) -> Transition {
        let mut t = Transition::new("test_transition", r#"#{}"#);
        t.input_ports = input_ports;
        t
    }

    // ── Batch read arc: all tokens should appear ────────────────────────

    #[test]
    fn batch_read_arc_returns_all_tokens() {
        let executor = TransitionExecutor::new();
        let place_id = PlaceId::named("observations");
        let t_id = TransitionId::named("dispatch_fit");

        let transition = transition_with_ports(vec![Port::batch("obs")]);

        let arc = PetriArc::input(place_id.clone(), t_id, "obs").with_read(true);

        // Seed 5 tokens
        let mut marking = Marking::new();
        for i in 0..5 {
            marking.add_token(
                place_id.clone(),
                data_token(json!({ "a": i as f64 * 0.1, "d": 0.5, "z": i as f64 })),
            );
        }

        let arcs: Vec<&PetriArc> = vec![&arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed");

        // The batch port should contain ALL 5 tokens as a JSON array
        let obs = binding.port_inputs.get("obs").expect("obs port missing");
        let arr = obs.as_array().expect("obs should be an array");
        assert_eq!(arr.len(), 5, "batch read should return all 5 tokens");

        // Read arc: tokens should NOT be consumed
        assert!(
            binding.consumed_tokens.is_empty(),
            "read arc must not consume tokens"
        );

        // All 5 tokens tracked as read
        assert_eq!(
            binding.read_tokens.len(),
            5,
            "all 5 tokens should be tracked as read"
        );

        // Port should be in read_port_names
        assert!(binding.read_port_names.contains(&"obs".to_string()));
    }

    #[test]
    fn batch_read_arc_with_single_token() {
        let executor = TransitionExecutor::new();
        let place_id = PlaceId::named("observations");
        let t_id = TransitionId::named("dispatch_fit");

        let transition = transition_with_ports(vec![Port::batch("obs")]);
        let arc = PetriArc::input(place_id.clone(), t_id, "obs").with_read(true);

        let mut marking = Marking::new();
        marking.add_token(place_id.clone(), data_token(json!({ "x": 1 })));

        let arcs: Vec<&PetriArc> = vec![&arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed");

        let obs = binding.port_inputs.get("obs").unwrap();
        let arr = obs.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert!(binding.consumed_tokens.is_empty());
        assert_eq!(binding.read_tokens.len(), 1);
    }

    #[test]
    fn batch_read_arc_with_normal_input() {
        // Scenario: one batch-read arc + one normal single-consume arc
        // (mimics dispatch_fit: trigger consumed, observations read)
        let executor = TransitionExecutor::new();
        let obs_place = PlaceId::named("observation_log");
        let trigger_place = PlaceId::named("fit_trigger");
        let t_id = TransitionId::named("dispatch_fit");

        let transition = transition_with_ports(vec![Port::new("trigger"), Port::batch("obs")]);

        let trigger_arc = PetriArc::input(trigger_place.clone(), t_id.clone(), "trigger");
        let obs_arc = PetriArc::input(obs_place.clone(), t_id, "obs").with_read(true);

        let mut marking = Marking::new();

        // 1 trigger token
        marking.add_token(trigger_place.clone(), data_token(json!({ "iteration": 5 })));

        // 4 observation tokens
        for i in 0..4 {
            marking.add_token(
                obs_place.clone(),
                data_token(json!({ "a": i, "d": i, "z": i })),
            );
        }

        let arcs: Vec<&PetriArc> = vec![&trigger_arc, &obs_arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed");

        // Trigger consumed
        assert_eq!(binding.consumed_tokens.len(), 1);
        let trigger_data = binding.port_inputs.get("trigger").unwrap();
        assert_eq!(trigger_data["iteration"], 5);

        // All 4 observations read (not consumed)
        let obs = binding.port_inputs.get("obs").unwrap();
        assert_eq!(obs.as_array().unwrap().len(), 4);
        assert_eq!(binding.read_tokens.len(), 4);
    }

    #[test]
    fn batch_read_empty_place_returns_none() {
        let executor = TransitionExecutor::new();
        let place_id = PlaceId::named("observations");
        let t_id = TransitionId::named("dispatch_fit");

        let transition = transition_with_ports(vec![Port::batch("obs")]);
        let arc = PetriArc::input(place_id.clone(), t_id, "obs").with_read(true);

        let marking = Marking::new(); // empty — no tokens

        let arcs: Vec<&PetriArc> = vec![&arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None);
        assert!(binding.is_none(), "should not bind when place is empty");
    }

    // ── Gather barrier: count-gated, correlated Batch input ────────────

    /// Helper: build the gather scenario — a Batch "results" input arc with
    /// `count_from = "expected.k"` plus a Single read arc "expected" (coordinator).
    fn gather_setup() -> (
        TransitionExecutor,
        Transition,
        PlaceId,
        PlaceId,
        PetriArc,
        PetriArc,
    ) {
        let executor = TransitionExecutor::new();
        let results_place = PlaceId::named("results");
        let expected_place = PlaceId::named("expected");
        let t_id = TransitionId::named("gather");

        let transition = transition_with_ports(vec![Port::new("expected"), Port::batch("results")]);

        // coordinator (single, read) bound first; results (batch, count-gated)
        let expected_arc =
            PetriArc::input(expected_place.clone(), t_id.clone(), "expected").with_read(true);
        let results_arc =
            PetriArc::input(results_place.clone(), t_id, "results").with_count_from("expected.k");

        (
            executor,
            transition,
            results_place,
            expected_place,
            expected_arc,
            results_arc,
        )
    }

    #[test]
    fn gather_barrier_holds_until_k_present() {
        let (executor, transition, results_place, expected_place, expected_arc, results_arc) =
            gather_setup();

        let mut marking = Marking::new();
        marking.add_token(expected_place.clone(), data_token(json!({ "k": 3 })));
        // Only 2 of 3 results present.
        for i in 0..2 {
            marking.add_token(results_place.clone(), data_token(json!({ "v": i })));
        }

        let arcs: Vec<&PetriArc> = vec![&expected_arc, &results_arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None);
        assert!(
            binding.is_none(),
            "barrier must hold while fewer than K results present"
        );
    }

    #[test]
    fn gather_binds_and_consumes_exactly_k() {
        let (executor, transition, results_place, expected_place, expected_arc, results_arc) =
            gather_setup();

        let mut marking = Marking::new();
        marking.add_token(expected_place.clone(), data_token(json!({ "k": 3 })));
        for i in 0..3 {
            marking.add_token(results_place.clone(), data_token(json!({ "v": i })));
        }

        let arcs: Vec<&PetriArc> = vec![&expected_arc, &results_arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed once K results present");

        // Script sees exactly K results as an array.
        let results = binding.port_inputs.get("results").expect("results missing");
        let arr = results.as_array().expect("results should be an array");
        assert_eq!(arr.len(), 3, "script should see exactly K results");

        // Coordinator was a read arc → not consumed; exactly K results consumed.
        assert_eq!(
            binding.consumed_tokens.len(),
            3,
            "exactly K result tokens consumed"
        );
        for (place_id, _) in &binding.consumed_tokens {
            assert_eq!(place_id, &results_place, "only results are consumed");
        }
    }

    #[test]
    fn gather_with_more_than_k_takes_first_k() {
        let (executor, transition, results_place, expected_place, expected_arc, results_arc) =
            gather_setup();

        let mut marking = Marking::new();
        marking.add_token(expected_place.clone(), data_token(json!({ "k": 2 })));
        for i in 0..5 {
            marking.add_token(results_place.clone(), data_token(json!({ "v": i })));
        }

        let arcs: Vec<&PetriArc> = vec![&expected_arc, &results_arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed");

        let arr = binding.port_inputs["results"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "exactly K taken");
        // Deterministic marking order → first two tokens (v:0, v:1).
        assert_eq!(arr[0]["v"], 0);
        assert_eq!(arr[1]["v"], 1);
        assert_eq!(binding.consumed_tokens.len(), 2);
    }

    #[test]
    fn gather_correlates_on_iteration_id() {
        let executor = TransitionExecutor::new();
        let results_place = PlaceId::named("results");
        let expected_place = PlaceId::named("expected");
        let t_id = TransitionId::named("gather");

        let transition = transition_with_ports(vec![Port::new("expected"), Port::batch("results")]);

        let expected_arc =
            PetriArc::input(expected_place.clone(), t_id.clone(), "expected").with_read(true);
        let results_arc = PetriArc::input(results_place.clone(), t_id, "results")
            .with_count_from("expected.k")
            .with_correlate_on("iteration_id");

        let mut marking = Marking::new();
        // Coordinator says iteration A needs k=3.
        marking.add_token(
            expected_place.clone(),
            data_token(json!({ "k": 3, "iteration_id": "A" })),
        );
        // 3 tokens of iteration A, 2 of iteration B (interleaved).
        marking.add_token(
            results_place.clone(),
            data_token(json!({ "iteration_id": "A", "v": 0 })),
        );
        marking.add_token(
            results_place.clone(),
            data_token(json!({ "iteration_id": "B", "v": 100 })),
        );
        marking.add_token(
            results_place.clone(),
            data_token(json!({ "iteration_id": "A", "v": 1 })),
        );
        marking.add_token(
            results_place.clone(),
            data_token(json!({ "iteration_id": "B", "v": 101 })),
        );
        marking.add_token(
            results_place.clone(),
            data_token(json!({ "iteration_id": "A", "v": 2 })),
        );

        let arcs: Vec<&PetriArc> = vec![&expected_arc, &results_arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed: 3 A-tokens present");

        // Script sees exactly the 3 A-tokens.
        let arr = binding.port_inputs["results"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        for el in arr {
            assert_eq!(el["iteration_id"], "A", "only iteration-A tokens gathered");
        }

        // Exactly the 3 A result tokens consumed; the 2 B-tokens left in place.
        assert_eq!(binding.consumed_tokens.len(), 3);
        let consumed_ids: Vec<_> = binding.consumed_tokens.iter().map(|(_, id)| id).collect();
        let remaining: Vec<_> = marking
            .tokens_at(&results_place)
            .iter()
            .filter(|t| !consumed_ids.contains(&&t.id))
            .map(|t| token_color_to_json(&t.color))
            .collect();
        assert_eq!(remaining.len(), 2, "2 B-tokens remain unconsumed");
        for r in &remaining {
            assert_eq!(r["iteration_id"], "B");
        }
    }

    #[test]
    fn gather_correlation_barrier_holds_when_subset_short() {
        // 5 results total but only 2 of the correlated iteration → barrier holds.
        let executor = TransitionExecutor::new();
        let results_place = PlaceId::named("results");
        let expected_place = PlaceId::named("expected");
        let t_id = TransitionId::named("gather");

        let transition = transition_with_ports(vec![Port::new("expected"), Port::batch("results")]);
        let expected_arc =
            PetriArc::input(expected_place.clone(), t_id.clone(), "expected").with_read(true);
        let results_arc = PetriArc::input(results_place.clone(), t_id, "results")
            .with_count_from("expected.k")
            .with_correlate_on("iteration_id");

        let mut marking = Marking::new();
        marking.add_token(
            expected_place.clone(),
            data_token(json!({ "k": 3, "iteration_id": "A" })),
        );
        marking.add_token(
            results_place.clone(),
            data_token(json!({ "iteration_id": "A" })),
        );
        marking.add_token(
            results_place.clone(),
            data_token(json!({ "iteration_id": "A" })),
        );
        // 3 B-tokens — irrelevant to iteration A's gather.
        for _ in 0..3 {
            marking.add_token(
                results_place.clone(),
                data_token(json!({ "iteration_id": "B" })),
            );
        }

        let arcs: Vec<&PetriArc> = vec![&expected_arc, &results_arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None);
        assert!(
            binding.is_none(),
            "barrier holds: only 2 of K=3 correlated tokens present"
        );
    }

    // ── Reply routing merge tests ──────────────────────────────────────

    fn addr(net: &str, place: &str) -> BridgeReplyAddress {
        BridgeReplyAddress {
            net_id: net.to_string(),
            place_name: place.to_string(),
        }
    }

    #[test]
    fn merge_reply_routing_one_token_has_it() {
        let executor = TransitionExecutor::new();
        let p1 = PlaceId::named("a");
        let p2 = PlaceId::named("b");
        let t_id = TransitionId::named("t");

        let transition = transition_with_ports(vec![Port::new("a"), Port::new("b")]);
        let arc1 = PetriArc::input(p1.clone(), t_id.clone(), "a");
        let arc2 = PetriArc::input(p2.clone(), t_id, "b");

        let mut marking = Marking::new();
        // Token with reply routing
        let mut t1 = data_token(json!({"x": 1}));
        t1 = t1.with_reply_routing(ReplyRouting {
            reply_to: Some(addr("net-a", "reply_inbox")),
            reply_channels: None,
        });
        marking.add_token(p1, t1);
        // Token without reply routing
        marking.add_token(p2, data_token(json!({"y": 2})));

        let arcs: Vec<&PetriArc> = vec![&arc1, &arc2];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed");
        let routing = binding.consumed_reply_routing.expect("should have routing");
        assert_eq!(routing.reply_to.unwrap().net_id, "net-a");
    }

    #[test]
    fn merge_reply_routing_compatible_channels() {
        let existing = ReplyRouting {
            reply_to: None,
            reply_channels: Some(HashMap::from([(
                "alpha".to_string(),
                addr("net-a", "alpha_inbox"),
            )])),
        };
        let incoming = ReplyRouting {
            reply_to: None,
            reply_channels: Some(HashMap::from([(
                "beta".to_string(),
                addr("net-a", "beta_inbox"),
            )])),
        };
        let merged = merge_reply_routing(existing, &incoming).expect("should merge");
        let channels = merged.reply_channels.unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels["alpha"].place_name, "alpha_inbox");
        assert_eq!(channels["beta"].place_name, "beta_inbox");
    }

    #[test]
    fn merge_reply_routing_conflicting_channel() {
        let existing = ReplyRouting {
            reply_to: None,
            reply_channels: Some(HashMap::from([(
                "result".to_string(),
                addr("net-a", "inbox_a"),
            )])),
        };
        let incoming = ReplyRouting {
            reply_to: None,
            reply_channels: Some(HashMap::from([
                ("result".to_string(), addr("net-b", "inbox_b")), // different address
            ])),
        };
        assert!(
            merge_reply_routing(existing, &incoming).is_none(),
            "conflicting channel key should fail"
        );
    }

    #[test]
    fn merge_reply_routing_conflicting_reply_to() {
        let existing = ReplyRouting {
            reply_to: Some(addr("net-a", "reply_a")),
            reply_channels: None,
        };
        let incoming = ReplyRouting {
            reply_to: Some(addr("net-b", "reply_b")),
            reply_channels: None,
        };
        assert!(
            merge_reply_routing(existing, &incoming).is_none(),
            "conflicting reply_to should fail"
        );
    }

    #[test]
    fn merge_reply_routing_identical_reply_to() {
        let existing = ReplyRouting {
            reply_to: Some(addr("net-a", "reply_inbox")),
            reply_channels: None,
        };
        let incoming = ReplyRouting {
            reply_to: Some(addr("net-a", "reply_inbox")),
            reply_channels: None,
        };
        let merged = merge_reply_routing(existing, &incoming).expect("identical should merge");
        assert_eq!(merged.reply_to.unwrap().place_name, "reply_inbox");
    }

    // ── Indexed equi-join binding ──────────────────────────────────────

    /// Reference binder: the full cross-product, used to prove the indexed
    /// fast path is selection-equivalent (identical first binding).
    fn brute_force(
        executor: &TransitionExecutor,
        transition: &Transition,
        input_arcs: &[&PetriArc],
        marking: &Marking,
    ) -> Option<TokenBinding> {
        let arc_tokens: Vec<Vec<&Token>> = input_arcs
            .iter()
            .map(|arc| marking.tokens_at(&arc.place_id).iter().collect())
            .collect();
        let sizes: Vec<usize> = arc_tokens.iter().map(|t| t.len()).collect();
        if sizes.iter().any(|&s| s == 0) {
            return None;
        }
        for indices in CombinationIterator::new(sizes) {
            if let Some(binding) =
                build_binding_for_indices(transition, input_arcs, &arc_tokens, &indices, None)
            {
                if let Some(guard) = &transition.guard {
                    match executor.evaluate_guard(guard, &binding.port_inputs) {
                        Ok(true) => return Some(binding),
                        _ => continue,
                    }
                } else {
                    return Some(binding);
                }
            }
        }
        None
    }

    /// Build a 2-input guarded transition + its arcs over two places.
    fn guarded_pair(
        guard: &str,
    ) -> (TransitionExecutor, Transition, PlaceId, PlaceId, PetriArc, PetriArc) {
        let executor = TransitionExecutor::new();
        let pa = PlaceId::named("place_a");
        let pb = PlaceId::named("place_b");
        let t_id = TransitionId::named("join");
        let mut t = transition_with_ports(vec![Port::new("a"), Port::new("b")]);
        t.guard = Some(guard.to_string());
        let arc_a = PetriArc::input(pa.clone(), t_id.clone(), "a");
        let arc_b = PetriArc::input(pb.clone(), t_id, "b");
        (executor, t, pa, pb, arc_a, arc_b)
    }

    fn consumed_ids(b: &TokenBinding) -> Vec<TokenId> {
        b.consumed_tokens.iter().map(|(_, id)| id.clone()).collect()
    }

    #[test]
    fn indexed_join_matches_on_key_and_equals_bruteforce() {
        let (executor, t, pa, pb, arc_a, arc_b) = guarded_pair("a.k == b.k");
        let mut marking = Marking::new();
        // a: keys 1,2,3   b: keys 3,2,1  → first lex match is a[0](k1) with b[2](k1)
        for k in [1, 2, 3] {
            marking.add_token(pa.clone(), data_token(json!({ "k": k })));
        }
        for k in [3, 2, 1] {
            marking.add_token(pb.clone(), data_token(json!({ "k": k })));
        }
        let arcs: Vec<&PetriArc> = vec![&arc_a, &arc_b];

        let indexed = find_valid_binding(&executor, &t, &arcs, &marking, None).expect("binds");
        let brute = brute_force(&executor, &t, &arcs, &marking).expect("brute binds");

        assert_eq!(indexed.port_inputs["a"]["k"], 1);
        assert_eq!(indexed.port_inputs["b"]["k"], 1);
        // Selection-equivalent: identical tokens consumed, identical order.
        assert_eq!(consumed_ids(&indexed), consumed_ids(&brute));
    }

    #[test]
    fn indexed_join_no_match_returns_none() {
        let (executor, t, pa, pb, arc_a, arc_b) = guarded_pair("a.k == b.k");
        let mut marking = Marking::new();
        marking.add_token(pa.clone(), data_token(json!({ "k": 1 })));
        marking.add_token(pb.clone(), data_token(json!({ "k": 2 })));
        let arcs: Vec<&PetriArc> = vec![&arc_a, &arc_b];
        assert!(find_valid_binding(&executor, &t, &arcs, &marking, None).is_none());
        assert!(brute_force(&executor, &t, &arcs, &marking).is_none());
    }

    #[test]
    fn indexed_join_with_extra_predicate_filters_via_guard() {
        // Index prunes to key matches; the guard's `>` still filters.
        let (executor, t, pa, pb, arc_a, arc_b) = guarded_pair("a.k == b.k && a.v > b.v");
        let mut marking = Marking::new();
        // a: (k1,v5)(k1,v9)   b: (k1,v7)  → a[0] fails v>7, a[1] passes
        marking.add_token(pa.clone(), data_token(json!({ "k": 1, "v": 5 })));
        marking.add_token(pa.clone(), data_token(json!({ "k": 1, "v": 9 })));
        marking.add_token(pb.clone(), data_token(json!({ "k": 1, "v": 7 })));
        let arcs: Vec<&PetriArc> = vec![&arc_a, &arc_b];

        let indexed = find_valid_binding(&executor, &t, &arcs, &marking, None).expect("binds");
        let brute = brute_force(&executor, &t, &arcs, &marking).expect("brute binds");
        assert_eq!(indexed.port_inputs["a"]["v"], 9);
        assert_eq!(consumed_ids(&indexed), consumed_ids(&brute));
    }

    #[test]
    fn string_keyed_join_like_grant_id() {
        // Mirrors t_release: req.grant_id == held.grant_id over many holds.
        let (executor, t, pa, pb, arc_a, arc_b) = guarded_pair("a.grant_id == b.grant_id");
        let mut marking = Marking::new();
        for id in ["g0", "g1", "g2", "g3", "g4"] {
            marking.add_token(pb.clone(), data_token(json!({ "grant_id": id })));
        }
        // single release targeting g3
        marking.add_token(pa.clone(), data_token(json!({ "grant_id": "g3" })));
        let arcs: Vec<&PetriArc> = vec![&arc_a, &arc_b];

        let indexed = find_valid_binding(&executor, &t, &arcs, &marking, None).expect("binds");
        let brute = brute_force(&executor, &t, &arcs, &marking).expect("brute binds");
        assert_eq!(indexed.port_inputs["b"]["grant_id"], "g3");
        assert_eq!(consumed_ids(&indexed), consumed_ids(&brute));
    }

    #[test]
    fn numeric_int_float_equality_not_split() {
        // Guard equality coerces numerics; 3 (int) and 3.0 (float) must join.
        let (executor, t, pa, pb, arc_a, arc_b) = guarded_pair("a.k == b.k");
        let mut marking = Marking::new();
        marking.add_token(pa.clone(), data_token(json!({ "k": 3 })));
        marking.add_token(pb.clone(), data_token(json!({ "k": 3.0 })));
        let arcs: Vec<&PetriArc> = vec![&arc_a, &arc_b];
        // If the engine's guard considers them equal, the indexed path must too
        // (must not bucket them apart). Cross-product is the oracle.
        let indexed = find_valid_binding(&executor, &t, &arcs, &marking, None);
        let brute = brute_force(&executor, &t, &arcs, &marking);
        assert_eq!(
            indexed.map(|b| consumed_ids(&b)),
            brute.map(|b| consumed_ids(&b)),
        );
    }

    #[test]
    fn three_way_chain_join() {
        // p0.key == p1.key && p1.key == p2.key — the bench `match` shape.
        let executor = TransitionExecutor::new();
        let (p0, p1, p2) = (
            PlaceId::named("p0"),
            PlaceId::named("p1"),
            PlaceId::named("p2"),
        );
        let t_id = TransitionId::named("j3");
        let mut t = transition_with_ports(vec![Port::new("p0"), Port::new("p1"), Port::new("p2")]);
        t.guard = Some("p0.key == p1.key && p1.key == p2.key".to_string());
        let arc0 = PetriArc::input(p0.clone(), t_id.clone(), "p0");
        let arc1 = PetriArc::input(p1.clone(), t_id.clone(), "p1");
        let arc2 = PetriArc::input(p2.clone(), t_id, "p2");

        let mut marking = Marking::new();
        for k in 0..4 {
            marking.add_token(p0.clone(), data_token(json!({ "key": k })));
            marking.add_token(p1.clone(), data_token(json!({ "key": k })));
            marking.add_token(p2.clone(), data_token(json!({ "key": k })));
        }
        let arcs: Vec<&PetriArc> = vec![&arc0, &arc1, &arc2];

        let indexed = find_valid_binding(&executor, &t, &arcs, &marking, None).expect("binds");
        let brute = brute_force(&executor, &t, &arcs, &marking).expect("brute binds");
        assert_eq!(indexed.port_inputs["p0"]["key"], 0);
        assert_eq!(consumed_ids(&indexed), consumed_ids(&brute));
    }

    #[test]
    fn non_equi_guard_falls_back_to_crossproduct() {
        // `a.v > b.v` extracts no equi-join → cross-product path; still correct.
        let (executor, t, pa, pb, arc_a, arc_b) = guarded_pair("a.v > b.v");
        let mut marking = Marking::new();
        marking.add_token(pa.clone(), data_token(json!({ "v": 1 })));
        marking.add_token(pa.clone(), data_token(json!({ "v": 9 })));
        marking.add_token(pb.clone(), data_token(json!({ "v": 5 })));
        let arcs: Vec<&PetriArc> = vec![&arc_a, &arc_b];

        let indexed = find_valid_binding(&executor, &t, &arcs, &marking, None).expect("binds");
        let brute = brute_force(&executor, &t, &arcs, &marking).expect("brute binds");
        assert_eq!(indexed.port_inputs["a"]["v"], 9);
        assert_eq!(consumed_ids(&indexed), consumed_ids(&brute));
    }
}
