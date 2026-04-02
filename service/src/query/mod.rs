//! Generic list-query infrastructure: filtering, sorting, pagination, SQL generation.
//!
//! Ported from the Suessco web-platform query infrastructure. Provides:
//! - Typed filter operators (eq, gt, contains, in, is_null, ...)
//! - Bracket-notation query param parsing (`filter[field][op]=value`)
//! - Whitelist-validated field names (prevents SQL injection)
//! - Parameterized SQL generation via sqlx `QueryBuilder`
//! - Paginated response wrapper

pub mod builder;
pub mod extractor;
pub mod filter;
pub mod pagination;

pub use filter::*;
pub use pagination::*;
