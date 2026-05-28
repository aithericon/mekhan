//! Compile-time resource-type registry.
//!
//! Built-in resource structs in [`crate::types`] derive `ResourceType`, which
//! emits an `inventory::submit!` block. At binary link time, `inventory`
//! collects all submissions into a single iterable. [`all`] yields them in
//! deterministic order (sorted by `name`) so handlers, tests, and the OpenAPI
//! introspection endpoint see a stable result regardless of link order.
//!
//! ## Why compile-time
//!
//! The registry is the single source of truth that drives:
//! - the OpenAPI `/api/resources/types` endpoint (B.9),
//! - the workflow-level alias validator (`compile_to_air` in B.6),
//! - and the frontend `ResourcePicker` (B.10).
//!
//! Doing it at compile time means a missing `#[derive(ResourceType)]` is a
//! visible omission in the binary, not a silent runtime gap.

use std::sync::OnceLock;

use serde_json::Value as JsonValue;

/// Compile-time descriptor for a resource type. One instance per
/// `#[derive(ResourceType)]` struct; collected by [`inventory`].
///
/// All fields are `'static` so the descriptor can be submitted from a const
/// context. The `schema_json` field is a function pointer rather than a value
/// so the (potentially expensive) `schemars` walk runs lazily on first use.
pub struct ResourceTypeDescriptor {
    /// Stable wire identifier — stored in `resources.resource_type`.
    /// Changing this for a released type is a breaking DB migration.
    pub name: &'static str,
    /// UI label, defaults to `name` when the derive doesn't set it.
    pub display_name: &'static str,
    /// Lucide-style icon hint for the picker; empty string when unset.
    pub icon: &'static str,
    /// OAuth provider key (e.g., `"google"`) for OAuth-managed types.
    /// `None` for credential types managed via the standard CRUD flow.
    pub oauth_provider: Option<&'static str>,
    /// Names of fields written to Vault. Order matches struct declaration.
    pub secret_fields: &'static [&'static str],
    /// Names of fields stored as `resource_versions.public_config`. Order
    /// matches struct declaration.
    pub public_fields: &'static [&'static str],
    /// Lazy schema producer — invoked the first time the OpenAPI handler or a
    /// picker render asks for the JSON Schema. Result is cached in
    /// [`schema_json_cached`].
    pub schema_json: fn() -> JsonValue,
    /// `true` when the field set is per-INSTANCE rather than per-TYPE — the
    /// `kv` escape hatch. With `dynamic_fields: true` the CRUD handler
    /// accepts any string-keyed config map, treats every value as a secret,
    /// and records the user-supplied key list in `public_config.__kv_keys`
    /// so the picker + resolver can iterate them at runtime. Typed
    /// resources (Postgres, OpenAI, etc.) set this to `false`.
    pub dynamic_fields: bool,
}

inventory::collect!(ResourceTypeDescriptor);

/// Borrowed view of every registered resource type, sorted by `name`.
///
/// Returns a sorted slice over leaked memory; subsequent calls return the
/// same slice. The leak is bounded (one slot per built-in type, ~5 today)
/// and we accept it as the price of a `'static`-friendly API.
pub fn all() -> &'static [&'static ResourceTypeDescriptor] {
    static SORTED: OnceLock<&'static [&'static ResourceTypeDescriptor]> = OnceLock::new();
    SORTED.get_or_init(|| {
        let mut v: Vec<&'static ResourceTypeDescriptor> =
            inventory::iter::<ResourceTypeDescriptor>().collect();
        v.sort_by_key(|d| d.name);
        // Move into a `Box<[…]>` and leak so we can hand out a `'static` slice.
        let boxed: Box<[&'static ResourceTypeDescriptor]> = v.into_boxed_slice();
        Box::leak(boxed)
    })
}

/// Look up a descriptor by its `name`. Returns `None` if the registry has no
/// matching type. Workflow compilation uses this to surface user-typed
/// alias-targets that point at unknown types.
pub fn lookup(name: &str) -> Option<&'static ResourceTypeDescriptor> {
    all().iter().copied().find(|d| d.name == name)
}

/// Lazily compute and cache the JSON Schema for a descriptor. The first call
/// per descriptor invokes `schemars`; subsequent calls return the cached
/// value. Implemented as a free function rather than a method so the
/// `'static` descriptor remains POD.
///
/// The cache is keyed by `name` because descriptors are deduplicated by name
/// at the `all()` boundary.
pub fn schema_json_cached(descriptor: &ResourceTypeDescriptor) -> &'static JsonValue {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: OnceLock<Mutex<HashMap<&'static str, &'static JsonValue>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().expect("schema cache mutex poisoned");
    if let Some(v) = guard.get(descriptor.name) {
        return v;
    }
    let computed = (descriptor.schema_json)();
    let leaked: &'static JsonValue = Box::leak(Box::new(computed));
    guard.insert(descriptor.name, leaked);
    leaked
}
