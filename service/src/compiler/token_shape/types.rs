use std::collections::BTreeMap;

use serde_json::Value;

use crate::models::template::{FieldKind, WorkflowNode}; // ─── Structural token type ──────────────────────────────────────────────────

/// Leaf type of a token field. Deliberately small — the point is to model
/// *where a value lives and roughly what it is*, not a full JSON Schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarTy {
    String,
    Number,
    Bool,
    /// Catalogue file reference — at runtime an object `{ url, filename,
    /// content_type, .. }`, but commonly addressed as a scalar handle.
    FileRef,
    Timestamp,
    /// Opaque / dynamic — the `Json` escape-hatch field kind.
    Json,
}

impl ScalarTy {
    pub(super) fn from_kind(k: FieldKind) -> ScalarTy {
        match k {
            FieldKind::Text | FieldKind::Textarea | FieldKind::Select | FieldKind::Signature => {
                ScalarTy::String
            }
            FieldKind::Number => ScalarTy::Number,
            FieldKind::Bool => ScalarTy::Bool,
            FieldKind::File => ScalarTy::FileRef,
            FieldKind::Timestamp => ScalarTy::Timestamp,
            // Container markers have no scalar equivalent — `port_to_shape`
            // models them as `TokenShape::Object`/`Array` (or `Schema` with a
            // `.schema` override) and never reaches here. TaskFieldKind (the
            // Repeater path) can't produce these. Treat as the opaque escape
            // hatch defensively rather than panicking.
            FieldKind::Json | FieldKind::Object | FieldKind::Array => ScalarTy::Json,
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            ScalarTy::String => "String",
            ScalarTy::Number => "Number",
            ScalarTy::Bool => "Bool",
            ScalarTy::FileRef => "FileRef",
            ScalarTy::Timestamp => "Timestamp",
            ScalarTy::Json => "Json",
        }
    }
}

/// Structural shape of a token (or sub-tree of one).
#[derive(Debug, Clone)]
pub enum TokenShape {
    Object(BTreeMap<String, Field>),
    Array(Box<TokenShape>),
    Scalar(ScalarTy),
    /// Unconstrained — an empty declared port ("accepts any token").
    Any,
    /// A modelled-but-deliberately-unexpanded envelope internal (e.g. an
    /// executor's `detail`: real, typed elsewhere, but not worth reproducing
    /// here). Carries a human note.
    Opaque(String),
    /// A leaf carrying an explicit JSON Schema — the field declared a rich
    /// `schema` override (`PortField::schema`) too structured for the flat
    /// scalar vocabulary.
    ///
    /// Two faces, deliberately kept distinct:
    /// - `raw` is the author's JSON Schema *verbatim*. [`Self::to_json_schema`]
    ///   emits it unchanged so the runtime `SchemaRegistry` enforces the full
    ///   set of constraints (`required`, `enum`, `min`/`max`, …) on the
    ///   produced value. This must NEVER be re-derived from `structural`.
    /// - `structural` is a parsed *shadow* (see
    ///   [`super::schema_parse::json_schema_to_token_shape`]) used purely for
    ///   the read-side: display (`kind_label`/`render`), the picker descriptor
    ///   (`collect_scope_tree`), and the borrow resolver's path-walking
    ///   (`resolve`/`find_by_leaf`). It makes schema-backed ports *drillable*
    ///   without touching what the runtime enforces. Unparseable constructs in
    ///   `raw` (combinators, missing type) shadow as [`Self::Any`].
    Schema {
        raw: Box<Value>,
        structural: Box<TokenShape>,
    },
}

/// A field of an [`TokenShape::Object`]: its shape plus *where it came from*.
#[derive(Debug, Clone)]
pub struct Field {
    pub shape: TokenShape,
    pub prov: Provenance,
}

