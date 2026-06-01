//! # `#[derive(ResourceType)]`
//!
//! Compile-time derivation for [`aithericon-resources`] built-in resource
//! types. The derive walks the struct's fields, partitions them into
//! `secret_fields` / `public_fields` based on the per-field
//! `#[resource(secret)]` attribute, and emits an `inventory::submit!` block
//! that registers a `ResourceTypeDescriptor` at link time.
//!
//! ## Author surface
//!
//! ```ignore
//! use aithericon_resources::ResourceType;
//! use serde::{Deserialize, Serialize};
//! use schemars::JsonSchema;
//!
//! #[derive(ResourceType, Serialize, Deserialize, JsonSchema)]
//! #[resource(name = "postgres", display_name = "Postgres", icon = "lucide-database")]
//! pub struct Postgres {
//!     pub host: String,
//!     pub port: u16,
//!     pub database: String,
//!     pub username: String,
//!     #[resource(secret)]
//!     pub password: String,
//!     #[serde(default)]
//!     pub sslmode: Option<String>,
//! }
//! ```
//!
//! ## Attributes recognized
//!
//! Struct-level `#[resource(...)]`:
//! - `name = "..."` (required) — stable wire identifier; written to the DB.
//! - `display_name = "..."` (optional) — UI label; defaults to `name`.
//! - `icon = "..."` (optional) — lucide-style icon hint; defaults to `""`.
//! - `oauth_provider = "..."` (optional) — when present, marks the type as
//!   OAuth-managed; consumed by the OAuth handler in B.11.
//!
//! Field-level `#[resource(secret)]` — marks the field as a Vault-stored
//! secret. Field name is captured verbatim (no rename mapping in v1).

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, LitStr};

