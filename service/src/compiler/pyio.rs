//! Generation of the per-node `_aithericon_io` Python files (a thin SDK
//! delegate plus a typing-only `.pyi` overlay of the node's input scope
//! AND one `class _<Slug>NS:` per upstream producer reachable as
//! `<slug>.<field>` — the direct-attribute access model the Python
//! runner exposes via `globals()`).

use crate::models::template::FieldKind;

/// Map a port `FieldKind` to the Python annotation used in the generated stub.
/// Token values are JSON; everything non-numeric/bool/opaque serialises as a
/// string in practice, so collapse the text-like kinds to `str`.
fn py_type(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::Number => "float",
        FieldKind::Bool => "bool",
        FieldKind::Json => "Any",
        _ => "str",
    }
}

/// `true` if `name` is a safe Python attribute identifier (valid identifier,
/// not a keyword). Unsafe field names are dropped from the typed surface but
/// remain reachable via `Input.raw[...]`, so one odd field name can never
/// break the whole step at import time.
fn is_py_identifier(name: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield", "match", "case",
    ];
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    if !chars.all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        return false;
    }
    !KEYWORDS.contains(&name)
}

/// Convert a slug into a PascalCase namespace class name (`review` →
/// `Review`, `manual_review` → `ManualReview`, `step-1` → `Step1`).
fn ns_class_name(slug: &str) -> String {
    let mut out = String::new();
    let mut up = true;
    for c in slug.chars() {
        if c == '_' || c == '-' {
            up = true;
            continue;
        }
        if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        return "Ns".to_string();
    }
    out
}

/// A generated per-node file: `(filename, source)`.
pub type GeneratedFile = (&'static str, String);

/// Generate the per-node `_aithericon_io.pyi` typing overlay for a Python
/// automated step. Typing-only — the runtime token loader is
/// `aithericon.token()` (exported by the SDK package) and `token` / `input`
/// are also injected as runner globals (see executor PythonBackend). There
/// is no companion `.py` file: it would be pure delegation
/// (`return aithericon.token()`) staged per-node for no benefit.
///
/// The `.pyi` declares:
/// - The inbound control token's `Token.<field>` set (Start fields,
///   identity/metadata).
/// - One `class _<Slug>NS:` per upstream producer reachable as
///   `<slug>.<field>` (the same set the canvas / IDE picker offers). The
///   compiler stages those producers as `<slug>.json` whenever the user's
///   source actually references them, and the runner promotes each to a
///   module-level global — so `review.invoice_amount` is a plain attribute
///   lookup typed against this overlay.
/// - Declared output names (`set_output(name, ...)` candidates) so a top-
///   level assignment in user code type-checks as a known output write.
///
/// Regenerated on every publish from the live graph.
pub fn generate_py_io_files(
    fields: &std::collections::BTreeMap<String, FieldKind>,
    namespaces: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, FieldKind>>,
    outputs: &std::collections::BTreeMap<String, FieldKind>,
) -> Vec<GeneratedFile> {
    let mut decls = String::new();
    for (name, kind) in fields {
        if !is_py_identifier(name) {
            // Reachable via `token["odd-name"]` (Token is a dict); just no
            // typed attribute surface for it.
            continue;
        }
        decls.push_str(&format!(
            "    {name}: Optional[{ty}]\n",
            name = name,
            ty = py_type(*kind)
        ));
    }

    // One `class _<Slug>NS:` per reachable upstream producer plus a
    // top-level `<slug>: _<Slug>NS` declaration. Pyright/Pylance picks
    // these up so `review.<TAB>` autocompletes against the upstream
    // producer's field set instead of falling back to "unknown name".
    let mut ns_classes = String::new();
    let mut ns_decls = String::new();
    for (slug, ns_fields) in namespaces {
        if !is_py_identifier(slug) {
            continue;
        }
        let class = format!("_{}NS", ns_class_name(slug));
        ns_classes.push_str(&format!("\n\nclass {class}:\n"));
        let mut emitted_any = false;
        for (name, kind) in ns_fields {
            if !is_py_identifier(name) {
                continue;
            }
            ns_classes.push_str(&format!(
                "    {name}: Optional[{ty}]\n",
                name = name,
                ty = py_type(*kind)
            ));
            emitted_any = true;
        }
        if !emitted_any {
            ns_classes.push_str("    pass\n");
        }
        ns_decls.push_str(&format!("{slug}: {class}\n"));
    }

    // `.pyi` — a `dict` subclass so every dict method is typed for free.
    // Declared fields are the only valid attributes, so out-of-scope access
    // is a type error; item access stays open as the escape hatch.
    let token_class = if decls.is_empty() {
        "class Token(dict): ...".to_string()
    } else {
        format!("class Token(dict):\n{decls}")
    };

    // Top-level annotations for declared output fields. The runner's post-exec
    // sweep promotes every declared name found in `globals()` to
    // `<name>.json`, so `vendor = "ACME"` in user code is the implicit
    // equivalent of `set_output("vendor", "ACME")`. Annotating the names
    // here lets Pyright/Pylance treat the assignment as a typed write
    // instead of a free-floating module-level binding. Unsafe identifiers
    // (Python keywords, hyphens) are dropped from the typed surface — the
    // runtime sweep still works for them via `globals()[name]`, just no
    // editor type-check.
    let mut output_decls = String::new();
    for (name, kind) in outputs {
        if !is_py_identifier(name) {
            continue;
        }
        output_decls.push_str(&format!(
            "{name}: {ty}\n",
            name = name,
            ty = py_type(*kind)
        ));
    }
    let output_block = if output_decls.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n# Declared outputs — assignments here are emitted at step end.\n{output_decls}"
        )
    };

    let stub = format!(
        r#"# Generated by Aithericon — do not edit. Typing stub only.
# Typed view of this step's borrowed data:
#   - `token.<field>` / `input.<field>` — the slim inbound control token
#     (Start fields, identity/metadata). Both names are injected by the
#     runner; no import needed.
#   - `<slug>.<field>` — each upstream producer (HumanTask / AutomatedStep
#     / Start) reachable from this node. The compiler stages whichever
#     producers are referenced in your source as `<slug>.json`, and the
#     runner exposes them as module globals.
# Runtime token loader is `aithericon.token()`; a missing attribute is
# None at runtime even though the stub types it Optional[T] for clarity.
from typing import Any, Optional


{token_class}{ns_classes}

{ns_decls}token: Token
input: Token{output_block}
"#
    );

    vec![("_aithericon_io.pyi", stub)]
}