/// Why/where a field exists — the thing the flat model throws away.
#[derive(Debug, Clone)]
pub struct Provenance {
    pub node_id: String,
    pub node_label: String,
    pub note: String,
    /// Set when the field is *both* a recursable container AND a usable
    /// scalar leaf of the given type — currently only File envelopes (the
    /// declared port-level `FieldKind::File`, which is a `FileRef` handle
    /// in its own right *and* exposes `{url, filename, content_type}`
    /// subkeys at runtime). `collect_leaves` emits the container path with
    /// this scalar type *and* continues into the children.
    pub anchor: Option<ScalarTy>,
}

impl Provenance {
    pub(super) fn new(node: &WorkflowNode, note: impl Into<String>) -> Provenance {
        Provenance {
            node_id: node.id.clone(),
            node_label: node.data.label().to_string(),
            note: note.into(),
            anchor: None,
        }
    }

    pub(super) fn with_anchor(mut self, anchor: ScalarTy) -> Provenance {
        self.anchor = Some(anchor);
        self
    }
}

/// Provenance attached to the *interior* fields of a JSON-Schema structural
/// shadow (see [`super::schema_parse`]). The schema-backed field itself carries
/// the real call-site provenance (which node/port declared it); its nested
/// children only need a neutral marker so they render with context if a caller
/// surfaces them directly. No node identity, so `node_id`/`node_label` are
/// empty and there is no anchor.
pub(super) fn structural_provenance() -> Provenance {
    Provenance {
        node_id: String::new(),
        node_label: String::new(),
        note: "JSON Schema field".to_string(),
        anchor: None,
    }
}

impl TokenShape {
    pub(super) fn object() -> TokenShape {
        TokenShape::Object(BTreeMap::new())
    }

    pub(super) fn insert(&mut self, key: &str, shape: TokenShape, prov: Provenance) {
        if let TokenShape::Object(map) = self {
            map.insert(key.to_string(), Field { shape, prov });
        }
    }

    /// Shallow last-wins merge of `other` into `self` — mirrors the runtime
    /// `for k in signal.keys() { result[k] = signal[k] }` and the
    /// `ShallowLastWins` join. `DeepMerge` recurses on nested objects.
    pub(super) fn merge_from(&mut self, other: &TokenShape, deep: bool) {
        match (self, other) {
            (TokenShape::Object(a), TokenShape::Object(b)) => {
                for (k, vf) in b {
                    match (deep, a.get_mut(k)) {
                        (true, Some(existing))
                            if matches!(existing.shape, TokenShape::Object(_))
                                && matches!(vf.shape, TokenShape::Object(_)) =>
                        {
                            existing.shape.merge_from(&vf.shape, true);
                        }
                        _ => {
                            a.insert(k.clone(), vf.clone());
                        }
                    }
                }
            }
            (slot, other) => {
                // Non-object on either side: last value wins (runtime parity).
                *slot = other.clone();
            }
        }
    }