/// Derive `ResourceType` registration for a struct.
#[proc_macro_derive(ResourceType, attributes(resource))]
pub fn derive_resource_type(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_ident = &input.ident;

    // --- Parse struct-level #[resource(...)] -------------------------------
    let mut name: Option<String> = None;
    let mut display_name: Option<String> = None;
    let mut icon: Option<String> = None;
    let mut oauth_provider: Option<String> = None;
    // For discriminated (internally-tagged-enum) resources: the serde tag field
    // name (e.g. `scheduler_flavor`). serde injects it at the top level; it has
    // no struct field, so the derive must be told its name to list it as public.
    let mut tag: Option<String> = None;

    for attr in &input.attrs {
        if !attr.path().is_ident("resource") {
            continue;
        }
        let result = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let lit: LitStr = meta.value()?.parse()?;
                name = Some(lit.value());
            } else if meta.path.is_ident("display_name") {
                let lit: LitStr = meta.value()?.parse()?;
                display_name = Some(lit.value());
            } else if meta.path.is_ident("icon") {
                let lit: LitStr = meta.value()?.parse()?;
                icon = Some(lit.value());
            } else if meta.path.is_ident("oauth_provider") {
                let lit: LitStr = meta.value()?.parse()?;
                oauth_provider = Some(lit.value());
            } else if meta.path.is_ident("tag") {
                let lit: LitStr = meta.value()?.parse()?;
                tag = Some(lit.value());
            } else {
                return Err(meta.error(
                    "unknown #[resource(...)] key on struct: expected one of \
                     `name`, `display_name`, `icon`, `oauth_provider`, `tag`",
                ));
            }
            Ok(())
        });
        if let Err(e) = result {
            return e.to_compile_error().into();
        }
    }

    let Some(name) = name else {
        return syn::Error::new_spanned(
            struct_ident,
            "missing `#[resource(name = \"...\")]` — every ResourceType needs a stable wire name",
        )
        .to_compile_error()
        .into();
    };
    let display_name = display_name.unwrap_or_else(|| name.clone());
    let icon = icon.unwrap_or_default();
    let oauth_provider_tokens = match oauth_provider {
        Some(p) => quote! { ::core::option::Option::Some(#p) },
        None => quote! { ::core::option::Option::None },
    };

    // --- Walk fields, partition into secret / public -----------------------
    // Supports a plain struct OR an internally-tagged enum (a DISCRIMINATED
    // resource — one variant per flavor, e.g. a datacenter's slurm/nomad/http).
    // For the enum the field lists are the UNION across variants (deduped), with
    // the serde discriminator field (`#[resource(tag = "...")]`) listed first as
    // public. The wire JSON is flat (internally-tagged enums flatten variant
    // fields next to the tag), so storage / `split_config` are unchanged.
    let mut all_fields: Vec<&syn::Field> = Vec::new();
    let is_enum = match &input.data {
        Data::Struct(s) => {
            match &s.fields {
                Fields::Named(named) => all_fields.extend(named.named.iter()),
                Fields::Unit => {
                    return syn::Error::new_spanned(
                        struct_ident,
                        "ResourceType cannot be derived for unit structs — fields are required",
                    )
                    .to_compile_error()
                    .into();
                }
                Fields::Unnamed(_) => {
                    return syn::Error::new_spanned(
                        struct_ident,
                        "ResourceType requires named fields (tuple structs are not supported)",
                    )
                    .to_compile_error()
                    .into();
                }
            }
            false
        }
        Data::Enum(e) => {
            for variant in &e.variants {
                match &variant.fields {
                    Fields::Named(named) => all_fields.extend(named.named.iter()),
                    // A flavor with no fields beyond the tag is allowed.
                    Fields::Unit => {}
                    Fields::Unnamed(_) => {
                        return syn::Error::new_spanned(
                            variant,
                            "ResourceType enum variants require named fields",
                        )
                        .to_compile_error()
                        .into();
                    }
                }
            }
            true
        }
        _ => {
            return syn::Error::new_spanned(
                struct_ident,
                "ResourceType can only be derived for a struct or an internally-tagged enum",
            )
            .to_compile_error()
            .into();
        }
    };

    let mut secret_fields: Vec<String> = Vec::new();
    let mut public_fields: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    if is_enum {
        let Some(tag_name) = tag.clone() else {
            return syn::Error::new_spanned(
                struct_ident,
                "ResourceType on an enum requires #[resource(tag = \"...\")] naming the \
                 serde discriminator field (e.g. `tag = \"scheduler_flavor\"`)",
            )
            .to_compile_error()
            .into();
        };
        seen.insert(tag_name.clone());
        public_fields.push(tag_name);
    }

    for field in all_fields {
        let Some(ident) = &field.ident else {
            continue;
        };
        let field_name = ident.to_string();
        // Union across variants: count each field name once.
        if !seen.insert(field_name.clone()) {
            continue;
        }
        let mut is_secret = false;
        for attr in &field.attrs {
            if !attr.path().is_ident("resource") {
                continue;
            }
            // Accept both `#[resource(secret)]` and `#[resource(secret = true)]`
            // The simple `secret` flag form is the canonical surface.
            let res = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("secret") {
                    is_secret = true;
                    Ok(())
                } else {
                    Err(meta
                        .error("unknown #[resource(...)] key on field: only `secret` is supported"))
                }
            });
            if let Err(e) = res {
                return e.to_compile_error().into();
            }
        }
        if is_secret {
            secret_fields.push(field_name);
        } else {
            public_fields.push(field_name);
        }
    }

    // --- Emit the inventory submission ------------------------------------
    // We resolve the runtime types through the consumer crate's
    // `aithericon_resources::__macro_support` re-exports so that the derive
    // doesn't introduce its own dep on `inventory` at the caller's site.
    let expanded = quote! {
        const _: () = {
            ::aithericon_resources::__macro_support::inventory::submit! {
                ::aithericon_resources::registry::ResourceTypeDescriptor {
                    name: #name,
                    display_name: #display_name,
                    icon: #icon,
                    oauth_provider: #oauth_provider_tokens,
                    secret_fields: &[ #( #secret_fields ),* ],
                    public_fields: &[ #( #public_fields ),* ],
                    schema_json: || {
                        // Schema generation defers to schemars at runtime so we
                        // don't pull schemars through the proc-macro boundary.
                        let schema = ::aithericon_resources::__macro_support::schemars::schema_for!(#struct_ident);
                        ::aithericon_resources::__macro_support::serde_json::to_value(&schema)
                            .expect("JsonSchema derive output must serialize cleanly")
                    },
                    // The derive is for typed resources only. The dynamic-field
                    // escape hatch (`kv`) registers manually via
                    // `inventory::submit!` in `types.rs`.
                    dynamic_fields: false,
                }
            }
        };
    };

    expanded.into()
}
