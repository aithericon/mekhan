use serde::{Deserialize, Serialize};

use crate::types::AttributeValue;

// ============================================================================
// File format identification
// ============================================================================

/// Supported file formats.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFormat {
    // Tabular / columnar
    Csv,
    Parquet,
    Json,
    Arrow,
    // Spreadsheet
    Xlsx,
    Xls,
    Ods,
    // Scientific
    Hdf5,
    NetCdf,
    Fits,
    ZarrV2,
    ZarrV3,
    // Mesh / VTK
    VtkLegacy,
    Vtu,
    Vtp,
    Vts,
    Vtr,
    Vti,
    // Image
    Jpeg,
    Png,
    Tiff,
    WebP,
    Gif,
    Bmp,
    // Audio
    Mp3,
    Flac,
    Wav,
    Ogg,
    Aac,
    // Video
    Mp4,
    Mkv,
    Avi,
    WebM,
    // Archive / compression
    Zip,
    Tar,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    SevenZip,
    Rar,
    // Text / markup / configuration
    Toml,
    Yaml,
    Markdown,
    Xml,
    Html,
    Ini,
    Env,
    Txt,
    /// Format we can identify by name but don't have a typed variant for.
    Unknown(String),
}

impl FileFormat {
    /// Return the MIME type string for this format.
    pub fn mime_type(&self) -> &str {
        match self {
            Self::Csv => "text/csv",
            Self::Json => "application/json",
            Self::Parquet => "application/vnd.apache.parquet",
            Self::Arrow => "application/vnd.apache.arrow.file",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Xls => "application/vnd.ms-excel",
            Self::Ods => "application/vnd.oasis.opendocument.spreadsheet",
            Self::Hdf5 => "application/x-hdf5",
            Self::NetCdf => "application/x-netcdf",
            Self::Fits => "application/fits",
            Self::ZarrV2 | Self::ZarrV3 => "application/x-zarr",
            Self::VtkLegacy | Self::Vtu | Self::Vtp | Self::Vts | Self::Vtr | Self::Vti => {
                "application/x-vtk"
            }
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Tiff => "image/tiff",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
            Self::Bmp => "image/bmp",
            Self::Mp3 => "audio/mpeg",
            Self::Flac => "audio/flac",
            Self::Wav => "audio/wav",
            Self::Ogg => "audio/ogg",
            Self::Aac => "audio/aac",
            Self::Mp4 => "video/mp4",
            Self::Mkv => "video/x-matroska",
            Self::Avi => "video/x-msvideo",
            Self::WebM => "video/webm",
            Self::Zip => "application/zip",
            Self::Tar => "application/x-tar",
            Self::Gzip => "application/gzip",
            Self::Bzip2 => "application/x-bzip2",
            Self::Xz => "application/x-xz",
            Self::Zstd => "application/zstd",
            Self::SevenZip => "application/x-7z-compressed",
            Self::Rar => "application/vnd.rar",
            Self::Toml => "application/toml",
            Self::Yaml => "application/yaml",
            Self::Markdown => "text/markdown",
            Self::Xml => "application/xml",
            Self::Html => "text/html",
            Self::Ini => "text/plain",
            Self::Env => "text/plain",
            Self::Txt => "text/plain",
            Self::Unknown(_) => "application/octet-stream",
        }
    }
}

// ============================================================================
// Format-specific metadata (typed enum)
// ============================================================================

/// Format-specific metadata, dispatched by format variant.
///
/// Uses `#[serde(tag = "format", content = "details")]` for clean JSONB queries:
/// `metadata->'format_specific'->>'format'` gives the discriminant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "format", content = "details")]
pub enum FormatMetadata {
    // Tabular / columnar
    Csv(CsvMetadata),
    Parquet(ParquetMetadata),
    Arrow(ArrowMetadata),
    // Scientific
    Hdf5(Hdf5Metadata),
    NetCdf(NetCdfMetadata),
    Fits(FitsMetadata),
    Zarr(ZarrMetadata),
    Vtk(VtkMetadata),
    // Spreadsheet
    Spreadsheet(SpreadsheetMetadata),
    // Media
    Image(ImageMetadata),
    Audio(AudioMetadata),
    Video(VideoMetadata),
    // Archive
    Archive(ArchiveMetadata),
    // Text / markup / configuration
    Toml(TomlMetadata),
    Yaml(YamlMetadata),
    Markdown(MarkdownMetadata),
    Xml(XmlMetadata),
    Html(HtmlMetadata),
    Ini(IniMetadata),
    Env(EnvMetadata),
    Txt(TxtMetadata),
}

