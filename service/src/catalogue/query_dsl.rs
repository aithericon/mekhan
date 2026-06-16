//! Catalogue query language — server-side Rust port of the frontend DSL
//! compiler (`app/src/lib/components/data/query-language.ts`).
//!
//! A tiny text DSL that compiles to the catalogue's existing [`QueryParams`]
//! (free-text search, field filters, and a single deep-merged `file_metadata`
//! JSONB containment object with Postgres `@>` AND-semantics).
//!
//! This is the SINGLE server-side compiler: the data browser, catalog
//! triggers, and catalogue subscriptions all submit the raw DSL string, which
//! is compiled here. The TS `parseQuery`/`validateTerms` survives for live chip
//! UX only — TS `compileQuery` no longer drives requests.
//!
//! Grammar (whitespace-separated terms; double-quoted values may contain
//! spaces; a term that fails to parse becomes a [`ParseError`], never a panic):
//!
//! ```text
//! 1. bare word / "quoted string"        -> free-text search term
//! 2. field OP value                     -> filter term
//!      ops: `:` (eq), `!=` / `!:` (ne), `>`, `>=`, `<`, `<=`
//!      `~` (contains), `^` (starts_with), `$` (ends_with)
//!      `field:a,b,c` (unquoted list)    -> in;   `field!:a,b,c` -> not_in
//!      `field:null` -> is_null;          `field:*` -> is_not_null
//!      quoting opts out of the null/*/comma special forms
//! 3. containment sugar (one merged file_metadata object):
//!      col:NAME | dim:NAME | pii:CLASS | attr:KEY=VALUE
//!    (`format:VALUE` is NOT containment -- it compiles to a `meta.format` eq.)
//! 4. datatype sugar (at most one per query):
//!      datatype:NAME -> a `meta.schema` eq/in via a caller-supplied resolver
//!      (fail-closed `meta.schema eq ''` when the name does not resolve)
//! 5. compile-time value coercions (pure function of a `now` parameter):
//!      byte suffixes (10k/5m/2g/1t[b|ib], decimals ok) for `*_bytes` fields
//!      relative dates (-7d/-24h/-90m/-3w/-2y) for `*_at` fields
//! ```

use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value};

use crate::query::extractor::QueryParams;
use crate::query::filter::{Filter, FilterCondition, FilterOperator, FilterValue};
use crate::query::pagination::PageQuery;

