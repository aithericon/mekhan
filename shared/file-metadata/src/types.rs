use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::checksum::ChecksumInfo;
use crate::classify::ClassificationTag;
use crate::data_type::DataType;
use crate::fingerprint::SchemaFingerprint;
use crate::format::{FileFormat, FormatMetadata};
use crate::preview::ContentPreview;
use crate::quality::DataQualityReport;
use crate::statistics::ColumnStatistics;

/// Top-level metadata extracted from a file.
///
/// Designed for PostgreSQL JSONB storage: commonly-queried fields are promoted
/// to top-level scalars for efficient indexing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileMetadata {
    /// Detected or specified file format.
    pub format: FileFormat,
    /// MIME type string for the detected format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,

    // -- Promoted scalars for JSONB indexing --
    /// Total row count (for tabular/matrix data). Directly indexable in JSONB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_rows: Option<u64>,
    /// Total column/field count. Directly indexable in JSONB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_columns: Option<u64>,
    /// File size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size_bytes: Option<u64>,

    // -- Filesystem metadata --
    /// File name (without directory path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// Last modification time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<DateTime<Utc>>,
    /// Creation time (not available on all platforms/filesystems).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// Whether the file is read-only.
    #[serde(default)]
    pub readonly: bool,
    /// Unix permission mode bits (e.g., 0o644). Only present on Unix systems.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unix_mode: Option<u32>,

    // -- Queryable shortcuts --
    /// Column names as a flat string list.
    /// Enables efficient JSONB queries: `metadata->'column_names' ? 'temperature'`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub column_names: Vec<String>,

    // -- Full structural data --
    /// Named dimensions with sizes.
    /// Examples: `[rows: 1000, cols: 50]` for tabular, `[z: 10, y: 256, x: 256]` for 3D arrays,
    /// `[width: 1920, height: 1080]` for images.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<Dimension>,
    /// Full column schema with types. Empty for non-tabular formats.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ColumnInfo>,

    // -- Attributes as object (not array) for JSONB key access --
    /// Key-value metadata extracted from the file.
    /// Serializes to a JSON object: `{"author": {"type": "String", "value": "..."}}`
    /// Queryable with `metadata->'attributes'->>'author'`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<String, AttributeValue>,

    // -- Format-specific details --
    /// Format-specific metadata (typed enum, not opaque JSON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_specific: Option<FormatMetadata>,

    /// Content preview (first N rows for tabular formats).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<ContentPreview>,

    /// Whether the file is encrypted.
    ///
    /// - `None` — N/A or not checked (CSV, JSON, images, etc.)
    /// - `Some(false)` — checked, not encrypted (ZIP without encrypted entries, normal Excel)
    /// - `Some(true)` — encrypted (OLE2 encrypted Excel, ZIP with encrypted entries)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,

    /// File checksum (not auto-populated; set via [`crate::checksum::compute_checksum`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<ChecksumInfo>,

    /// Deterministic schema fingerprint for fast equality checks.
    /// Auto-computed by [`crate::extract_metadata`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_fingerprint: Option<SchemaFingerprint>,

    /// Aggregate data quality scores derived from column statistics.
    /// Computed on demand via [`crate::quality::compute_quality`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_quality: Option<DataQualityReport>,

    /// When the metadata was extracted.
    pub extracted_at: DateTime<Utc>,
}

impl FileMetadata {
    /// Build a minimal, format-agnostic [`FileMetadata`] for a path whose
    /// content cannot (or need not) be format-parsed — only its identity
    /// (size + checksum) matters.
    ///
    /// The format is recorded as [`FileFormat::Unknown`] (`extension`, lowercase)
    /// and only the filesystem fields are populated; no extractor runs. This is
    /// the integrity-check fallback the legacy-migration probe path uses when
    /// `extract_metadata` returns [`crate::MetadataError::UnsupportedFormat`]
    /// but a checksum was still requested — a 4M-file NAS corpus is mostly
    /// arbitrary binaries the metadata extractors don't model, yet every one
    /// still needs its content hash for reconcile.
    pub fn checksum_only(path: &std::path::Path) -> Self {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_ascii_lowercase();
        let format = FileFormat::Unknown(extension);
        let mut meta = FileMetadata {
            mime_type: Some(format.mime_type().to_string()),
            format,
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
            extracted_at: Utc::now(),
        };
        meta.populate_fs_metadata(path);
        meta
    }

    /// Populate filesystem metadata fields from the given path.
    ///
    /// Sets `file_name`, `file_size_bytes`, `modified_at`, `created_at`,
    /// `readonly`, and (on Unix) `unix_mode` from `std::fs::metadata`.
    ///
    /// Called automatically by [`crate::extract_metadata`]. When using a
    /// backend's [`crate::MetadataExtractor::extract`] directly, call this
    /// on the result to fill in filesystem fields.
    pub fn populate_fs_metadata(&mut self, path: &std::path::Path) {
        self.file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());

        if let Ok(fs_meta) = std::fs::metadata(path) {
            self.file_size_bytes = Some(fs_meta.len());
            self.modified_at = fs_meta.modified().ok().map(DateTime::<Utc>::from);
            self.created_at = fs_meta.created().ok().map(DateTime::<Utc>::from);
            self.readonly = fs_meta.permissions().readonly();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                self.unix_mode = Some(fs_meta.permissions().mode());
            }
        }
    }
}

