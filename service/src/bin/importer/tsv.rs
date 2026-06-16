//! TSV escaping for the Postgres `COPY ... FROM STDIN WITH (FORMAT text)`
//! wire format, plus the legacy-hash normalization shared by both collections.
//!
//! Postgres text-format COPY is tab-delimited, newline-terminated, with `\N`
//! as the NULL sentinel and backslash as the escape character. Any tab,
//! newline, carriage-return, or backslash inside a field value MUST be escaped
//! or the row framing breaks. `path` and the `raw` JSONB blob both routinely
//! contain backslashes/quotes, so escaping is not optional.
//! See: <https://www.postgresql.org/docs/current/sql-copy.html> (Text Format).

/// Escape a single non-NULL field for COPY text format.
pub fn escape(field: &str) -> String {
    // Most fields have no special chars; fast-path them.
    if !field
        .bytes()
        .any(|b| matches!(b, b'\\' | b'\t' | b'\n' | b'\r'))
    {
        return field.to_string();
    }
    let mut out = String::with_capacity(field.len() + 8);
    for ch in field.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Builds one TSV row from a column list, escaping each present value and
/// emitting `\N` for NULLs. Appends the trailing newline. Columns are joined
/// with a single tab.
pub struct Row {
    buf: String,
    first: bool,
}

impl Row {
    pub fn new() -> Self {
        Row {
            buf: String::with_capacity(256),
            first: true,
        }
    }

    fn sep(&mut self) {
        if self.first {
            self.first = false;
        } else {
            self.buf.push('\t');
        }
    }

    /// Append a non-NULL text column (escaped).
    pub fn text(&mut self, value: &str) -> &mut Self {
        self.sep();
        self.buf.push_str(&escape(value));
        self
    }

    /// Append an optional text column — `None` ⇒ SQL NULL (`\N`).
    pub fn opt_text(&mut self, value: Option<&str>) -> &mut Self {
        match value {
            Some(v) => self.text(v),
            None => self.null(),
        }
    }

    /// Append an optional integer column — `None` ⇒ SQL NULL.
    pub fn opt_i64(&mut self, value: Option<i64>) -> &mut Self {
        match value {
            Some(v) => {
                self.sep();
                self.buf.push_str(itoa(v).as_str());
                self
            }
            None => self.null(),
        }
    }

    /// Append a literal SQL NULL.
    pub fn null(&mut self) -> &mut Self {
        self.sep();
        self.buf.push_str("\\N");
        self
    }

    /// Finish the row: append newline and return the bytes.
    pub fn finish(mut self) -> String {
        self.buf.push('\n');
        self.buf
    }
}

fn itoa(v: i64) -> String {
    v.to_string()
}

/// Normalize a legacy hash field to bare lowercase hex.
///
/// Legacy dump stores `"SHA256:<hex>"` (mixed case possible). We strip the
/// `SHA256:` (case-insensitively) prefix and lowercase so it matches the
/// `probe` op output and dedups cleanly. Returns `None` for empty/missing.
pub fn normalize_hash(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip a leading `algo:` prefix if present (SHA256:, sha256:, …).
    let bare = match trimmed.split_once(':') {
        Some((_algo, hex)) => hex,
        None => trimmed,
    };
    let bare = bare.trim();
    if bare.is_empty() {
        return None;
    }
    Some(bare.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_passthrough() {
        assert_eq!(escape("/Data/file.txt"), "/Data/file.txt");
    }

    #[test]
    fn escape_specials() {
        assert_eq!(escape("a\tb"), "a\\tb");
        assert_eq!(escape("a\nb"), "a\\nb");
        assert_eq!(escape("a\\b"), "a\\\\b");
        assert_eq!(escape("a\r\nb"), "a\\r\\nb");
        // Windows-style legacy paths carry backslashes.
        assert_eq!(escape("C:\\nas\\x"), "C:\\\\nas\\\\x");
    }

    #[test]
    fn row_nulls_and_values() {
        let mut r = Row::new();
        r.text("k").opt_text(None).opt_i64(Some(42)).opt_i64(None);
        assert_eq!(r.finish(), "k\t\\N\t42\t\\N\n");
    }

    #[test]
    fn hash_strips_prefix_and_lowercases() {
        assert_eq!(
            normalize_hash("SHA256:9F86D081884C7D659A2FEAA0C55AD015"),
            Some("9f86d081884c7d659a2feaa0c55ad015".to_string())
        );
        assert_eq!(normalize_hash("sha256:abcdef"), Some("abcdef".to_string()));
        // Already-bare hex passes through (lowercased).
        assert_eq!(normalize_hash("ABCD"), Some("abcd".to_string()));
        assert_eq!(normalize_hash(""), None);
        assert_eq!(normalize_hash("   "), None);
        assert_eq!(normalize_hash("SHA256:"), None);
    }
}