// ============================================================================
// Tabular formats
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CsvMetadata {
    /// Field delimiter (e.g., ',' or '\t').
    pub delimiter: char,
    /// Quote character (typically '"').
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote_char: Option<char>,
    /// Whether the first row is a header.
    pub has_header: bool,
    /// Detected or declared text encoding (e.g., "utf-8").
    pub encoding: String,
    /// Number of comment/skip lines at the top.
    #[serde(default)]
    pub comment_lines: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParquetMetadata {
    /// Number of row groups in the file.
    pub num_row_groups: usize,
    /// Total rows across all row groups.
    pub num_rows: u64,
    /// Compression codec (e.g., "SNAPPY", "ZSTD", "GZIP", "NONE").
    pub compression: String,
    /// The "created_by" string from the file footer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Parquet format version.
    pub version: i32,
    /// Per-row-group information.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub row_groups: Vec<RowGroupInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RowGroupInfo {
    pub num_rows: u64,
    /// Total compressed byte size.
    pub total_byte_size: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<RowGroupColumnInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RowGroupColumnInfo {
    pub column_name: String,
    pub compression: String,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    /// Per-column-chunk statistics from the Parquet footer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statistics: Option<ColumnChunkStatistics>,
}

/// Statistics for a single column chunk in a Parquet row group.
///
/// Min/max are string-encoded because Parquet statistics are type-specific.
/// The column's `DataType` in [`crate::ColumnInfo`] indicates how to interpret them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnChunkStatistics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub null_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distinct_count: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArrowMetadata {
    pub num_record_batches: usize,
    pub schema_fields: Vec<String>,
}

// ============================================================================
// Spreadsheet formats
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpreadsheetMetadata {
    /// Number of sheets in the workbook.
    pub num_sheets: usize,
    /// Per-sheet metadata.
    pub sheets: Vec<SheetInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetInfo {
    /// Sheet name.
    pub name: String,
    /// Number of data rows (excluding header).
    pub num_rows: u64,
    /// Number of columns.
    pub num_columns: u64,
    /// Column headers from the first row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub column_names: Vec<String>,
}

// ============================================================================
// Scientific formats
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hdf5Metadata {
    /// Hierarchical group structure.
    pub groups: Vec<Hdf5Group>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hdf5Group {
    /// Full path in the HDF5 hierarchy (e.g., "/data/measurements").
    pub path: String,
    /// Dataset names within this group.
    pub datasets: Vec<String>,
    /// Group-level attributes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<(String, AttributeValue)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetCdfMetadata {
    /// CF conventions string (e.g., "CF-1.8").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conventions: Option<String>,
    /// Unlimited (record) dimension names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unlimited_dimensions: Vec<String>,
    /// Variable names.
    pub variables: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FitsMetadata {
    /// Number of Header Data Units.
    pub num_hdus: usize,
    /// BITPIX value of the primary HDU (bits per pixel).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_bitpix: Option<i32>,
    /// Header cards from the primary HDU.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_cards: Vec<FitsHeaderCard>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FitsHeaderCard {
    pub keyword: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

// ============================================================================
// Zarr formats
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ZarrMetadata {
    /// Zarr format version (2 or 3).
    pub zarr_version: u8,
    /// Number of arrays in the hierarchy.
    pub num_arrays: usize,
    /// Number of groups in the hierarchy.
    pub num_groups: usize,
    /// Hierarchical structure of the store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hierarchy: Vec<ZarrNode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ZarrNode {
    /// Path within the store (e.g., "/", "/group1/array1").
    pub path: String,
    /// Whether this node is an array (false = group).
    pub is_array: bool,
    /// Array metadata (present only for arrays).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub array_meta: Option<ZarrArrayMeta>,
    /// Node attributes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<(String, AttributeValue)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ZarrArrayMeta {
    /// Array shape (e.g., [100, 200]).
    pub shape: Vec<u64>,
    /// Data type string (e.g., "float32", "int64").
    pub data_type: String,
    /// Chunk shape (e.g., [10, 20]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunk_shape: Vec<u64>,
    /// Codec chain names (e.g., ["bytes", "gzip"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codecs: Vec<String>,
    /// Fill value as string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill_value: Option<String>,
    /// Dimension names (e.g., ["time", "lat", "lon"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimension_names: Vec<String>,
}

// ============================================================================
// VTK / Mesh formats
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VtkMetadata {
    /// VTK file format version (e.g., "4.2").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Title from file header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Dataset type (e.g., "UnstructuredGrid", "PolyData").
    pub dataset_type: String,
    /// Number of points in the mesh.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_points: Option<u64>,
    /// Number of cells in the mesh.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_cells: Option<u64>,
    /// Point data arrays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub point_data: Vec<VtkDataArray>,
    /// Cell data arrays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cell_data: Vec<VtkDataArray>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VtkDataArray {
    /// Array name.
    pub name: String,
    /// Number of components per tuple (1=scalar, 3=vector).
    pub num_components: u32,
    /// Number of tuples.
    pub num_tuples: u64,
    /// Data type (e.g., "float64", "int32").
    pub data_type: String,
}

// ============================================================================
// Media formats
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageMetadata {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Color space (e.g., "RGB", "RGBA", "Grayscale", "CMYK").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_space: Option<String>,
    /// Bits per channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    /// Number of color channels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    /// Whether the image is animated (e.g., animated GIF/WebP/PNG).
    #[serde(default)]
    pub animated: bool,
    /// Number of frames for animated images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_count: Option<u32>,
    /// DPI / resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpi: Option<f64>,
    /// Compression method (e.g., "lossless", "lossy", "none").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioMetadata {
    /// Duration in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    /// Sample rate in Hz (e.g., 44100, 48000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// Number of audio channels (1 = mono, 2 = stereo).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    /// Bits per sample (e.g., 16, 24, 32).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    /// Bitrate in kbps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    /// Codec name (e.g., "mp3", "flac", "aac", "vorbis").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoMetadata {
    /// Video width in pixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Video height in pixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Duration in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    /// Frames per second.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    /// Video codec (e.g., "h264", "h265", "vp9", "av1").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    /// Audio codec of the primary audio track.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
    /// Total bitrate in kbps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    /// Number of audio tracks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_tracks: Option<u32>,
    /// Number of subtitle tracks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle_tracks: Option<u32>,
}

// ============================================================================
// Archive / compression formats
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArchiveMetadata {
    /// Number of entries (files + directories) in the archive.
    /// Reports the true total even if the entries list is truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_entries: Option<u64>,
    /// Total uncompressed size of all entries in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_uncompressed_size: Option<u64>,
    /// Total compressed size of all entries in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_compressed_size: Option<u64>,
    /// Primary compression method (e.g., "deflate", "stored", "lzma", "zstd").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression: Option<String>,
    /// Whether any entries are encrypted.
    #[serde(default)]
    pub encrypted: bool,
    /// Archive-level comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Shallow catalog of archive entries.
    /// May be truncated for very large archives (see `num_entries` for true total).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<ArchiveEntry>,
}

/// A single entry in an archive's shallow catalog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    /// Path relative to the archive root.
    pub path: String,
    /// Uncompressed size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncompressed_size: Option<u64>,
    /// Compressed size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compressed_size: Option<u64>,
    /// Compression method for this entry (e.g., "deflate", "stored").
    pub compression: String,
    /// Whether this entry is a directory.
    #[serde(default)]
    pub is_dir: bool,
    /// Detected file format from the entry's extension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<FileFormat>,
    /// Last modification timestamp (treated as UTC).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether this entry is encrypted.
    #[serde(default)]
    pub encrypted: bool,
}

// ============================================================================
// Text / markup / configuration formats
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TomlMetadata {
    /// Number of top-level tables (sections).
    pub num_tables: usize,
    /// Total number of key-value pairs.
    pub num_keys: usize,
    /// Maximum nesting depth.
    pub max_depth: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct YamlMetadata {
    /// Number of YAML documents in the file.
    pub num_documents: usize,
    /// Total number of top-level keys across all documents.
    pub num_keys: usize,
    /// Maximum nesting depth.
    pub max_depth: usize,
    /// Whether YAML anchors/aliases are used.
    #[serde(default)]
    pub has_anchors: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkdownMetadata {
    /// Heading hierarchy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headings: Vec<MarkdownHeading>,
    /// Word count (excluding code blocks).
    pub word_count: usize,
    /// Line count.
    pub line_count: usize,
    /// Number of code blocks.
    pub code_blocks: usize,
    /// Programming languages found in fenced code blocks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub code_languages: Vec<String>,
    /// Number of links.
    pub link_count: usize,
    /// Number of images.
    pub image_count: usize,
    /// Whether YAML/TOML front matter is present.
    #[serde(default)]
    pub has_front_matter: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkdownHeading {
    pub level: u8,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct XmlMetadata {
    /// Root element name.
    pub root_element: String,
    /// Namespace declarations (prefix, URI).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub namespaces: Vec<(String, String)>,
    /// Total number of elements.
    pub num_elements: usize,
    /// Total number of attributes across all elements.
    pub num_attributes: usize,
    /// Maximum depth of element nesting.
    pub max_depth: usize,
    /// Processing instructions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processing_instructions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HtmlMetadata {
    /// Document title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Number of heading elements (h1-h6).
    pub num_headings: usize,
    /// Number of links.
    pub num_links: usize,
    /// Number of script tags.
    pub num_scripts: usize,
    /// Number of stylesheet links.
    pub num_stylesheets: usize,
    /// Number of images.
    pub num_images: usize,
    /// Number of forms.
    pub num_forms: usize,
    /// Number of tables.
    pub num_tables: usize,
    /// Meta tags as (name/property, content) pairs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meta_tags: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IniMetadata {
    /// Number of sections.
    pub num_sections: usize,
    /// Section names in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub section_names: Vec<String>,
    /// Total number of key-value pairs.
    pub num_keys: usize,
    /// Number of comment lines.
    pub num_comments: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvMetadata {
    /// Number of environment variables defined.
    pub num_variables: usize,
    /// Number of comment lines.
    pub num_comments: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TxtMetadata {
    /// Number of lines.
    pub line_count: usize,
    /// Number of whitespace-delimited words.
    pub word_count: usize,
    /// Number of characters (Unicode scalar values).
    pub char_count: usize,
    /// Length of the longest line in characters.
    pub max_line_length: usize,
    /// Average line length in characters.
    pub avg_line_length: f64,
    /// Whether the file starts with a UTF-8 BOM.
    pub has_bom: bool,
    /// Whether the file contains non-ASCII bytes.
    pub non_ascii: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_metadata_tagged_serialization() {
        let csv = FormatMetadata::Csv(CsvMetadata {
            delimiter: ',',
            quote_char: Some('"'),
            has_header: true,
            encoding: "utf-8".into(),
            comment_lines: 0,
        });

        let json = serde_json::to_string(&csv).unwrap();
        assert!(json.contains(r#""format":"Csv""#));
        assert!(json.contains(r#""details":"#));

        let back: FormatMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(csv, back);
    }

    #[test]
    fn image_metadata_round_trip() {
        let img = FormatMetadata::Image(ImageMetadata {
            width: 1920,
            height: 1080,
            color_space: Some("RGB".into()),
            bit_depth: Some(8),
            channels: Some(3),
            animated: false,
            frame_count: None,
            dpi: Some(72.0),
            compression: Some("lossy".into()),
        });

        let json = serde_json::to_string(&img).unwrap();
        let back: FormatMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(img, back);
    }

    #[test]
    fn audio_metadata_round_trip() {
        let audio = FormatMetadata::Audio(AudioMetadata {
            duration_secs: Some(245.5),
            sample_rate: Some(44100),
            channels: Some(2),
            bit_depth: Some(16),
            bitrate_kbps: Some(320),
            codec: Some("mp3".into()),
        });

        let json = serde_json::to_string(&audio).unwrap();
        let back: FormatMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(audio, back);
    }

    #[test]
    fn video_metadata_round_trip() {
        let video = FormatMetadata::Video(VideoMetadata {
            width: Some(3840),
            height: Some(2160),
            duration_secs: Some(7200.0),
            fps: Some(23.976),
            video_codec: Some("h265".into()),
            audio_codec: Some("aac".into()),
            bitrate_kbps: Some(15000),
            audio_tracks: Some(2),
            subtitle_tracks: Some(3),
        });

        let json = serde_json::to_string(&video).unwrap();
        let back: FormatMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(video, back);
    }

    #[test]
    fn archive_metadata_round_trip() {
        let archive = FormatMetadata::Archive(ArchiveMetadata {
            num_entries: Some(42),
            total_uncompressed_size: Some(1024 * 1024 * 50),
            total_compressed_size: Some(1024 * 1024 * 20),
            compression: Some("deflate".into()),
            encrypted: false,
            comment: Some("test archive".into()),
            entries: vec![
                ArchiveEntry {
                    path: "data/report.csv".into(),
                    uncompressed_size: Some(1024),
                    compressed_size: Some(512),
                    compression: "deflate".into(),
                    is_dir: false,
                    format: Some(FileFormat::Csv),
                    modified_at: None,
                    encrypted: false,
                },
                ArchiveEntry {
                    path: "data/".into(),
                    uncompressed_size: None,
                    compressed_size: None,
                    compression: "stored".into(),
                    is_dir: true,
                    format: None,
                    modified_at: None,
                    encrypted: false,
                },
            ],
        });

        let json = serde_json::to_string(&archive).unwrap();
        let back: FormatMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(archive, back);
    }

    #[test]
    fn spreadsheet_metadata_round_trip() {
        let ss = FormatMetadata::Spreadsheet(SpreadsheetMetadata {
            num_sheets: 2,
            sheets: vec![
                SheetInfo {
                    name: "Sheet1".into(),
                    num_rows: 100,
                    num_columns: 5,
                    column_names: vec!["id".into(), "name".into()],
                },
                SheetInfo {
                    name: "Sheet2".into(),
                    num_rows: 50,
                    num_columns: 3,
                    column_names: vec![],
                },
            ],
        });

        let json = serde_json::to_string(&ss).unwrap();
        let back: FormatMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(ss, back);
    }

    #[test]
    fn mime_type_mapping() {
        assert_eq!(FileFormat::Csv.mime_type(), "text/csv");
        assert_eq!(FileFormat::Json.mime_type(), "application/json");
        assert_eq!(
            FileFormat::Parquet.mime_type(),
            "application/vnd.apache.parquet"
        );
        assert_eq!(
            FileFormat::Arrow.mime_type(),
            "application/vnd.apache.arrow.file"
        );
        assert_eq!(
            FileFormat::Xlsx.mime_type(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(FileFormat::Xls.mime_type(), "application/vnd.ms-excel");
        assert_eq!(
            FileFormat::Ods.mime_type(),
            "application/vnd.oasis.opendocument.spreadsheet"
        );
        assert_eq!(FileFormat::Hdf5.mime_type(), "application/x-hdf5");
        assert_eq!(FileFormat::NetCdf.mime_type(), "application/x-netcdf");
        assert_eq!(FileFormat::Fits.mime_type(), "application/fits");
        assert_eq!(FileFormat::ZarrV2.mime_type(), "application/x-zarr");
        assert_eq!(FileFormat::ZarrV3.mime_type(), "application/x-zarr");
        assert_eq!(FileFormat::VtkLegacy.mime_type(), "application/x-vtk");
        assert_eq!(FileFormat::Vtu.mime_type(), "application/x-vtk");
        assert_eq!(FileFormat::Vtp.mime_type(), "application/x-vtk");
        assert_eq!(FileFormat::Vts.mime_type(), "application/x-vtk");
        assert_eq!(FileFormat::Vtr.mime_type(), "application/x-vtk");
        assert_eq!(FileFormat::Vti.mime_type(), "application/x-vtk");
        assert_eq!(FileFormat::Jpeg.mime_type(), "image/jpeg");
        assert_eq!(FileFormat::Png.mime_type(), "image/png");
        assert_eq!(FileFormat::Tiff.mime_type(), "image/tiff");
        assert_eq!(FileFormat::WebP.mime_type(), "image/webp");
        assert_eq!(FileFormat::Gif.mime_type(), "image/gif");
        assert_eq!(FileFormat::Bmp.mime_type(), "image/bmp");
        assert_eq!(FileFormat::Mp3.mime_type(), "audio/mpeg");
        assert_eq!(FileFormat::Flac.mime_type(), "audio/flac");
        assert_eq!(FileFormat::Wav.mime_type(), "audio/wav");
        assert_eq!(FileFormat::Ogg.mime_type(), "audio/ogg");
        assert_eq!(FileFormat::Aac.mime_type(), "audio/aac");
        assert_eq!(FileFormat::Mp4.mime_type(), "video/mp4");
        assert_eq!(FileFormat::Mkv.mime_type(), "video/x-matroska");
        assert_eq!(FileFormat::Avi.mime_type(), "video/x-msvideo");
        assert_eq!(FileFormat::WebM.mime_type(), "video/webm");
        assert_eq!(FileFormat::Zip.mime_type(), "application/zip");
        assert_eq!(FileFormat::Tar.mime_type(), "application/x-tar");
        assert_eq!(FileFormat::Gzip.mime_type(), "application/gzip");
        assert_eq!(FileFormat::Bzip2.mime_type(), "application/x-bzip2");
        assert_eq!(FileFormat::Xz.mime_type(), "application/x-xz");
        assert_eq!(FileFormat::Zstd.mime_type(), "application/zstd");
        assert_eq!(
            FileFormat::SevenZip.mime_type(),
            "application/x-7z-compressed"
        );
        assert_eq!(FileFormat::Rar.mime_type(), "application/vnd.rar");
        assert_eq!(
            FileFormat::Unknown("custom".into()).mime_type(),
            "application/octet-stream"
        );
    }

    #[test]
    fn zarr_metadata_round_trip() {
        let zarr = FormatMetadata::Zarr(ZarrMetadata {
            zarr_version: 3,
            num_arrays: 2,
            num_groups: 1,
            hierarchy: vec![
                ZarrNode {
                    path: "/".into(),
                    is_array: false,
                    array_meta: None,
                    attributes: vec![("description".into(), AttributeValue::String("test".into()))],
                },
                ZarrNode {
                    path: "/temperature".into(),
                    is_array: true,
                    array_meta: Some(ZarrArrayMeta {
                        shape: vec![100, 200],
                        data_type: "float32".into(),
                        chunk_shape: vec![10, 20],
                        codecs: vec!["bytes".into(), "gzip".into()],
                        fill_value: Some("0".into()),
                        dimension_names: vec!["lat".into(), "lon".into()],
                    }),
                    attributes: vec![],
                },
            ],
        });

        let json = serde_json::to_string(&zarr).unwrap();
        let back: FormatMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(zarr, back);
    }

    #[test]
    fn vtk_metadata_round_trip() {
        let vtk = FormatMetadata::Vtk(VtkMetadata {
            version: Some("4.2".into()),
            title: Some("test mesh".into()),
            dataset_type: "UnstructuredGrid".into(),
            num_points: Some(100),
            num_cells: Some(50),
            point_data: vec![VtkDataArray {
                name: "pressure".into(),
                num_components: 1,
                num_tuples: 100,
                data_type: "float64".into(),
            }],
            cell_data: vec![VtkDataArray {
                name: "temperature".into(),
                num_components: 1,
                num_tuples: 50,
                data_type: "float32".into(),
            }],
        });

        let json = serde_json::to_string(&vtk).unwrap();
        let back: FormatMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(vtk, back);
    }

    #[test]
    fn file_format_round_trip() {
        for fmt in [
            FileFormat::Csv,
            FileFormat::Parquet,
            FileFormat::Xlsx,
            FileFormat::Xls,
            FileFormat::Ods,
            FileFormat::Hdf5,
            FileFormat::ZarrV2,
            FileFormat::ZarrV3,
            FileFormat::VtkLegacy,
            FileFormat::Vtu,
            FileFormat::Vtp,
            FileFormat::Jpeg,
            FileFormat::Mp3,
            FileFormat::Mp4,
            FileFormat::Zip,
            FileFormat::Tar,
            FileFormat::Gzip,
            FileFormat::SevenZip,
            FileFormat::Unknown("custom".into()),
        ] {
            let json = serde_json::to_string(&fmt).unwrap();
            let back: FileFormat = serde_json::from_str(&json).unwrap();
            assert_eq!(fmt, back);
        }
    }
}