// ─────────────────────────────────────────────────────────────────────────────
// Term model
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed query operator (mirrors the TS `QueryOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    StartsWith,
    EndsWith,
    In,
    NotIn,
    IsNull,
    IsNotNull,
}

/// A `col`/`dim`/`pii`/`attr`/`format` containment-sugar discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainName {
    Format,
    Col,
    Dim,
    Pii,
    Attr,
}

/// A single parsed term (mirrors the TS `QueryTerm` union).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryTerm {
    Search {
        text: String,
        raw: String,
    },
    Filter {
        field: String,
        op: QueryOp,
        value: String,
        raw: String,
    },
    Contain {
        term: ContainName,
        key: Option<String>,
        value: String,
        raw: String,
    },
    Datatype {
        name: String,
        raw: String,
    },
}

/// A parse error entry (mirrors the TS `ParseError`); `index` is the byte
/// offset of the offending token in the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub raw: String,
    pub index: usize,
    pub message: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Field-shape helpers
// ─────────────────────────────────────────────────────────────────────────────

fn is_bytes_field(field: &str) -> bool {
    field == "size_bytes" || field.ends_with("_bytes")
}

fn is_at_field(field: &str) -> bool {
    field.ends_with("_at")
}

/// `^-?\d+(\.\d+)?$`
fn is_num(s: &str) -> bool {
    let b = s.strip_prefix('-').unwrap_or(s);
    let mut parts = b.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    if int.is_empty() || !int.bytes().all(|c| c.is_ascii_digit()) {
        return false;
    }
    match parts.next() {
        None => true,
        Some(frac) => !frac.is_empty() && frac.bytes().all(|c| c.is_ascii_digit()),
    }
}

/// Parse a byte-suffixed size: `^(\d+(\.\d+)?)([kmgt])(i?b)?$` (case-insensitive).
/// Returns `(mantissa, exp)` where exp is the 1024-power.
fn parse_byte(s: &str) -> Option<(f64, u32)> {
    let lower = s.to_ascii_lowercase();
    // Trailing optional `ib` or `b`.
    let body = if let Some(stripped) = lower.strip_suffix("ib") {
        stripped
    } else if let Some(stripped) = lower.strip_suffix('b') {
        stripped
    } else {
        &lower
    };
    let unit = body.chars().last()?;
    let exp = match unit {
        'k' => 1,
        'm' => 2,
        'g' => 3,
        't' => 4,
        _ => return None,
    };
    let mantissa_str = &body[..body.len() - 1];
    if mantissa_str.is_empty() || !is_num_unsigned(mantissa_str) {
        return None;
    }
    let mantissa: f64 = mantissa_str.parse().ok()?;
    Some((mantissa, exp))
}

/// `^\d+(\.\d+)?$` (no sign — byte mantissas are unsigned).
fn is_num_unsigned(s: &str) -> bool {
    let mut parts = s.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    if int.is_empty() || !int.bytes().all(|c| c.is_ascii_digit()) {
        return false;
    }
    match parts.next() {
        None => true,
        Some(frac) => !frac.is_empty() && frac.bytes().all(|c| c.is_ascii_digit()),
    }
}

/// Parse a relative date `^-(\d+(\.\d+)?)([mhdwy])$`; returns `(magnitude, unit)`.
fn parse_rel_date(s: &str) -> Option<(f64, char)> {
    let rest = s.strip_prefix('-')?;
    let unit = rest.chars().last()?;
    if !matches!(unit, 'm' | 'h' | 'd' | 'w' | 'y') {
        return None;
    }
    let mag_str = &rest[..rest.len() - 1];
    if mag_str.is_empty() || !is_num_unsigned(mag_str) {
        return None;
    }
    let mag: f64 = mag_str.parse().ok()?;
    Some((mag, unit))
}

/// `^\d{4}-\d{2}-\d{2}([T ].*)?$`
fn is_iso_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 10 {
        return false;
    }
    let digit = |i: usize| bytes[i].is_ascii_digit();
    let ok = digit(0)
        && digit(1)
        && digit(2)
        && digit(3)
        && bytes[4] == b'-'
        && digit(5)
        && digit(6)
        && bytes[7] == b'-'
        && digit(8)
        && digit(9);
    if !ok {
        return false;
    }
    match bytes.get(10) {
        None => true,
        Some(&c) => c == b'T' || c == b' ',
    }
}

/// Comparison ops require a value that compiles to something ordered: a plain
/// number, a byte-suffixed size (only on `*_bytes` fields), or a relative / ISO
/// date (only on `*_at` fields).
fn is_comparable_value(field: &str, value: &str) -> bool {
    if is_num(value) {
        return true;
    }
    if is_bytes_field(field) && parse_byte(value).is_some() {
        return true;
    }
    if is_at_field(field) && (parse_rel_date(value).is_some() || is_iso_date(value)) {
        return true;
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Tokenizer
// ─────────────────────────────────────────────────────────────────────────────

struct Token {
    raw: String,
    index: usize,
}

/// Split input into whitespace-separated tokens; double quotes glue spaces.
/// Indexing is by Unicode scalar position (matching the TS string-index
/// semantics closely enough for ASCII queries, and never panicking on UTF-8).
fn tokenize(input: &str) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < n {
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= n {
            break;
        }
        let start = i;
        let mut in_quote = false;
        while i < n && (in_quote || !chars[i].is_whitespace()) {
            if chars[i] == '"' {
                in_quote = !in_quote;
            }
            i += 1;
        }
        let raw: String = chars[start..i].iter().collect();
        tokens.push(Token { raw, index: start });
    }
    tokens
}

struct Unquoted {
    value: String,
    quoted: bool,
    error: Option<&'static str>,
}

/// Strip a single pair of surrounding double quotes, if present.
fn unquote(v: &str) -> Unquoted {
    if !v.starts_with('"') {
        return Unquoted {
            value: v.to_string(),
            quoted: false,
            error: None,
        };
    }
    // Find the next `"` after the opening one (char-based).
    match v.char_indices().skip(1).find(|(_, c)| *c == '"') {
        None => Unquoted {
            value: v[1..].to_string(),
            quoted: true,
            error: Some("unterminated quote"),
        },
        Some((end, _)) => {
            if end != v.len() - 1 {
                Unquoted {
                    value: v.to_string(),
                    quoted: true,
                    error: Some("unexpected characters after closing quote"),
                }
            } else {
                Unquoted {
                    value: v[1..end].to_string(),
                    quoted: true,
                    error: None,
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Term classification
// ─────────────────────────────────────────────────────────────────────────────

enum Classified {
    Term(QueryTerm),
    Error(String),
}

const CONTAIN_NAMES: &[(&str, ContainName)] = &[
    ("format", ContainName::Format),
    ("col", ContainName::Col),
    ("dim", ContainName::Dim),
    ("pii", ContainName::Pii),
    ("attr", ContainName::Attr),
];

/// Match the TS `FILTER_RE`: `^([A-Za-z_][A-Za-z0-9_.]*)(>=|<=|!=|!:|>|<|~|\^|\$|:)(.*)$`.
/// Returns `(field, op_text, rest)`.
fn match_filter(raw: &str) -> Option<(String, &'static str, String)> {
    let chars: Vec<char> = raw.chars().collect();
    if chars.is_empty() {
        return None;
    }
    // Field: [A-Za-z_][A-Za-z0-9_.]*
    let first = chars[0];
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut i = 1;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            i += 1;
        } else {
            break;
        }
    }
    let field: String = chars[..i].iter().collect();
    // Operator: longest-first among the two-char ops, then single-char.
    let two: String = chars.get(i..i + 2).map(|s| s.iter().collect()).unwrap_or_default();
    let (op, op_len): (&'static str, usize) = match two.as_str() {
        ">=" => (">=", 2),
        "<=" => ("<=", 2),
        "!=" => ("!=", 2),
        "!:" => ("!:", 2),
        _ => match chars.get(i) {
            Some('>') => (">", 1),
            Some('<') => ("<", 1),
            Some('~') => ("~", 1),
            Some('^') => ("^", 1),
            Some('$') => ("$", 1),
            Some(':') => (":", 1),
            _ => return None,
        },
    };
    let rest: String = chars[i + op_len..].iter().collect();
    Some((field, op, rest))
}

fn classify_token(raw: &str) -> Classified {
    if raw.starts_with('"') {
        let u = unquote(raw);
        if let Some(e) = u.error {
            return Classified::Error(e.to_string());
        }
        return Classified::Term(QueryTerm::Search {
            text: u.value,
            raw: raw.to_string(),
        });
    }
    if let Some((field, op_text, rest)) = match_filter(raw) {
        // `datatype:` is sugar only with the `:` op.
        if op_text == ":" && field == "datatype" {
            let u = unquote(&rest);
            if let Some(e) = u.error {
                return Classified::Error(e.to_string());
            }
            if u.value.is_empty() {
                return Classified::Error("missing value".to_string());
            }
            return Classified::Term(QueryTerm::Datatype {
                name: u.value,
                raw: raw.to_string(),
            });
        }
        if op_text == ":" {
            if let Some((_, name)) = CONTAIN_NAMES.iter().find(|(n, _)| *n == field) {
                return classify_contain(*name, &rest, raw);
            }
        }
        return classify_filter(&field, op_text, &rest, raw);
    }
    if raw.contains([':', '<', '>', '"']) {
        return Classified::Error("could not parse term".to_string());
    }
    Classified::Term(QueryTerm::Search {
        text: raw.to_string(),
        raw: raw.to_string(),
    })
}

fn classify_contain(name: ContainName, rest: &str, raw: &str) -> Classified {
    if name == ContainName::Attr {
        let eq = rest.find('=');
        match eq {
            // `eq <= 0`: no `=`, or `=` at index 0 (empty key).
            None => return Classified::Error("attr term requires KEY=VALUE".to_string()),
            Some(0) => return Classified::Error("attr term requires KEY=VALUE".to_string()),
            Some(pos) => {
                let key = rest[..pos].to_string();
                let u = unquote(&rest[pos + 1..]);
                if let Some(e) = u.error {
                    return Classified::Error(e.to_string());
                }
                if u.value.is_empty() {
                    return Classified::Error("missing value".to_string());
                }
                return Classified::Term(QueryTerm::Contain {
                    term: ContainName::Attr,
                    key: Some(key),
                    value: u.value,
                    raw: raw.to_string(),
                });
            }
        }
    }
    let u = unquote(rest);
    if let Some(e) = u.error {
        return Classified::Error(e.to_string());
    }
    if u.value.is_empty() {
        return Classified::Error("missing value".to_string());
    }
    Classified::Term(QueryTerm::Contain {
        term: name,
        key: None,
        value: u.value,
        raw: raw.to_string(),
    })
}

fn classify_filter(field: &str, op_text: &str, rest: &str, raw: &str) -> Classified {
    if rest.is_empty() {
        return Classified::Error("missing value".to_string());
    }
    let u = unquote(rest);
    if let Some(e) = u.error {
        return Classified::Error(e.to_string());
    }
    let base_op = match op_text {
        ":" => QueryOp::Eq,
        "!=" | "!:" => QueryOp::Ne,
        "~" => QueryOp::Contains,
        "^" => QueryOp::StartsWith,
        "$" => QueryOp::EndsWith,
        ">" => QueryOp::Gt,
        ">=" => QueryOp::Gte,
        "<" => QueryOp::Lt,
        _ => QueryOp::Lte, // "<="
    };
    let value = u.value;
    let mut op = base_op;
    if !u.quoted {
        // Unquoted-only special forms: quoting opts out of them.
        if base_op == QueryOp::Eq && value == "null" {
            return Classified::Term(QueryTerm::Filter {
                field: field.to_string(),
                op: QueryOp::IsNull,
                value: String::new(),
                raw: raw.to_string(),
            });
        }
        if base_op == QueryOp::Eq && value == "*" {
            return Classified::Term(QueryTerm::Filter {
                field: field.to_string(),
                op: QueryOp::IsNotNull,
                value: String::new(),
                raw: raw.to_string(),
            });
        }
        if (base_op == QueryOp::Eq || base_op == QueryOp::Ne) && value.contains(',') {
            op = if base_op == QueryOp::Eq {
                QueryOp::In
            } else {
                QueryOp::NotIn
            };
        }
    }
    if matches!(op, QueryOp::Gt | QueryOp::Gte | QueryOp::Lt | QueryOp::Lte)
        && !is_comparable_value(field, &value)
    {
        return Classified::Error(format!(
            "non-numeric value \"{value}\" for comparison on field \"{field}\""
        ));
    }
    Classified::Term(QueryTerm::Filter {
        field: field.to_string(),
        op,
        value,
        raw: raw.to_string(),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Containment fragments + deep merge
// ─────────────────────────────────────────────────────────────────────────────

fn contain_fragment(term: &QueryTerm) -> Value {
    let QueryTerm::Contain {
        term: name,
        key,
        value,
        ..
    } = term
    else {
        return Value::Null;
    };
    match name {
        ContainName::Format => serde_json::json!({ "format": value }),
        ContainName::Col => serde_json::json!({ "column_names": [value] }),
        ContainName::Dim => serde_json::json!({ "dimensions": [{ "name": value }] }),
        ContainName::Pii => {
            serde_json::json!({ "columns": [{ "classifications": [{ "category": value }] }] })
        }
        ContainName::Attr => {
            let k = key.clone().unwrap_or_default();
            let mut attrs = Map::new();
            attrs.insert(
                k,
                serde_json::json!({ "type": "String", "value": value }),
            );
            Value::Object({
                let mut m = Map::new();
                m.insert("attributes".to_string(), Value::Object(attrs));
                m
            })
        }
    }
}

/// Pure deep merge: plain objects merge recursively, arrays CONCATENATE,
/// anything else (scalar vs scalar, scalar vs container) is a conflict.
fn try_merge(a: Value, b: Value) -> Option<Value> {
    match (a, b) {
        (Value::Null, b) => Some(b),
        (Value::Array(mut av), Value::Array(bv)) => {
            av.extend(bv);
            Some(Value::Array(av))
        }
        (Value::Object(am), Value::Object(bm)) => {
            let mut out = am;
            for (k, v) in bm {
                let existing = out.remove(&k).unwrap_or(Value::Null);
                let merged = try_merge(existing, v)?;
                out.insert(k, merged);
            }
            Some(Value::Object(out))
        }
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Parse
// ─────────────────────────────────────────────────────────────────────────────

/// Parse the DSL into terms + parse errors (mirrors TS `parseQuery`). Errors
/// never throw; a malformed token becomes an error entry.
pub fn parse_query(input: &str) -> (Vec<QueryTerm>, Vec<ParseError>) {
    let mut terms = Vec::new();
    let mut errors = Vec::new();
    // Running merge of containment fragments so a scalar conflict (e.g. a second
    // `format:` term) is flagged on the LATER term at parse time.
    let mut contain_acc: Value = Value::Object(Map::new());
    let mut has_datatype = false;
    for tok in tokenize(input) {
        match classify_token(&tok.raw) {
            Classified::Error(message) => {
                errors.push(ParseError {
                    raw: tok.raw,
                    index: tok.index,
                    message,
                });
            }
            Classified::Term(term) => {
                if let QueryTerm::Contain { term: name, .. } = &term {
                    let frag = contain_fragment(&term);
                    match try_merge(contain_acc.clone(), frag) {
                        Some(v) => contain_acc = v,
                        None => {
                            errors.push(ParseError {
                                raw: tok.raw,
                                index: tok.index,
                                message: format!("duplicate {} term", contain_name_str(*name)),
                            });
                            continue;
                        }
                    }
                }
                if let QueryTerm::Datatype { .. } = &term {
                    if has_datatype {
                        errors.push(ParseError {
                            raw: tok.raw,
                            index: tok.index,
                            message: "duplicate datatype term".to_string(),
                        });
                        continue;
                    }
                    has_datatype = true;
                }
                terms.push(term);
            }
        }
    }
    (terms, errors)
}

fn contain_name_str(n: ContainName) -> &'static str {
    match n {
        ContainName::Format => "format",
        ContainName::Col => "col",
        ContainName::Dim => "dim",
        ContainName::Pii => "pii",
        ContainName::Attr => "attr",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Compile
// ─────────────────────────────────────────────────────────────────────────────

/// Resolver: map a registered data-type name to its schema-fingerprint digests.
/// `None` = unknown name (or registry not loaded) — compile fails closed.
pub type DatatypeResolver<'a> = dyn Fn(&str) -> Option<Vec<String>> + 'a;

/// Coerce a single filter value at EVAL time against `now`: byte suffixes for
/// `*_bytes` fields, relative dates for `*_at` fields. Pure in `now`.
fn coerce_value(field: &str, value: &str, now: DateTime<Utc>) -> String {
    if is_bytes_field(field) {
        if let Some((mantissa, exp)) = parse_byte(value) {
            let bytes = (mantissa * 1024f64.powi(exp as i32)).round() as i64;
            return bytes.to_string();
        }
    }
    if is_at_field(field) {
        if let Some((mag, unit)) = parse_rel_date(value) {
            let ms = match unit {
                'm' => 60_000f64,
                'h' => 3_600_000f64,
                'd' => 86_400_000f64,
                'w' => 7.0 * 86_400_000f64,
                'y' => 365.0 * 86_400_000f64,
                _ => 0.0,
            };
            let delta_ms = (mag * ms).round() as i64;
            let when = now - Duration::milliseconds(delta_ms);
            // Match JS `toISOString()`: always UTC, millisecond precision, `Z`.
            return when.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        }
    }
    value.to_string()
}

/// Wrap a string value into a [`FilterValue`], mirroring the extractor's
/// `parse_value` type ladder so the SQL binds compare natively.
///
/// - null ops → [`FilterValue::Null`]
/// - in/not_in → [`FilterValue::StringList`]
/// - contains/starts_with/ends_with → [`FilterValue::String`]
/// - eq/ne/comparisons → bool → i64 → f64 → string
fn to_filter_value(op: QueryOp, value: &str) -> FilterValue {
    match op {
        QueryOp::IsNull | QueryOp::IsNotNull => FilterValue::Null,
        QueryOp::In | QueryOp::NotIn => {
            FilterValue::StringList(value.split(',').map(|v| v.trim().to_string()).collect())
        }
        QueryOp::Contains | QueryOp::StartsWith | QueryOp::EndsWith => {
            FilterValue::String(value.to_string())
        }
        QueryOp::Eq | QueryOp::Ne | QueryOp::Gt | QueryOp::Gte | QueryOp::Lt | QueryOp::Lte => {
            if let Ok(b) = value.parse::<bool>() {
                FilterValue::Bool(b)
            } else if let Ok(i) = value.parse::<i64>() {
                FilterValue::Int(i)
            } else if let Ok(f) = value.parse::<f64>() {
                FilterValue::Float(f)
            } else {
                FilterValue::String(value.to_string())
            }
        }
    }
}

fn op_to_operator(op: QueryOp) -> FilterOperator {
    match op {
        QueryOp::Eq => FilterOperator::Eq,
        QueryOp::Ne => FilterOperator::Ne,
        QueryOp::Gt => FilterOperator::Gt,
        QueryOp::Gte => FilterOperator::Gte,
        QueryOp::Lt => FilterOperator::Lt,
        QueryOp::Lte => FilterOperator::Lte,
        QueryOp::Contains => FilterOperator::Contains,
        QueryOp::StartsWith => FilterOperator::StartsWith,
        QueryOp::EndsWith => FilterOperator::EndsWith,
        QueryOp::In => FilterOperator::In,
        QueryOp::NotIn => FilterOperator::NotIn,
        QueryOp::IsNull => FilterOperator::IsNull,
        QueryOp::IsNotNull => FilterOperator::IsNotNull,
    }
}

/// Compile parsed terms into [`QueryParams`]. Pure: relative dates resolve from
/// `now`, and `datatype:` terms resolve through `resolve_datatype`.
///
/// Page / sort are left at default (page 0, size 20, no sort) — callers that
/// run this as a membership test override the page bounds.
pub fn compile_terms(
    terms: &[QueryTerm],
    now: DateTime<Utc>,
    resolve_datatype: &DatatypeResolver,
) -> QueryParams {
    let mut search_parts: Vec<String> = Vec::new();
    let mut conditions: Vec<FilterCondition> = Vec::new();
    let mut meta: Value = Value::Null;

    for t in terms {
        match t {
            QueryTerm::Search { text, .. } => {
                search_parts.push(text.clone());
            }
            QueryTerm::Datatype { name, .. } => {
                let digests = resolve_datatype(name).filter(|d| !d.is_empty());
                match digests {
                    Some(d) if d.len() == 1 => {
                        conditions.push(FilterCondition {
                            field: "meta.schema".to_string(),
                            operator: FilterOperator::Eq,
                            value: FilterValue::String(d[0].clone()),
                        });
                    }
                    Some(d) => {
                        conditions.push(FilterCondition {
                            field: "meta.schema".to_string(),
                            operator: FilterOperator::In,
                            value: FilterValue::StringList(d),
                        });
                    }
                    None => {
                        // FAIL CLOSED: an unresolved data type matches nothing.
                        conditions.push(FilterCondition {
                            field: "meta.schema".to_string(),
                            operator: FilterOperator::Eq,
                            value: FilterValue::String(String::new()),
                        });
                    }
                }
            }
            QueryTerm::Filter {
                field, op, value, ..
            } => {
                let coerced = match op {
                    QueryOp::In | QueryOp::NotIn => value
                        .split(',')
                        .map(|v| coerce_value(field, v, now))
                        .collect::<Vec<_>>()
                        .join(","),
                    QueryOp::IsNull | QueryOp::IsNotNull => value.clone(),
                    _ => coerce_value(field, value, now),
                };
                conditions.push(FilterCondition {
                    field: field.clone(),
                    operator: op_to_operator(*op),
                    value: to_filter_value(*op, &coerced),
                });
            }
            QueryTerm::Contain {
                term: ContainName::Format,
                value,
                ..
            } => {
                // `format:` compiles to a `meta.format` eq (lowercased), not a
                // JSONB containment fragment. The server unwraps the
                // `FileFormat::Unknown` `{"unknown":…}` envelope behind that
                // expr, so `format:fasta` matches probe-unknown formats too.
                conditions.push(FilterCondition {
                    field: "meta.format".to_string(),
                    operator: FilterOperator::Eq,
                    value: FilterValue::String(value.to_ascii_lowercase()),
                });
            }
            QueryTerm::Contain { .. } => {
                // Conflicting fragments were flagged at parse time; here we keep
                // the first writer and silently skip the conflicting term.
                if let Some(merged) = try_merge(meta.clone(), contain_fragment(t)) {
                    meta = merged;
                }
            }
        }
    }

    let search = {
        let joined = search_parts.join(" ");
        let trimmed = joined.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    let file_metadata = match &meta {
        Value::Null => None,
        other => Some(other.to_string()),
    };

    let filter = if conditions.is_empty() {
        None
    } else {
        Some(Filter::new(conditions))
    };

    QueryParams {
        page: PageQuery {
            page: 0,
            page_size: 20,
        },
        filter,
        sort: None,
        metadata: None,
        file_metadata,
        search,
    }
}

/// One-shot entry point: parse + compile a raw DSL string into [`QueryParams`].
/// Parse errors are dropped here (the good terms still compile); call
/// [`parse_query`] directly when you need to surface them.
///
/// `now` re-resolves relative dates per call (pass `Utc::now()` in production,
/// a fixed instant in tests). `resolve_datatype` injects the data-type registry.
pub fn compile_query(
    dsl: &str,
    now: DateTime<Utc>,
    resolve_datatype: &DatatypeResolver,
) -> QueryParams {
    let (terms, _errors) = parse_query(dsl);
    compile_terms(&terms, now, resolve_datatype)
}

// ─────────────────────────────────────────────────────────────────────────────
// Registry-backed resolver wiring
// ─────────────────────────────────────────────────────────────────────────────

/// Collect the `datatype:<name>` names referenced by a DSL string (deduped).
/// Used to pre-fetch the registry before building a sync resolver closure.
pub fn datatype_names(dsl: &str) -> Vec<String> {
    let (terms, _errors) = parse_query(dsl);
    let mut out: Vec<String> = Vec::new();
    for t in terms {
        if let QueryTerm::Datatype { name, .. } = t {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// A pre-resolved name→digests map. Build it once (async, off the DB) then hand
/// `as_resolver()` to the pure [`compile_query`] / [`compile_terms`]. A name
/// absent from the map resolves to `None` (fail-closed `meta.schema eq ''`).
#[derive(Debug, Default, Clone)]
pub struct DatatypeRegistry {
    map: std::collections::HashMap<String, Vec<String>>,
}

impl DatatypeRegistry {
    /// Build a registry by resolving every `datatype:<name>` in `dsl` against
    /// `catalogue_data_types` (global; names are globally unique). Names with no
    /// matching type are simply absent from the map (resolver → fail-closed).
    pub async fn resolve(pool: &sqlx::PgPool, dsl: &str) -> Result<Self, sqlx::Error> {
        let names = datatype_names(dsl);
        Self::resolve_names(pool, &names).await
    }

    /// Build a registry for an explicit name list.
    pub async fn resolve_names(
        pool: &sqlx::PgPool,
        names: &[String],
    ) -> Result<Self, sqlx::Error> {
        let mut map = std::collections::HashMap::new();
        if names.is_empty() {
            return Ok(Self { map });
        }
        // name → array of digests (ordered for determinism). One row per name
        // that has at least one digest.
        let rows: Vec<(String, Vec<String>)> = sqlx::query_as(
            "SELECT t.name, \
                    coalesce(array_agg(d.digest ORDER BY d.created_at, d.digest) \
                             FILTER (WHERE d.digest IS NOT NULL), '{}') AS digests \
             FROM catalogue_data_types t \
             LEFT JOIN catalogue_data_type_digests d ON d.data_type_id = t.id \
             WHERE t.name = ANY($1) \
             GROUP BY t.name",
        )
        .bind(names)
        .fetch_all(pool)
        .await?;
        for (name, digests) in rows {
            map.insert(name, digests);
        }
        Ok(Self { map })
    }

    /// A `Fn(&str) -> Option<Vec<String>>` view over the map, suitable as the
    /// `resolve_datatype` argument. An empty digest list still maps to `Some`,
    /// which the compiler treats as fail-closed.
    pub fn as_resolver(&self) -> impl Fn(&str) -> Option<Vec<String>> + '_ {
        move |name: &str| self.map.get(name).cloned()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — ported from app/src/lib/components/data/query-language.test.ts
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-06-10T12:00:00.000Z".parse().unwrap()
    }

    /// No-op resolver (fail-closed datatype).
    fn no_resolver(_: &str) -> Option<Vec<String>> {
        None
    }

    /// Parse, assert no errors, return terms.
    fn good_terms(input: &str) -> Vec<QueryTerm> {
        let (terms, errors) = parse_query(input);
        assert!(errors.is_empty(), "unexpected errors for {input:?}: {errors:?}");
        terms
    }

    fn compile(input: &str) -> QueryParams {
        compile_query(input, now(), &no_resolver)
    }

    /// Render filters as `(field, op, value-as-string)` triples for assertions.
    fn filter_view(qp: &QueryParams) -> Vec<(String, FilterOperator, String)> {
        qp.filter
            .as_ref()
            .map(|f| {
                f.conditions
                    .iter()
                    .map(|c| (c.field.clone(), c.operator, fv_str(&c.value)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn fv_str(v: &FilterValue) -> String {
        match v {
            FilterValue::String(s) => s.clone(),
            FilterValue::Int(i) => i.to_string(),
            FilterValue::Float(f) => f.to_string(),
            FilterValue::Bool(b) => b.to_string(),
            FilterValue::StringList(l) => l.join(","),
            FilterValue::Null => String::new(),
        }
    }

    // ── search terms ──────────────────────────────────────────────────────────

    #[test]
    fn bare_word_is_search() {
        assert_eq!(
            good_terms("hello"),
            vec![QueryTerm::Search {
                text: "hello".into(),
                raw: "hello".into()
            }]
        );
    }

    #[test]
    fn quoted_string_glues_spaces() {
        assert_eq!(
            good_terms("\"hello big world\""),
            vec![QueryTerm::Search {
                text: "hello big world".into(),
                raw: "\"hello big world\"".into()
            }]
        );
    }

    #[test]
    fn quoted_op_chars_stay_search() {
        assert_eq!(
            good_terms("\"a:b\""),
            vec![QueryTerm::Search {
                text: "a:b".into(),
                raw: "\"a:b\"".into()
            }]
        );
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert_eq!(parse_query("").0, vec![]);
        assert_eq!(parse_query("   \t \n ").0, vec![]);
    }

    #[test]
    fn bare_quote_pair_is_empty_search() {
        let (terms, errors) = parse_query("\"\"");
        assert!(errors.is_empty());
        assert_eq!(
            terms,
            vec![QueryTerm::Search {
                text: "".into(),
                raw: "\"\"".into()
            }]
        );
    }

    // ── filter ops ────────────────────────────────────────────────────────────

    #[test]
    fn filter_ops_parse() {
        let cases = [
            ("name:alice", "name", QueryOp::Eq, "alice"),
            ("name!=bob", "name", QueryOp::Ne, "bob"),
            ("name!:bob", "name", QueryOp::Ne, "bob"),
            ("count>5", "count", QueryOp::Gt, "5"),
            ("count>=5", "count", QueryOp::Gte, "5"),
            ("count<5", "count", QueryOp::Lt, "5"),
            ("count<=5", "count", QueryOp::Lte, "5"),
        ];
        for (input, field, op, value) in cases {
            assert_eq!(
                good_terms(input),
                vec![QueryTerm::Filter {
                    field: field.into(),
                    op,
                    value: value.into(),
                    raw: input.into()
                }],
                "case {input}"
            );
        }
    }

    #[test]
    fn comma_lists_are_in_not_in() {
        assert_eq!(
            good_terms("ext:csv,json"),
            vec![QueryTerm::Filter {
                field: "ext".into(),
                op: QueryOp::In,
                value: "csv,json".into(),
                raw: "ext:csv,json".into()
            }]
        );
        assert_eq!(
            good_terms("ext!=csv,json")[0],
            QueryTerm::Filter {
                field: "ext".into(),
                op: QueryOp::NotIn,
                value: "csv,json".into(),
                raw: "ext!=csv,json".into()
            }
        );
        match &good_terms("ext!:a,b,c")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::NotIn);
                assert_eq!(value, "a,b,c");
            }
            other => panic!("expected filter, got {other:?}"),
        }
    }

    #[test]
    fn substring_ops_parse() {
        match &good_terms("filename~report")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::Contains);
                assert_eq!(value, "report");
            }
            o => panic!("{o:?}"),
        }
        match &good_terms("name^run-")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::StartsWith);
                assert_eq!(value, "run-");
            }
            o => panic!("{o:?}"),
        }
        match &good_terms("filename$.csv")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::EndsWith);
                assert_eq!(value, ".csv");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn substring_ops_carry_quoted_and_literal_values() {
        match &good_terms("filename~\"q3 report\"")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::Contains);
                assert_eq!(value, "q3 report");
            }
            o => panic!("{o:?}"),
        }
        // ~ has no special comma form — value is literal.
        match &good_terms("filename~a,b")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::Contains);
                assert_eq!(value, "a,b");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn substring_ops_compile_to_server_operators() {
        assert_eq!(
            filter_view(&compile("filename~report")),
            vec![("filename".into(), FilterOperator::Contains, "report".into())]
        );
        assert_eq!(
            filter_view(&compile("name^run-")),
            vec![("name".into(), FilterOperator::StartsWith, "run-".into())]
        );
        assert_eq!(
            filter_view(&compile("filename$.csv")),
            vec![("filename".into(), FilterOperator::EndsWith, ".csv".into())]
        );
    }

    #[test]
    fn null_and_star_forms() {
        assert_eq!(
            good_terms("owner:null"),
            vec![QueryTerm::Filter {
                field: "owner".into(),
                op: QueryOp::IsNull,
                value: "".into(),
                raw: "owner:null".into()
            }]
        );
        assert_eq!(
            good_terms("owner:*"),
            vec![QueryTerm::Filter {
                field: "owner".into(),
                op: QueryOp::IsNotNull,
                value: "".into(),
                raw: "owner:*".into()
            }]
        );
    }

    #[test]
    fn quoting_opts_out_of_special_forms() {
        for (input, expected) in [
            ("owner:\"null\"", "null"),
            ("owner:\"*\"", "*"),
            ("ext:\"a,b\"", "a,b"),
        ] {
            match &good_terms(input)[0] {
                QueryTerm::Filter { op, value, .. } => {
                    assert_eq!(*op, QueryOp::Eq, "case {input}");
                    assert_eq!(value, expected, "case {input}");
                }
                o => panic!("{o:?}"),
            }
        }
    }

    #[test]
    fn quoted_value_with_spaces() {
        assert_eq!(
            good_terms("name:\"Alice Smith\""),
            vec![QueryTerm::Filter {
                field: "name".into(),
                op: QueryOp::Eq,
                value: "Alice Smith".into(),
                raw: "name:\"Alice Smith\"".into()
            }]
        );
    }

    #[test]
    fn dotted_meta_fields() {
        match &good_terms("meta.num_rows>100")[0] {
            QueryTerm::Filter {
                field, op, value, ..
            } => {
                assert_eq!(field, "meta.num_rows");
                assert_eq!(*op, QueryOp::Gt);
                assert_eq!(value, "100");
            }
            o => panic!("{o:?}"),
        }
        match &good_terms("meta.schema.version:2")[0] {
            QueryTerm::Filter { field, op, .. } => {
                assert_eq!(field, "meta.schema.version");
                assert_eq!(*op, QueryOp::Eq);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn negative_numbers_in_comparisons() {
        match &good_terms("delta>-5")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::Gt);
                assert_eq!(value, "-5");
            }
            o => panic!("{o:?}"),
        }
    }

    // ── containment sugar ─────────────────────────────────────────────────────

    #[test]
    fn parses_all_five_sugars() {
        assert_eq!(
            good_terms("format:CSV")[0],
            QueryTerm::Contain {
                term: ContainName::Format,
                key: None,
                value: "CSV".into(),
                raw: "format:CSV".into()
            }
        );
        assert_eq!(
            good_terms("col:age")[0],
            QueryTerm::Contain {
                term: ContainName::Col,
                key: None,
                value: "age".into(),
                raw: "col:age".into()
            }
        );
        assert_eq!(
            good_terms("dim:time")[0],
            QueryTerm::Contain {
                term: ContainName::Dim,
                key: None,
                value: "time".into(),
                raw: "dim:time".into()
            }
        );
        assert_eq!(
            good_terms("pii:EMAIL")[0],
            QueryTerm::Contain {
                term: ContainName::Pii,
                key: None,
                value: "EMAIL".into(),
                raw: "pii:EMAIL".into()
            }
        );
        assert_eq!(
            good_terms("attr:source=manual")[0],
            QueryTerm::Contain {
                term: ContainName::Attr,
                key: Some("source".into()),
                value: "manual".into(),
                raw: "attr:source=manual".into()
            }
        );
    }

    #[test]
    fn quoted_sugar_values() {
        match &good_terms("col:\"my col\"")[0] {
            QueryTerm::Contain { term, value, .. } => {
                assert_eq!(*term, ContainName::Col);
                assert_eq!(value, "my col");
            }
            o => panic!("{o:?}"),
        }
        match &good_terms("attr:note=\"hello world\"")[0] {
            QueryTerm::Contain {
                term, key, value, ..
            } => {
                assert_eq!(*term, ContainName::Attr);
                assert_eq!(key.as_deref(), Some("note"));
                assert_eq!(value, "hello world");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn format_with_non_colon_op_is_plain_filter() {
        match &good_terms("format!=csv")[0] {
            QueryTerm::Filter { field, op, .. } => {
                assert_eq!(field, "format");
                assert_eq!(*op, QueryOp::Ne);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn second_format_is_duplicate() {
        let (terms, errors) = parse_query("format:csv format:parquet");
        assert_eq!(terms.len(), 1);
        assert_eq!(
            terms[0],
            QueryTerm::Contain {
                term: ContainName::Format,
                key: None,
                value: "csv".into(),
                raw: "format:csv".into()
            }
        );
        assert_eq!(
            errors,
            vec![ParseError {
                raw: "format:parquet".into(),
                index: 11,
                message: "duplicate format term".into()
            }]
        );
    }

    #[test]
    fn reassigned_attr_key_is_duplicate() {
        let (terms, errors) = parse_query("attr:k=v1 attr:k=v2");
        assert_eq!(terms.len(), 1);
        assert_eq!(errors[0].raw, "attr:k=v2");
        assert_eq!(errors[0].message, "duplicate attr term");
    }

    #[test]
    fn repeated_array_sugars_and_distinct_attrs_ok() {
        assert_eq!(
            good_terms("col:a col:b dim:x dim:y pii:EMAIL pii:SSN attr:a=1 attr:b=2").len(),
            8
        );
    }

    // ── datatype sugar ────────────────────────────────────────────────────────

    #[test]
    fn parses_datatype() {
        assert_eq!(
            good_terms("datatype:gene_table"),
            vec![QueryTerm::Datatype {
                name: "gene_table".into(),
                raw: "datatype:gene_table".into()
            }]
        );
    }

    #[test]
    fn datatype_quoted_name() {
        assert_eq!(
            good_terms("datatype:\"Gene expression\""),
            vec![QueryTerm::Datatype {
                name: "Gene expression".into(),
                raw: "datatype:\"Gene expression\"".into()
            }]
        );
    }

    #[test]
    fn datatype_empty_is_error() {
        let (terms, errors) = parse_query("datatype:");
        assert_eq!(terms, vec![]);
        assert_eq!(
            errors,
            vec![ParseError {
                raw: "datatype:".into(),
                index: 0,
                message: "missing value".into()
            }]
        );
    }

    #[test]
    fn datatype_non_colon_falls_through() {
        match &good_terms("datatype!=x")[0] {
            QueryTerm::Filter { field, op, .. } => {
                assert_eq!(field, "datatype");
                assert_eq!(*op, QueryOp::Ne);
            }
            o => panic!("{o:?}"),
        }
        match &good_terms("datatype>5")[0] {
            QueryTerm::Filter { field, op, .. } => {
                assert_eq!(field, "datatype");
                assert_eq!(*op, QueryOp::Gt);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn second_datatype_is_duplicate() {
        let (terms, errors) = parse_query("datatype:a datatype:b");
        assert_eq!(terms.len(), 1);
        assert_eq!(
            terms[0],
            QueryTerm::Datatype {
                name: "a".into(),
                raw: "datatype:a".into()
            }
        );
        assert_eq!(
            errors,
            vec![ParseError {
                raw: "datatype:b".into(),
                index: 11,
                message: "duplicate datatype term".into()
            }]
        );
    }

    // ── comparison value validation ───────────────────────────────────────────

    #[test]
    fn rejects_byte_suffix_on_non_bytes_field() {
        let (terms, errors) = parse_query("meta.num_rows>1k");
        assert_eq!(terms, vec![]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].raw, "meta.num_rows>1k");
        assert!(errors[0].message.contains("non-numeric"));
    }

    #[test]
    fn accepts_byte_suffix_on_bytes_field() {
        match &good_terms("size_bytes>10k")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::Gt);
                assert_eq!(value, "10k");
            }
            o => panic!("{o:?}"),
        }
        match &good_terms("meta.total_bytes<=1.5g")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::Lte);
                assert_eq!(value, "1.5g");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn accepts_dates_on_at_fields() {
        match &good_terms("created_at>-7d")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::Gt);
                assert_eq!(value, "-7d");
            }
            o => panic!("{o:?}"),
        }
        match &good_terms("created_at<2026-01-01")[0] {
            QueryTerm::Filter { op, value, .. } => {
                assert_eq!(*op, QueryOp::Lt);
                assert_eq!(value, "2026-01-01");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn rejects_rel_dates_on_non_at_and_words_on_numeric() {
        assert_eq!(parse_query("name>-7d").1.len(), 1);
        assert_eq!(parse_query("name>abc").1.len(), 1);
    }

    // ── garbage never panics ──────────────────────────────────────────────────

    #[test]
    fn garbage_becomes_error_entry() {
        for input in [
            ":::",
            "\"unterminated",
            "field>",
            "field:",
            ">foo",
            "attr:noequals",
            "attr:=v",
            "col:",
            "a\"b",
            "name:\"a\"b",
            "<<<>>>",
            "!::!",
        ] {
            let (terms, errors) = parse_query(input);
            assert_eq!(terms, vec![], "case {input:?}");
            assert_eq!(errors.len(), 1, "case {input:?}: {errors:?}");
            assert_eq!(errors[0].raw, input, "case {input:?}");
        }
    }

    #[test]
    fn good_terms_alongside_errors_with_indices() {
        let (terms, errors) = parse_query("name:alice ::: count>2");
        assert_eq!(terms.len(), 2);
        assert_eq!(
            errors,
            vec![ParseError {
                raw: ":::".into(),
                index: 11,
                message: "could not parse term".into()
            }]
        );
    }

    // ── compile: search + filters ─────────────────────────────────────────────

    #[test]
    fn joins_search_terms() {
        assert_eq!(
            compile("hello \"big world\" foo").search.as_deref(),
            Some("hello big world foo")
        );
    }

    #[test]
    fn omits_search_and_filemeta_when_absent() {
        let empty = compile_terms(&[], now(), &no_resolver);
        assert!(empty.search.is_none());
        assert!(empty.file_metadata.is_none());
        assert!(empty.filter.is_none());

        let c = compile("name:alice");
        assert!(c.search.is_none());
        assert!(c.file_metadata.is_none());
        assert_eq!(
            filter_view(&c),
            vec![("name".into(), FilterOperator::Eq, "alice".into())]
        );
    }

    #[test]
    fn passes_every_op_through() {
        let c = compile("a:1 b!=2 c>3 d>=4 e<5 f<=6 g:x,y h!=x,y i:null j:*");
        let ops: Vec<FilterOperator> = c
            .filter
            .as_ref()
            .unwrap()
            .conditions
            .iter()
            .map(|x| x.operator)
            .collect();
        assert_eq!(
            ops,
            vec![
                FilterOperator::Eq,
                FilterOperator::Ne,
                FilterOperator::Gt,
                FilterOperator::Gte,
                FilterOperator::Lt,
                FilterOperator::Lte,
                FilterOperator::In,
                FilterOperator::NotIn,
                FilterOperator::IsNull,
                FilterOperator::IsNotNull,
            ]
        );
    }

    // ── compile: byte-size coercion ───────────────────────────────────────────

    #[test]
    fn byte_size_coercion() {
        let cases = [
            ("size_bytes:10k", "10240"),
            ("size_bytes:5m", "5242880"),
            ("size_bytes:2g", "2147483648"),
            ("size_bytes:1t", "1099511627776"),
            ("size_bytes:1.5g", "1610612736"),
            ("size_bytes>10K", "10240"),
            ("size_bytes>=10kb", "10240"),
            ("size_bytes<10KiB", "10240"),
            ("meta.total_bytes<=2g", "2147483648"),
        ];
        for (input, expected) in cases {
            assert_eq!(filter_view(&compile(input))[0].2, expected, "case {input}");
        }
    }

    #[test]
    fn coerces_each_element_of_in_list() {
        let v = filter_view(&compile("size_bytes:1k,2k"));
        assert_eq!(v[0].0, "size_bytes");
        assert_eq!(v[0].1, FilterOperator::In);
        assert_eq!(v[0].2, "1024,2048");
    }

    #[test]
    fn no_byte_coercion_on_non_bytes_field() {
        assert_eq!(filter_view(&compile("meta.num_rows:1k"))[0].2, "1k");
    }

    #[test]
    fn plain_numbers_pass_through() {
        assert_eq!(filter_view(&compile("size_bytes>1048576"))[0].2, "1048576");
    }

    // ── compile: relative-date coercion ───────────────────────────────────────

    #[test]
    fn relative_date_coercion() {
        let cases = [
            ("created_at>-7d", "2026-06-03T12:00:00.000Z"),
            ("updated_at<-24h", "2026-06-09T12:00:00.000Z"),
            ("seen_at>=-90m", "2026-06-10T10:30:00.000Z"),
            ("created_at<=-3w", "2026-05-20T12:00:00.000Z"),
            ("archived_at>-2y", "2024-06-10T12:00:00.000Z"),
            ("created_at:-24h", "2026-06-09T12:00:00.000Z"),
        ];
        for (input, expected) in cases {
            assert_eq!(filter_view(&compile(input))[0].2, expected, "case {input}");
        }
    }

    #[test]
    fn no_rel_date_coercion_on_non_at_field() {
        assert_eq!(filter_view(&compile("label:-7d"))[0].2, "-7d");
    }

    #[test]
    fn pure_in_now() {
        let terms = good_terms("created_at>-7d");
        let a = compile_terms(
            &terms,
            "2026-01-08T00:00:00.000Z".parse().unwrap(),
            &no_resolver,
        );
        assert_eq!(filter_view(&a)[0].2, "2026-01-01T00:00:00.000Z");
        let b = compile_terms(&terms, now(), &no_resolver);
        assert_eq!(filter_view(&b)[0].2, "2026-06-03T12:00:00.000Z");
    }

    // ── compile: fileMetadata containment ─────────────────────────────────────

    fn fmeta(input: &str) -> Value {
        let s = compile(input).file_metadata.expect("file_metadata present");
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn each_containment_sugar_fragment() {
        assert_eq!(fmeta("col:age"), serde_json::json!({"column_names": ["age"]}));
        assert_eq!(
            fmeta("dim:time"),
            serde_json::json!({"dimensions": [{"name": "time"}]})
        );
        assert_eq!(
            fmeta("pii:EMAIL"),
            serde_json::json!({"columns": [{"classifications": [{"category": "EMAIL"}]}]})
        );
        assert_eq!(
            fmeta("attr:source=manual"),
            serde_json::json!({"attributes": {"source": {"type": "String", "value": "manual"}}})
        );
    }

    #[test]
    fn deep_merges_arrays_concat() {
        assert_eq!(
            fmeta("col:a col:b"),
            serde_json::json!({"column_names": ["a", "b"]})
        );
        assert_eq!(
            fmeta("pii:EMAIL pii:SSN"),
            serde_json::json!({"columns": [
                {"classifications": [{"category": "EMAIL"}]},
                {"classifications": [{"category": "SSN"}]}
            ]})
        );
        assert_eq!(
            fmeta("col:age dim:time attr:a=1 attr:b=2"),
            serde_json::json!({
                "column_names": ["age"],
                "dimensions": [{"name": "time"}],
                "attributes": {
                    "a": {"type": "String", "value": "1"},
                    "b": {"type": "String", "value": "2"}
                }
            })
        );
    }

    #[test]
    fn scalar_conflict_fed_to_compile_first_writer_wins() {
        let terms = vec![
            QueryTerm::Contain {
                term: ContainName::Attr,
                key: Some("k".into()),
                value: "a".into(),
                raw: "attr:k=a".into(),
            },
            QueryTerm::Contain {
                term: ContainName::Attr,
                key: Some("k".into()),
                value: "b".into(),
                raw: "attr:k=b".into(),
            },
        ];
        let qp = compile_terms(&terms, now(), &no_resolver);
        let fm: Value = serde_json::from_str(&qp.file_metadata.unwrap()).unwrap();
        assert_eq!(
            fm,
            serde_json::json!({"attributes": {"k": {"type": "String", "value": "a"}}})
        );
    }

    // ── compile: format ───────────────────────────────────────────────────────

    #[test]
    fn format_compiles_to_meta_format_eq_lowercased() {
        assert_eq!(
            filter_view(&compile("format:CSV")),
            vec![("meta.format".into(), FilterOperator::Eq, "csv".into())]
        );
        assert!(compile("format:CSV").file_metadata.is_none());
    }

    #[test]
    fn format_unknown_rides_same_path() {
        assert_eq!(
            filter_view(&compile("format:fasta")),
            vec![("meta.format".into(), FilterOperator::Eq, "fasta".into())]
        );
    }

    // ── compile: datatype resolution ──────────────────────────────────────────

    fn resolver(name: &str) -> Option<Vec<String>> {
        match name {
            "one" => Some(vec!["abc123".into()]),
            "two" => Some(vec!["abc123".into(), "def456".into()]),
            _ => None,
        }
    }

    fn compile_with(input: &str) -> QueryParams {
        compile_query(input, now(), &resolver)
    }

    #[test]
    fn single_digest_to_meta_schema_eq() {
        assert_eq!(
            filter_view(&compile_with("datatype:one")),
            vec![("meta.schema".into(), FilterOperator::Eq, "abc123".into())]
        );
    }

    #[test]
    fn multi_digest_to_meta_schema_in() {
        assert_eq!(
            filter_view(&compile_with("datatype:two")),
            vec![(
                "meta.schema".into(),
                FilterOperator::In,
                "abc123,def456".into()
            )]
        );
    }

    #[test]
    fn datatype_fails_closed_on_miss() {
        assert_eq!(
            filter_view(&compile_with("datatype:nope")),
            vec![("meta.schema".into(), FilterOperator::Eq, "".into())]
        );
    }

    #[test]
    fn datatype_fails_closed_without_resolver() {
        assert_eq!(
            filter_view(&compile("datatype:one")),
            vec![("meta.schema".into(), FilterOperator::Eq, "".into())]
        );
    }

    #[test]
    fn datatype_fails_closed_on_empty_digest_list() {
        let empty = |_: &str| Some(Vec::<String>::new());
        let qp = compile_query("datatype:empty", now(), &empty);
        assert_eq!(
            filter_view(&qp),
            vec![("meta.schema".into(), FilterOperator::Eq, "".into())]
        );
    }

    #[test]
    fn datatype_mixes_with_other_filters_in_term_order() {
        let c = compile_with("name:alice datatype:two format:CSV");
        assert_eq!(
            filter_view(&c),
            vec![
                ("name".into(), FilterOperator::Eq, "alice".into()),
                ("meta.schema".into(), FilterOperator::In, "abc123,def456".into()),
                ("meta.format".into(), FilterOperator::Eq, "csv".into()),
            ]
        );
        assert!(c.file_metadata.is_none());
    }

    /// `umeta.<key>` is a plain field filter to the compiler (it is field-agnostic);
    /// the `user_metadata ->>` projection is resolved later by the SQL builder's
    /// `CATALOGUE_DYN_FIELDS`. Here we only assert the field/op/value round-trips.
    #[test]
    fn umeta_key_compiles_as_a_plain_field_filter() {
        assert_eq!(
            filter_view(&compile("umeta.kind:bo_observation")),
            vec![(
                "umeta.kind".into(),
                FilterOperator::Eq,
                "bo_observation".into()
            )]
        );
        // Operators carry through just like any field.
        assert_eq!(
            filter_view(&compile("umeta.kind!=bo_observation")),
            vec![("umeta.kind".into(), FilterOperator::Ne, "bo_observation".into())]
        );
        assert!(compile("umeta.kind:bo_observation").file_metadata.is_none());
    }
}