    /// Resolve a dotted path (the segments *after* `input`). Returns the
    /// matched field's shape + provenance, or `None` if any segment is absent.
    pub(crate) fn resolve<'a>(
        &'a self,
        segs: &[String],
    ) -> Option<(&'a TokenShape, Option<&'a Provenance>)> {
        let mut cur = self;
        let mut prov: Option<&Provenance> = None;
        for seg in segs {
            // A schema-backed leaf is transparent for path-walking: delegate
            // into its structural shadow so nested refs resolve exactly as if
            // it were a plain Object.
            if let TokenShape::Schema { structural, .. } = cur {
                cur = structural.as_ref();
            }
            match cur {
                TokenShape::Object(map) => {
                    let f = map.get(seg)?;
                    cur = &f.shape;
                    prov = Some(&f.prov);
                }
                // Can't walk into a scalar/opaque/any/array by key.
                _ => return None,
            }
        }
        // If the final landing spot is itself a schema-backed leaf, hand back
        // its structural shadow so callers see the real shape, not "Schema".
        if let TokenShape::Schema { structural, .. } = cur {
            cur = structural.as_ref();
        }
        Some((cur, prov))
    }

    /// Depth-first search for any leaf whose *final* path segment equals
    /// `name`. Used to suggest "did you mean …" when a guard ref is
    /// unresolved. Returns (dotted_path, scalar/shape label, provenance).
    pub(crate) fn find_by_leaf(&self, name: &str) -> Option<(String, String, Provenance)> {
        fn walk(
            shape: &TokenShape,
            prefix: &str,
            name: &str,
        ) -> Option<(String, String, Provenance)> {
            // Schema-backed leaves are transparent: search their structural
            // shadow as if it were the object directly in place.
            let shape = match shape {
                TokenShape::Schema { structural, .. } => structural.as_ref(),
                other => other,
            };
            if let TokenShape::Object(map) = shape {
                for (k, f) in map {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    if k == name {
                        return Some((path, f.shape.kind_label(), f.prov.clone()));
                    }
                    if let Some(hit) = walk(&f.shape, &path, name) {
                        return Some(hit);
                    }
                }
            }
            None
        }
        walk(self, "", name)
    }

    pub(crate) fn kind_label(&self) -> String {
        match self {
            TokenShape::Object(_) => "Object".to_string(),
            TokenShape::Array(_) => "Array".to_string(),
            TokenShape::Scalar(s) => s.label().to_string(),
            TokenShape::Any => "Any".to_string(),
            TokenShape::Opaque(n) => format!("Opaque({n})"),
            // Display the real (structural) shape, not "Schema".
            TokenShape::Schema { structural, .. } => structural.kind_label(),
        }
    }

    /// Structural shape → JSON Schema for the engine's `SchemaRegistry`.
    ///
    /// Known fields get a type constraint; `additionalProperties` stays open
    /// and nothing is `required`, so the schema *validates the shape we know*
    /// without rejecting extra/optional keys (the executor envelope, metadata
    /// the lowering stamps on, etc.). `Opaque`/`Any`/`FileRef` are permissive
    /// `{}` — undeclared executor outputs and catalogue refs must not be
    /// rejected (the declared→enforced ramp tightens these later).
    ///
    /// Scalar types also accept `null` — the executor backends (LLM,
    /// Python) legitimately set declared-but-unset optional outputs to
    /// `null` so downstream consumers (Python `<slug>.<field>` access)
    /// see `None` instead of `AttributeError`. Without the `null`
    /// alternative, the engine's `t_<id>_yield` schema validator rejects
    /// the parked envelope on the first nullable scalar and the whole
    /// instance fails.
    pub fn to_json_schema(&self) -> Value {
        match self {
            TokenShape::Object(map) => {
                let mut props = serde_json::Map::new();
                for (k, f) in map {
                    props.insert(k.clone(), f.shape.to_json_schema());
                }
                serde_json::json!({
                    "type": "object",
                    "properties": Value::Object(props),
                    "additionalProperties": true
                })
            }
            TokenShape::Array(inner) => serde_json::json!({
                "type": "array",
                "items": inner.to_json_schema()
            }),
            TokenShape::Scalar(ScalarTy::Number) => {
                serde_json::json!({ "type": ["number", "null"] })
            }
            TokenShape::Scalar(ScalarTy::Bool) => {
                serde_json::json!({ "type": ["boolean", "null"] })
            }
            TokenShape::Scalar(ScalarTy::String) | TokenShape::Scalar(ScalarTy::Timestamp) => {
                serde_json::json!({ "type": ["string", "null"] })
            }
            // FileRef is a catalogue handle (string or object at runtime),
            // Json/Any/Opaque are deliberately unconstrained.
            TokenShape::Scalar(ScalarTy::FileRef)
            | TokenShape::Scalar(ScalarTy::Json)
            | TokenShape::Any
            | TokenShape::Opaque(_) => serde_json::json!({}),
            // Explicit override: the *raw* schema IS the constraint the runtime
            // `SchemaRegistry` enforces — emitted verbatim. The `structural`
            // shadow is read-side only and never feeds runtime enforcement.
            TokenShape::Schema { raw, .. } => (**raw).clone(),
        }
    }

    /// Pretty multi-line render for the demo report.
    pub(super) fn render(&self, indent: usize) -> String {
        let pad = "  ".repeat(indent);
        match self {
            TokenShape::Object(map) if map.is_empty() => "{}".to_string(),
            TokenShape::Object(map) => {
                let mut s = String::from("{\n");
                for (k, f) in map {
                    s.push_str(&format!(
                        "{pad}  {k}: {}{}\n",
                        f.shape.render(indent + 1),
                        format_args!("   « {} »", f.prov.note),
                    ));
                }
                s.push_str(&format!("{pad}}}"));
                s
            }
            TokenShape::Array(inner) => format!("[{}]", inner.render(indent)),
            TokenShape::Scalar(t) => t.label().to_string(),
            TokenShape::Any => "Any".to_string(),
            TokenShape::Opaque(n) => format!("Opaque<{n}>"),
            // Render the real (structural) shape, not "Schema".
            TokenShape::Schema { structural, .. } => structural.render(indent),
        }
    }
}

