//! Normalized, UI-facing projection of probe (`fmeta`) file metadata.
//!
//! The catalogue stores the full probe output as opaque JSONB in
//! `catalogue_entries.file_metadata`. That blob carries three serde shapes the
//! frontend should never have to deal with directly:
//!
//! - `FileFormat` is externally tagged — `"csv"` for unit variants but
//!   `{"unknown":"fasta"}` for [`FileFormat::Unknown`].
//! - `FormatMetadata` is `#[serde(tag = "format", content = "details")]` — a
//!   `{ "format": "Image", "details": { … } }` envelope.
//! - `AttributeValue` is `#[serde(tag = "type", content = "value")]` and
//!   `DataType` has nested object variants (`{"Timestamp":{"timezone":"UTC"}}`).
//!
//! This module is the **consumer side** of the producer↔consumer type seam: it
//! deserializes the JSONB into the *real* [`fmeta::FileMetadata`] (so a new
//! `fmeta` field can never silently corrupt the parse — it just stays
//! unsurfaced until mapped here), then projects it into [`FileMetadataView`], a
//! presentation DTO whose schema crosses the BFF↔frontend seam via OpenAPI.
//!
//! Format-specific details are normalized into a uniform **fields + tables**
//! shape rather than a per-format typed union: scalar fields become labelled
//! chips, arrays-of-objects become tables. Any format we don't special-case —
//! including future `fmeta` additions — degrades gracefully through the generic
//! flattener instead of stringifying into the UI.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use aithericon_file_metadata::data_type::DataType;
use aithericon_file_metadata::format::{FileFormat, FormatMetadata};
use aithericon_file_metadata::types::{AttributeValue, FileMetadata};

/// Coarse format family, for icon choice and renderer dispatch on the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FormatFamily {
    Tabular,
    Spreadsheet,
    Scientific,
    Mesh,
    Image,
    Audio,
    Video,
    Archive,
    Document,
    Config,
    Unknown,
}

/// A single column in the file's schema, with the type pre-humanized to a
/// display string so the frontend never has to interpret a nested `DataType`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ColumnView {
    pub name: String,
    /// Humanized type, e.g. `int64`, `timestamp<UTC>`, `list<float64>`.
    pub data_type: String,
    pub nullable: bool,
    /// Classification tags (e.g. `email`, `ip_address`) with confidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classifications: Vec<ClassificationView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClassificationView {
    pub category: String,
    pub confidence: f64,
}

/// A named dimension with its size (rows/cols/width/height/depth/time/…).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DimensionView {
    pub name: String,
    pub size: Option<u64>,
}