/// A named dimension with its size.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dimension {
    /// Human-readable name (e.g., "rows", "columns", "width", "height", "depth", "time").
    pub name: String,
    /// Size along this dimension. `None` if unknown or unlimited (e.g., NetCDF unlimited dims).
    pub size: Option<u64>,
}

/// Schema information for a single column/field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnInfo {
    /// Column name.
    pub name: String,
    /// Inferred or declared data type.
    pub data_type: DataType,
    /// Whether the column can contain null/missing values.
    pub nullable: bool,
    /// Per-column metadata (as JSONB object for efficient key access).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, AttributeValue>,
    /// Column-level statistics (computed on demand via [`crate::statistics::compute_statistics`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statistics: Option<ColumnStatistics>,
    /// Content classification tags (PII detection, computed on demand).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classifications: Vec<ClassificationTag>,
}

/// Typed metadata value.
///
/// Uses `#[serde(tag = "type", content = "value")]` for unambiguous JSON representation
/// that distinguishes `{"type":"Int","value":42}` from `{"type":"Float","value":42.0}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum AttributeValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Bytes(Vec<u8>),
    List(Vec<AttributeValue>),
    Null,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::FileFormat;

    #[test]
    fn file_metadata_round_trip() {
        let meta = FileMetadata {
            format: FileFormat::Csv,
            mime_type: None,
            num_rows: Some(1000),
            num_columns: Some(5),
            file_size_bytes: Some(48_000),
            file_name: Some("data.csv".into()),
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: vec!["id".into(), "name".into(), "value".into()],
            dimensions: vec![
                Dimension {
                    name: "rows".into(),
                    size: Some(1000),
                },
                Dimension {
                    name: "columns".into(),
                    size: Some(5),
                },
            ],
            columns: vec![ColumnInfo {
                name: "id".into(),
                data_type: DataType::Int64,
                nullable: false,
                metadata: HashMap::new(),
                statistics: None,
                classifications: vec![],
            }],
            attributes: HashMap::from([
                ("source".into(), AttributeValue::String("sensor_a".into())),
                ("version".into(), AttributeValue::Int(2)),
            ]),
            format_specific: None,
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: Utc::now(),
        };

        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: FileMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta.format, back.format);
        assert_eq!(meta.num_rows, back.num_rows);
        assert_eq!(meta.column_names, back.column_names);
        assert_eq!(meta.attributes.len(), back.attributes.len());
    }

    #[test]
    fn attribute_value_tagged_serialization() {
        let val = AttributeValue::Int(42);
        let json = serde_json::to_string(&val).unwrap();
        assert!(json.contains(r#""type":"Int"#));
        assert!(json.contains(r#""value":42"#));

        let back: AttributeValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn attributes_serialize_as_object() {
        let attrs: HashMap<String, AttributeValue> = HashMap::from([
            ("author".into(), AttributeValue::String("test".into())),
            ("count".into(), AttributeValue::Int(10)),
        ]);

        let json = serde_json::to_value(&attrs).unwrap();
        // Should be a JSON object, not an array
        assert!(json.is_object());
        assert!(json.get("author").is_some());
        assert!(json.get("count").is_some());
    }

    #[test]
    fn empty_fields_omitted_in_json() {
        let meta = FileMetadata {
            format: FileFormat::Csv,
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
            extracted_at: Utc::now(),
        };

        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains("num_rows"));
        assert!(!json.contains("column_names"));
        assert!(!json.contains("attributes"));
        assert!(!json.contains("format_specific"));
        assert!(!json.contains("file_name"));
        assert!(!json.contains("modified_at"));
        assert!(!json.contains("unix_mode"));
    }

    #[test]
    fn populate_fs_metadata_fills_fields() {
        let tmp = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp.path(), "a,b\n1,2\n").unwrap();

        let mut meta = FileMetadata {
            format: FileFormat::Csv,
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
            extracted_at: Utc::now(),
        };

        meta.populate_fs_metadata(tmp.path());

        assert!(meta.file_name.is_some());
        assert!(meta.file_name.unwrap().ends_with(".csv"));
        assert!(meta.file_size_bytes.is_some());
        assert!(meta.modified_at.is_some());
        // created_at may not be available on all filesystems
        assert!(!meta.readonly);

        #[cfg(unix)]
        assert!(meta.unix_mode.is_some());
    }

    #[cfg(feature = "csv")]
    #[test]
    fn extract_metadata_populates_fs_fields() {
        let tmp = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp.path(), "a,b\n1,2\n").unwrap();

        let meta = crate::extract_metadata(tmp.path()).unwrap();

        assert!(meta.file_name.is_some());
        assert!(meta.file_size_bytes.is_some());
        assert!(meta.modified_at.is_some());

        #[cfg(unix)]
        assert!(meta.unix_mode.is_some());
    }

    #[test]
    fn checksum_only_records_unknown_format_and_fs_fields() {
        let tmp = tempfile::NamedTempFile::with_suffix(".dat").unwrap();
        std::fs::write(tmp.path(), b"arbitrary binary the extractors don't model").unwrap();

        let meta = FileMetadata::checksum_only(tmp.path());

        // Format is Unknown(lowercase-extension); no extractor ran.
        assert_eq!(meta.format, FileFormat::Unknown("dat".into()));
        assert_eq!(meta.mime_type.as_deref(), Some("application/octet-stream"));
        // Filesystem fields are populated so probe can still emit size.
        assert!(meta.file_size_bytes.is_some());
        assert!(meta.file_name.is_some());
        // No checksum computed yet — the caller sets it.
        assert!(meta.checksum.is_none());
    }
}