// Shared schema-definition vocabulary so lowering (WS2) and the read-arc
// synthesis phase (WS3) agree on `#/definitions/*` names. Node ids can contain
// `-` (e.g. `check-amount`); JSON-pointer definition keys allow it.

/// Definition name for a data-yielding node's parked data token.
pub fn data_def_name(node_id: &str) -> String {
    format!("Data__{node_id}")
}

/// Definition name for a node's slim control token.
pub fn ctrl_def_name(node_id: &str) -> String {
    format!("Ctrl__{node_id}")
}

/// `#/definitions/<name>` ref for a definition name.
pub fn def_ref(name: &str) -> String {
    format!("#/definitions/{name}")
}

/// Permissive catch-all definition every non-split place/port keeps using so
/// the `SchemaRegistry` resolves their `#/definitions/DynamicToken` ref
/// (unresolvable refs *fail* validation) while constraining nothing.
pub fn dynamic_token_definition() -> (String, Value) {
    ("DynamicToken".to_string(), serde_json::json!({}))
}

// ─── Picker-facing recursive type descriptor (Feature B) ────────────────────

/// Wire-shaped recursive descriptor of a producer field's type — the picker
/// walks this to render nested fields and array element shapes. Mirrors
/// [`TokenShape`] but flattened to a serializable form. Plain (non-anchored)
/// Object containers carry `selectable: false`: the picker may expand them
/// but the row body acts as a toggle, not an emit (the user must drill into
/// scalar / file / array leaves). File-anchored containers carry
/// `selectable: true`, preserving the existing precedent that `document` is
/// pickable as a whole **and** `document.url` etc. are individually pickable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schema(no_recursion)]
pub enum TyDescriptor {
    Scalar {
        name: String,
    },
    Object {
        fields: BTreeMap<String, TyDescriptor>,
        selectable: bool,
    },
    Array {
        element: Box<TyDescriptor>,
    },
    Any,
    Opaque {
        name: String,
    },
}

impl TyDescriptor {
    /// Legacy `kind_label`-compatible string for callers that still want a
    /// single label (Python `.pyi` overlay, diagnostics rendering). Matches
    /// [`TokenShape::kind_label`] verbatim so `ty_label_to_field_kind` keeps
    /// working unchanged.
    pub fn kind_label(&self) -> String {
        match self {
            TyDescriptor::Object { .. } => "Object".to_string(),
            TyDescriptor::Array { .. } => "Array".to_string(),
            TyDescriptor::Scalar { name } => name.clone(),
            TyDescriptor::Any => "Any".to_string(),
            TyDescriptor::Opaque { name } => format!("Opaque({name})"),
        }
    }
}