/// A file-level attribute, unwrapped from the tagged [`AttributeValue`] into a
/// flat display value + a `kind` discriminant for optional styling.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AttributeView {
    pub key: String,
    pub value: String,
    /// One of `string`/`int`/`float`/`bool`/`bytes`/`list`/`null`.
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChecksumView {
    pub algorithm: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SchemaFingerprintView {
    pub digest: String,
    pub version: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PreviewView {
    pub columns: Vec<String>,
    /// Rows as display strings (cells stringified once, server-side).
    pub rows: Vec<Vec<String>>,
    pub total_row_count: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DataQualityView {
    /// Mean completeness across scored columns (0.0–1.0).
    pub completeness: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ColumnQualityView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ColumnQualityView {
    pub column_name: String,
    pub completeness: f64,
    pub distinctness: f64,
    pub score: f64,
}

/// One scalar fact about the format, ready to render as a labelled chip.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DetailField {
    /// Human-readable label (underscores → spaces).
    pub label: String,
    pub value: String,
    /// Optional unit appended after the value (e.g. `Hz`, `px`, `kbps`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// A small table flattened out of a nested array in the format details
/// (Parquet row groups, archive entries, Zarr hierarchy, …).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DetailTable {
    pub title: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// Normalized format-specific block: a discriminant plus the uniform
/// fields/tables decomposition of whatever `format_specific` carried.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FormatDetailsView {
    /// The `FormatMetadata` discriminant, snake-cased (`image`, `parquet`, …).
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<DetailField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<DetailTable>,
}

/// The full UI-facing view of a file's probe metadata.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FileMetadataView {
    /// Normalized format name (`csv`, `hdf5`, `fasta`, …).
    pub format: String,
    pub family: FormatFamily,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_columns: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unix_mode: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub readonly: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<DimensionView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ColumnView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<AttributeView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<PreviewView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<ChecksumView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_fingerprint: Option<SchemaFingerprintView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_quality: Option<DataQualityView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<FormatDetailsView>,
}

impl FileMetadataView {
    /// Build a view from the stored `file_metadata` JSONB.
    ///
    /// Returns `None` when the blob is empty (`{}`) or can't be parsed as
    /// [`fmeta::FileMetadata`] — e.g. pre-probe legacy rows that carry no
    /// `format`. Callers degrade to the row's own `content_hash`/`size_bytes`.
    pub fn from_raw(raw: &Value) -> Option<Self> {
        match raw {
            Value::Object(map) if map.is_empty() => return None,
            Value::Null => return None,
            _ => {}
        }
        let fm: FileMetadata = serde_json::from_value(raw.clone()).ok()?;
        Some(Self::from_fmeta(&fm))
    }

    fn from_fmeta(fm: &FileMetadata) -> Self {
        let mut attributes: Vec<AttributeView> = fm
            .attributes
            .iter()
            .map(|(k, v)| attribute_view(k, v))
            .collect();
        attributes.sort_by(|a, b| a.key.cmp(&b.key));

        Self {
            format: format_name(&fm.format),
            family: format_family(&fm.format),
            mime_type: fm.mime_type.clone(),
            num_rows: fm.num_rows,
            num_columns: fm.num_columns,
            size_bytes: fm.file_size_bytes,
            modified_at: fm.modified_at,
            unix_mode: fm.unix_mode,
            readonly: fm.readonly,
            encrypted: fm.encrypted,
            dimensions: fm
                .dimensions
                .iter()
                .map(|d| DimensionView {
                    name: d.name.clone(),
                    size: d.size,
                })
                .collect(),
            columns: fm
                .columns
                .iter()
                .map(|c| ColumnView {
                    name: c.name.clone(),
                    data_type: humanize_data_type(&c.data_type),
                    nullable: c.nullable,
                    classifications: c
                        .classifications
                        .iter()
                        .map(|t| ClassificationView {
                            category: t.category.clone(),
                            confidence: t.confidence,
                        })
                        .collect(),
                })
                .collect(),
            attributes,
            preview: fm.preview.as_ref().map(|p| PreviewView {
                columns: p.columns.clone(),
                rows: p
                    .rows
                    .iter()
                    .map(|row| row.iter().map(display_value).collect())
                    .collect(),
                total_row_count: p.total_row_count,
            }),
            checksum: fm.checksum.as_ref().map(|c| ChecksumView {
                algorithm: checksum_algo_name(c),
                digest: c.digest.clone(),
            }),
            schema_fingerprint: fm.schema_fingerprint.as_ref().map(|f| SchemaFingerprintView {
                digest: f.digest.clone(),
                version: f.version,
            }),
            data_quality: fm.data_quality.as_ref().map(|q| DataQualityView {
                completeness: q.completeness,
                columns: q
                    .column_scores
                    .iter()
                    .map(|c| ColumnQualityView {
                        column_name: c.column_name.clone(),
                        completeness: c.completeness,
                        distinctness: c.distinctness,
                        score: c.score,
                    })
                    .collect(),
            }),
            details: fm.format_specific.as_ref().map(build_details),
        }
    }
}

// ── FileFormat normalization ────────────────────────────────────────────────

/// Normalize a [`FileFormat`] to a plain string, collapsing the externally
/// tagged `{"unknown":"fasta"}` newtype variant to its inner name.
fn format_name(f: &FileFormat) -> String {
    match serde_json::to_value(f) {
        Ok(Value::String(s)) => s,
        Ok(Value::Object(map)) => map
            .into_iter()
            .next()
            .and_then(|(_, v)| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".into()),
        _ => "unknown".into(),
    }
}

fn format_family(f: &FileFormat) -> FormatFamily {
    use FileFormat::*;
    match f {
        Csv | Parquet | Json | Arrow => FormatFamily::Tabular,
        Xlsx | Xls | Ods => FormatFamily::Spreadsheet,
        Hdf5 | NetCdf | Fits | ZarrV2 | ZarrV3 => FormatFamily::Scientific,
        VtkLegacy | Vtu | Vtp | Vts | Vtr | Vti => FormatFamily::Mesh,
        Jpeg | Png | Tiff | WebP | Gif | Bmp => FormatFamily::Image,
        Mp3 | Flac | Wav | Ogg | Aac => FormatFamily::Audio,
        Mp4 | Mkv | Avi | WebM => FormatFamily::Video,
        Zip | Tar | Gzip | Bzip2 | Xz | Zstd | SevenZip | Rar => FormatFamily::Archive,
        Markdown | Xml | Html | Txt => FormatFamily::Document,
        Toml | Yaml | Ini | Env => FormatFamily::Config,
        Unknown(_) => FormatFamily::Unknown,
    }
}

// ── AttributeValue / DataType unwrapping ────────────────────────────────────

fn attribute_view(key: &str, v: &AttributeValue) -> AttributeView {
    let (value, kind) = match v {
        AttributeValue::String(s) => (s.clone(), "string"),
        AttributeValue::Int(n) => (n.to_string(), "int"),
        AttributeValue::Float(f) => (f.to_string(), "float"),
        AttributeValue::Bool(b) => (b.to_string(), "bool"),
        AttributeValue::Bytes(b) => (format!("{} bytes", b.len()), "bytes"),
        AttributeValue::List(items) => {
            let inner = items
                .iter()
                .map(|i| attribute_view("", i).value)
                .collect::<Vec<_>>()
                .join(", ");
            (inner, "list")
        }
        AttributeValue::Null => (String::new(), "null"),
    };
    AttributeView {
        key: key.to_string(),
        value,
        kind: kind.to_string(),
    }
}

/// Render a [`DataType`] as a compact display string the frontend can show
/// as-is. `pub(crate)`: also projects the canonical column set of a registered
/// data type (see `catalogue::data_types`).
pub(crate) fn humanize_data_type(dt: &DataType) -> String {
    match dt {
        DataType::Boolean => "boolean".into(),
        DataType::Int8 => "int8".into(),
        DataType::Int16 => "int16".into(),
        DataType::Int32 => "int32".into(),
        DataType::Int64 => "int64".into(),
        DataType::UInt8 => "uint8".into(),
        DataType::UInt16 => "uint16".into(),
        DataType::UInt32 => "uint32".into(),
        DataType::UInt64 => "uint64".into(),
        DataType::Float32 => "float32".into(),
        DataType::Float64 => "float64".into(),
        DataType::String => "string".into(),
        DataType::Binary => "binary".into(),
        DataType::Timestamp { timezone } => match timezone {
            Some(tz) => format!("timestamp<{tz}>"),
            None => "timestamp".into(),
        },
        DataType::Date => "date".into(),
        DataType::Time => "time".into(),
        DataType::Duration => "duration".into(),
        DataType::List(inner) => format!("list<{}>", humanize_data_type(inner)),
        DataType::Struct(fields) => {
            let inner = fields
                .iter()
                .map(|(n, t)| format!("{n}: {}", humanize_data_type(t)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("struct<{inner}>")
        }
        DataType::Dictionary { index, value } => format!(
            "dict<{}, {}>",
            humanize_data_type(index),
            humanize_data_type(value)
        ),
        DataType::Unknown(s) => s.clone(),
    }
}

fn checksum_algo_name(c: &aithericon_file_metadata::checksum::ChecksumInfo) -> String {
    match serde_json::to_value(&c.algorithm) {
        Ok(Value::String(s)) => s,
        _ => "unknown".into(),
    }
}

// ── Format-specific details: generic fields + tables ────────────────────────

/// Decompose `format_specific` into the uniform fields/tables view.
///
/// `format_specific` serializes as `{ "format": <Variant>, "details": <obj> }`;
/// we strip that envelope and flatten the inner object generically.
fn build_details(fs: &FormatMetadata) -> FormatDetailsView {
    let v = serde_json::to_value(fs).unwrap_or(Value::Null);
    let (kind, inner) = match v {
        Value::Object(mut m) => {
            let kind = m
                .get("format")
                .and_then(Value::as_str)
                .map(snake_case)
                .unwrap_or_default();
            let inner = m.remove("details").unwrap_or(Value::Null);
            (kind, inner)
        }
        _ => (String::new(), Value::Null),
    };

    let mut fields = Vec::new();
    let mut tables = Vec::new();
    if let Value::Object(map) = inner {
        for (key, val) in map {
            flatten_entry(&kind, &key, val, &mut fields, &mut tables);
        }
    }

    FormatDetailsView {
        kind,
        fields,
        tables,
    }
}

/// Route one `(key, value)` of a details object into either a scalar field, a
/// table (array of objects), or a joined-scalar field (array of scalars).
fn flatten_entry(
    kind: &str,
    key: &str,
    val: Value,
    fields: &mut Vec<DetailField>,
    tables: &mut Vec<DetailTable>,
) {
    match val {
        Value::Null => {}
        Value::Array(items) if items.is_empty() => {}
        Value::Array(items) => {
            if items.iter().any(|i| i.is_object()) {
                if let Some(table) = array_to_table(label(key), &items) {
                    tables.push(table);
                }
            } else {
                // Array of scalars → one joined chip.
                let joined = items
                    .iter()
                    .map(display_value)
                    .collect::<Vec<_>>()
                    .join(", ");
                fields.push(DetailField {
                    label: label(key),
                    value: joined,
                    unit: None,
                });
            }
        }
        Value::Object(obj) => {
            // Nested single-level object → prefix its scalar leaves.
            for (sub, subval) in obj {
                let nested = format!("{key}.{sub}");
                flatten_entry(kind, &nested, subval, fields, tables);
            }
        }
        scalar => fields.push(scalar_field(kind, key, &scalar)),
    }
}

/// Build a scalar [`DetailField`], applying light humanization (durations,
/// byte sizes, common units) keyed off the field name.
fn scalar_field(_kind: &str, key: &str, val: &Value) -> DetailField {
    let leaf = key.rsplit('.').next().unwrap_or(key);

    // Duration in seconds → m:ss / h:mm:ss.
    if leaf == "duration_secs" {
        if let Some(secs) = val.as_f64() {
            return DetailField {
                label: "duration".into(),
                value: human_duration(secs),
                unit: None,
            };
        }
    }

    // Byte-size fields → human-readable.
    if is_byte_field(leaf) {
        if let Some(bytes) = val.as_u64() {
            return DetailField {
                label: label(strip_suffix(leaf)),
                value: human_bytes(bytes),
                unit: None,
            };
        }
    }

    let (display_key, unit) = unit_for(leaf);
    DetailField {
        label: label(display_key),
        value: display_value(val),
        unit,
    }
}

/// Flatten an array of (mostly) objects into a table: column order is the
/// first-seen union of keys, cells are display-stringified.
fn array_to_table(title: String, items: &[Value]) -> Option<DetailTable> {
    let mut columns: Vec<String> = Vec::new();
    for item in items {
        if let Value::Object(map) = item {
            for k in map.keys() {
                if !columns.iter().any(|c| c == k) {
                    columns.push(k.clone());
                }
            }
        }
    }
    if columns.is_empty() {
        return None;
    }
    let rows = items
        .iter()
        .map(|item| {
            columns
                .iter()
                .map(|c| item.get(c).map(display_value).unwrap_or_default())
                .collect::<Vec<_>>()
        })
        .collect();
    Some(DetailTable {
        title,
        columns: columns.iter().map(|c| label(c)).collect(),
        rows,
    })
}

// ── Small formatting helpers ────────────────────────────────────────────────

/// Display a JSON scalar/compound as a compact human string (no `[object
/// Object]`, no raw `JSON.stringify`). Single-key tag objects like
/// `{"unknown":"fasta"}` collapse to their inner string.
fn display_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(display_value)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(map) => {
            if map.len() == 1 {
                if let Some((_, Value::String(s))) = map.iter().next() {
                    return s.clone();
                }
            }
            serde_json::to_string(v).unwrap_or_default()
        }
    }
}

fn label(key: &str) -> String {
    key.replace('_', " ")
}

fn snake_case(variant: &str) -> String {
    let mut out = String::with_capacity(variant.len() + 4);
    for (i, ch) in variant.char_indices() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn is_byte_field(leaf: &str) -> bool {
    leaf.ends_with("_size")
        || leaf.ends_with("_bytes")
        || leaf == "total_byte_size"
        || leaf == "byte_size"
}

fn strip_suffix(leaf: &str) -> &str {
    leaf.strip_suffix("_bytes")
        .or_else(|| leaf.strip_suffix("_size"))
        .unwrap_or(leaf)
}

/// Map a field name to an optional unit (and possibly a cleaned-up label key).
fn unit_for(leaf: &str) -> (&str, Option<String>) {
    match leaf {
        "sample_rate" => (leaf, Some("Hz".into())),
        "bitrate_kbps" => ("bitrate", Some("kbps".into())),
        "width" | "height" => (leaf, Some("px".into())),
        "fps" => (leaf, Some("fps".into())),
        "dpi" => (leaf, Some("dpi".into())),
        "channels" => (leaf, Some("ch".into())),
        _ => (leaf, None),
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

fn human_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return secs.to_string();
    }
    let total = secs.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_file_metadata::format::{
        ArchiveEntry, ArchiveMetadata, AudioMetadata, CsvMetadata, ImageMetadata, ParquetMetadata,
        RowGroupInfo,
    };
    use aithericon_file_metadata::types::{ColumnInfo, Dimension};
    use std::collections::HashMap;

    fn base(format: FileFormat) -> FileMetadata {
        FileMetadata {
            format,
            mime_type: None,
            num_rows: None,
            num_columns: None,
            file_size_bytes: None,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: vec![],
            dimensions: vec![],
            columns: vec![],
            attributes: HashMap::new(),
            format_specific: None,
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        }
    }

    fn view_of(fm: &FileMetadata) -> FileMetadataView {
        FileMetadataView::from_raw(&serde_json::to_value(fm).unwrap()).expect("view")
    }

    #[test]
    fn empty_and_legacy_rows_degrade_to_none() {
        assert!(FileMetadataView::from_raw(&serde_json::json!({})).is_none());
        assert!(FileMetadataView::from_raw(&Value::Null).is_none());
        // No `format` key → not a valid FileMetadata → None.
        assert!(FileMetadataView::from_raw(&serde_json::json!({"num_rows": 5})).is_none());
    }

    #[test]
    fn unknown_format_collapses_to_inner_name() {
        let fm = base(FileFormat::Unknown("fasta".into()));
        let v = view_of(&fm);
        assert_eq!(v.format, "fasta");
        assert_eq!(v.family, FormatFamily::Unknown);
    }

    #[test]
    fn csv_details_are_flat_fields() {
        let mut fm = base(FileFormat::Csv);
        fm.format_specific = Some(FormatMetadata::Csv(CsvMetadata {
            delimiter: ',',
            quote_char: Some('"'),
            has_header: true,
            encoding: "utf-8".into(),
            comment_lines: 0,
        }));
        let v = view_of(&fm);
        assert_eq!(v.family, FormatFamily::Tabular);
        let d = v.details.unwrap();
        assert_eq!(d.kind, "csv");
        assert!(d.tables.is_empty());
        assert!(d.fields.iter().any(|f| f.label == "has header" && f.value == "true"));
        assert!(d.fields.iter().any(|f| f.label == "encoding" && f.value == "utf-8"));
    }

    #[test]
    fn parquet_row_groups_become_a_table() {
        let mut fm = base(FileFormat::Parquet);
        fm.format_specific = Some(FormatMetadata::Parquet(ParquetMetadata {
            num_row_groups: 1,
            num_rows: 1000,
            compression: "SNAPPY".into(),
            created_by: Some("pyarrow".into()),
            version: 2,
            row_groups: vec![RowGroupInfo {
                num_rows: 1000,
                total_byte_size: 4096,
                columns: vec![],
            }],
        }));
        let v = view_of(&fm);
        let d = v.details.unwrap();
        assert_eq!(d.kind, "parquet");
        // scalar fields present
        assert!(d.fields.iter().any(|f| f.label == "compression" && f.value == "SNAPPY"));
        // row_groups → table
        let t = d.tables.iter().find(|t| t.title == "row groups").expect("table");
        assert!(t.columns.iter().any(|c| c == "num rows"));
        assert_eq!(t.rows.len(), 1);
    }

    #[test]
    fn image_units_and_no_tables() {
        let mut fm = base(FileFormat::Png);
        fm.format_specific = Some(FormatMetadata::Image(ImageMetadata {
            width: 1920,
            height: 1080,
            color_space: Some("RGB".into()),
            bit_depth: Some(8),
            channels: Some(3),
            animated: false,
            frame_count: None,
            dpi: Some(72.0),
            compression: Some("lossy".into()),
        }));
        let v = view_of(&fm);
        assert_eq!(v.family, FormatFamily::Image);
        let d = v.details.unwrap();
        let w = d.fields.iter().find(|f| f.label == "width").unwrap();
        assert_eq!(w.value, "1920");
        assert_eq!(w.unit.as_deref(), Some("px"));
    }

    #[test]
    fn audio_duration_is_humanized() {
        let mut fm = base(FileFormat::Mp3);
        fm.format_specific = Some(FormatMetadata::Audio(AudioMetadata {
            duration_secs: Some(245.0),
            sample_rate: Some(44100),
            channels: Some(2),
            bit_depth: Some(16),
            bitrate_kbps: Some(320),
            codec: Some("mp3".into()),
        }));
        let v = view_of(&fm);
        let d = v.details.unwrap();
        let dur = d.fields.iter().find(|f| f.label == "duration").unwrap();
        assert_eq!(dur.value, "4:05");
        let sr = d.fields.iter().find(|f| f.label == "sample rate").unwrap();
        assert_eq!(sr.unit.as_deref(), Some("Hz"));
    }

    #[test]
    fn archive_entries_become_a_table_with_collapsed_format() {
        let mut fm = base(FileFormat::Zip);
        fm.format_specific = Some(FormatMetadata::Archive(ArchiveMetadata {
            num_entries: Some(2),
            total_uncompressed_size: Some(50 * 1024 * 1024),
            total_compressed_size: Some(20 * 1024 * 1024),
            compression: Some("deflate".into()),
            encrypted: false,
            comment: None,
            entries: vec![ArchiveEntry {
                path: "data/report.csv".into(),
                uncompressed_size: Some(1024),
                compressed_size: Some(512),
                compression: "deflate".into(),
                is_dir: false,
                format: Some(FileFormat::Csv),
                modified_at: None,
                encrypted: false,
            }],
        }));
        let v = view_of(&fm);
        assert_eq!(v.family, FormatFamily::Archive);
        let d = v.details.unwrap();
        // byte-size totals humanized
        assert!(d
            .fields
            .iter()
            .any(|f| f.label == "total uncompressed" && f.value == "50.0 MB"));
        let t = d.tables.iter().find(|t| t.title == "entries").unwrap();
        // FileFormat::Csv cell collapses to "csv", not {"csv":null}/JSON.
        assert!(t.rows[0].iter().any(|c| c == "csv"));
    }

    #[test]
    fn columns_attributes_and_dimensions_surface() {
        let mut fm = base(FileFormat::Csv);
        fm.dimensions = vec![Dimension {
            name: "rows".into(),
            size: Some(10),
        }];
        fm.columns = vec![ColumnInfo {
            name: "ts".into(),
            data_type: DataType::Timestamp {
                timezone: Some("UTC".into()),
            },
            nullable: true,
            metadata: HashMap::new(),
            statistics: None,
            classifications: vec![],
        }];
        fm.attributes = HashMap::from([("author".into(), AttributeValue::String("alice".into()))]);
        let v = view_of(&fm);
        assert_eq!(v.dimensions[0].name, "rows");
        assert_eq!(v.columns[0].data_type, "timestamp<UTC>");
        assert_eq!(v.attributes[0].key, "author");
        assert_eq!(v.attributes[0].value, "alice");
        assert_eq!(v.attributes[0].kind, "string");
    }
}
