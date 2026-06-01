//! Per-run dispatch options carried alongside a scenario load.
//!
//! Sub-phase 2.5e-γ.mekhan additive surface: clinic → cloud-layer-workflow →
//! mekhan forwards `skip_mask` (ablation: skip transitions at evaluate-time)
//! and `stage_overrides` (RFC 7396 JSON merge-patch on transition
//! `effect_config` at fire-time) so the research-harness ablation study can
//! probe alternate pipeline behaviours without authoring per-ablation
//! scenario variants.
//!
//! Both fields default to empty; an unconfigured scenario behaves identically
//! to the pre-γ.mekhan baseline.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Per-run dispatch options. Owned by `PetriNetService` (per-`NetInstance`
/// scope — concurrent runs of distinct nets carry independent options).
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DispatchOptions {
    /// Transition IDs to skip at evaluate-time. Each entry MUST reference a
    /// declared transition in the loaded scenario; unknown IDs fail-closed at
    /// load time. Skipped transitions emit a `DomainEvent::TransitionSkipped`
    /// event and write `Token::new_unit()` to each declared output port
    /// place so downstream transitions proceed (or fail their own input
    /// validation, which is the honest outcome for ablation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_mask: Vec<String>,
    /// Per-transition JSON merge-patch (RFC 7396) overrides keyed by
    /// transition_id. At fire-time, the override is merged into the
    /// transition's `effect_config` BEFORE secret resolution + pre-dispatch
    /// hook enrichment. Unknown transition_ids fail-closed at load time.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub stage_overrides: HashMap<String, serde_json::Value>,
}

/// Apply an RFC 7396 JSON merge-patch (https://www.rfc-editor.org/rfc/rfc7396)
/// to a base value. Semantics:
///
/// - If `patch` is not an object, it replaces `base` entirely.
/// - If `patch` is an object:
///   - For each key in `patch`:
///     - If the patch value is `null`, the key is removed from `base`.
///     - Otherwise, the value is merged recursively (if both base[key] and
///       patch[key] are objects) or replaced.
///
/// `base` is mutated in place. The implementation is hand-rolled (~30 LOC)
/// to avoid adding a transitive crate dependency for a well-defined RFC.
pub fn apply_merge_patch(base: &mut serde_json::Value, patch: &serde_json::Value) {
    use serde_json::Value;
    match patch {
        Value::Object(patch_map) => {
            if !base.is_object() {
                *base = Value::Object(serde_json::Map::new());
            }
            let base_map = base.as_object_mut().expect("base is object after coercion");
            for (key, patch_value) in patch_map {
                if patch_value.is_null() {
                    base_map.remove(key);
                } else {
                    match base_map.get_mut(key) {
                        Some(existing) => apply_merge_patch(existing, patch_value),
                        None => {
                            base_map.insert(key.clone(), patch_value.clone());
                        }
                    }
                }
            }
        }
        _ => {
            *base = patch.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_patch_object_overlay_replaces_scalar_leaves() {
        let mut base = json!({"a": 1, "b": "old"});
        let patch = json!({"b": "new"});
        apply_merge_patch(&mut base, &patch);
        assert_eq!(base, json!({"a": 1, "b": "new"}));
    }

    #[test]
    fn merge_patch_null_value_deletes_key() {
        let mut base = json!({"a": 1, "b": 2});
        let patch = json!({"b": null});
        apply_merge_patch(&mut base, &patch);
        assert_eq!(base, json!({"a": 1}));
    }

    #[test]
    fn merge_patch_nested_objects_merge_recursively() {
        let mut base = json!({"a": {"b": 1, "c": 2}, "d": 3});
        let patch = json!({"a": {"c": 99}});
        apply_merge_patch(&mut base, &patch);
        assert_eq!(base, json!({"a": {"b": 1, "c": 99}, "d": 3}));
    }

    #[test]
    fn merge_patch_array_value_replaces_wholesale() {
        // RFC 7396 §1: arrays are not merged element-wise; the new array
        // replaces the old array entirely.
        let mut base = json!({"items": [1, 2, 3]});
        let patch = json!({"items": [9]});
        apply_merge_patch(&mut base, &patch);
        assert_eq!(base, json!({"items": [9]}));
    }

    #[test]
    fn merge_patch_non_object_patch_replaces_base() {
        let mut base = json!({"a": 1});
        let patch = json!("scalar");
        apply_merge_patch(&mut base, &patch);
        assert_eq!(base, json!("scalar"));
    }

    #[test]
    fn merge_patch_new_key_is_added() {
        let mut base = json!({"a": 1});
        let patch = json!({"b": 2});
        apply_merge_patch(&mut base, &patch);
        assert_eq!(base, json!({"a": 1, "b": 2}));
    }

    #[test]
    fn merge_patch_deeply_nested_overlay_preserves_unrelated_branches() {
        let mut base = json!({
            "model_config": {
                "model": "test-model-a",
                "temperature": 0.7,
                "tools": ["tool_a", "tool_b"],
            },
            "retry": {"max_attempts": 3, "backoff_ms": 100},
        });
        let patch = json!({
            "model_config": {"temperature": 0.0},
        });
        apply_merge_patch(&mut base, &patch);
        assert_eq!(
            base,
            json!({
                "model_config": {
                    "model": "test-model-a",
                    "temperature": 0.0,
                    "tools": ["tool_a", "tool_b"],
                },
                "retry": {"max_attempts": 3, "backoff_ms": 100},
            })
        );
    }
}
