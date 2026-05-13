# fmeta

Unified file metadata extraction for scientific, tabular, and media data formats. Extracts structural metadata (schemas, dimensions, format-specific details) without reading full file contents. Output types are optimized for PostgreSQL JSONB storage and querying.

## Quick start

```toml
[dependencies]
fmeta = { version = "0.1", features = ["csv", "json", "parquet"] }
```

```rust
use fmeta::extract_metadata;

let meta = extract_metadata(Path::new("data.parquet"))?;
println!("format: {:?}", meta.format);
println!("mime: {:?}", meta.mime_type);
println!("rows: {:?}", meta.num_rows);
println!("columns: {:?}", meta.column_names);
```

## Features

Each backend is behind its own feature flag. Enable only what you need:

| Feature | Backends enabled | Crate dependency |
|---------|-----------------|------------------|
| `csv` | CSV, TSV | `csv` |
| `json` | JSON, JSONL/NDJSON | _(none, uses serde_json)_ |
| `parquet` | Apache Parquet | `parquet`, `bytes` |
| `image` | JPEG, PNG, GIF, BMP, TIFF, WebP | `imagesize`, `kamadak-exif` |
| `audio` | MP3, FLAC, WAV, OGG, AAC | `symphonia` |
| `video` | MP4, MKV, AVI, WebM | `symphonia` |
| `zip` | ZIP archives | `zip` |
| `excel` | XLSX, XLS, XLSB, ODS | `calamine` |
| `arrow` | Arrow IPC / Feather | `arrow-ipc`, `arrow-schema` |
| `netcdf` | NetCDF, HDF5 | `netcdf` (requires system libnetcdf/libhdf5) |
| `zarr` | Zarr V2/V3 stores | `zarrs` (pure Rust) |
| `vtk` | VTK legacy + XML mesh files | `vtkio` (pure Rust) |
| `toml` | TOML config files | `toml` (pure Rust) |
| `yaml` | YAML documents | `serde_yaml` |
| `markdown` | Markdown documents | `pulldown-cmark` |
| `xml` | XML documents | `quick-xml` (pure Rust) |
| `html` | HTML documents | `scraper` |
| `ini` | INI config files | _(none)_ |
| `env` | .env files | _(none)_ |
| `config` | All config backends (toml, yaml, ini, env) | |
| `markup` | All markup backends (markdown, xml, html) | |
| `classify` | PII column classification + semantic/domain classification | `regex` |
| `tokio` | Async wrappers | `tokio` |
| `checksum-sha256` | SHA-256 file checksums | `sha2` |
| `checksum-blake3` | BLAKE3 file checksums | `blake3` |
| `checksum` | Alias for `checksum-sha256` | |
| `rayon` | Parallel batch extraction | `rayon` |
| `cli` | `fmeta` CLI binary | `clap` + all backends + checksum + classify |
| `all-backends` | All format backends (not tokio/checksum/classify) | |

## Supported formats

### Tabular / columnar

| Format | Extensions | Magic bytes | Feature | Extractor |
|--------|-----------|-------------|---------|-----------|
| CSV/TSV | `.csv`, `.tsv`, `.tab` | _(extension only)_ | `csv` | `CsvExtractor` |
| JSON | `.json`, `.jsonl`, `.ndjson` | _(extension only)_ | `json` | `JsonExtractor` |
| Parquet | `.parquet`, `.pq` | `PAR1` | `parquet` | `ParquetExtractor` |
| Arrow IPC | `.arrow`, `.arrows`, `.ipc`, `.feather` | `ARROW1` | `arrow` | `ArrowExtractor` |

### Spreadsheet

| Format | Extensions | Magic bytes | Feature | Extractor |
|--------|-----------|-------------|---------|-----------|
| Excel (OOXML) | `.xlsx`, `.xlsm`, `.xlsb` | ZIP + extension | `excel` | `ExcelExtractor` |
| Excel (legacy) | `.xls` | OLE2 `D0 CF 11 E0` | `excel` | `ExcelExtractor` |
| OpenDocument | `.ods` | ZIP + extension | `excel` | `ExcelExtractor` |

### Scientific data

| Format | Extensions | Magic bytes | Feature | Extractor |
|--------|-----------|-------------|---------|-----------|
| NetCDF | `.nc`, `.nc4`, `.netcdf` | `CDF` or HDF5 signature | `netcdf` | `NetCdfExtractor` |
| HDF5 | `.h5`, `.hdf5`, `.he5` | `\x89HDF\r\n\x1a\n` | `netcdf` | `Hdf5Extractor` |
| Zarr V3 | `.zarr` (directory) | `zarr.json` marker | `zarr` | `ZarrExtractor` |
| Zarr V2 | `.zarr` (directory) | `.zgroup` / `.zarray` marker | `zarr` | `ZarrExtractor` |

### Mesh / simulation

| Format | Extensions | Magic bytes | Feature | Extractor |
|--------|-----------|-------------|---------|-----------|
| VTK Legacy | `.vtk` | `# vtk DataFile Version` | `vtk` | `VtkExtractor` |
| VTK UnstructuredGrid | `.vtu` | `<VTKFile type="UnstructuredGrid">` | `vtk` | `VtkExtractor` |
| VTK PolyData | `.vtp` | `<VTKFile type="PolyData">` | `vtk` | `VtkExtractor` |
| VTK StructuredGrid | `.vts` | `<VTKFile type="StructuredGrid">` | `vtk` | `VtkExtractor` |
| VTK RectilinearGrid | `.vtr` | `<VTKFile type="RectilinearGrid">` | `vtk` | `VtkExtractor` |
| VTK ImageData | `.vti` | `<VTKFile type="ImageData">` | `vtk` | `VtkExtractor` |

### Text & configuration

| Format | Extensions | Magic bytes | Feature | Extractor |
|--------|-----------|-------------|---------|-----------|
| TOML | `.toml` | _(extension only)_ | `toml` | `TomlExtractor` |
| YAML | `.yaml`, `.yml` | _(extension only)_ | `yaml` | `YamlExtractor` |
| INI | `.ini` | _(extension only)_ | `ini` | `IniExtractor` |
| Env | `.env` | _(extension only)_ | `env` | `EnvExtractor` |

### Markup

| Format | Extensions | Magic bytes | Feature | Extractor |
|--------|-----------|-------------|---------|-----------|
| Markdown | `.md`, `.markdown` | _(extension only)_ | `markdown` | `MarkdownExtractor` |
| XML | `.xml`, `.xsl`, `.xsd`, `.svg` | `<?xml` | `xml` | `XmlExtractor` |
| HTML | `.html`, `.htm`, `.xhtml` | `<!DOCTYPE html`, `<html` | `html` | `HtmlExtractor` |

### Astronomical

| Format | Extensions | Magic bytes | Feature | Extractor |
|--------|-----------|-------------|---------|-----------|
| FITS | `.fits`, `.fit`, `.fts` | `SIMPLE` prefix | _(detection only)_ | _(none yet)_ |

### Image

| Format | Extensions | Magic bytes | Feature |
|--------|-----------|-------------|---------|
| JPEG | `.jpg`, `.jpeg`, `.jpe` | `FF D8 FF` | `image` |
| PNG | `.png` | `89 50 4E 47` | `image` |
| GIF | `.gif` | `GIF8` | `image` |
| BMP | `.bmp` | `BM` | `image` |
| TIFF | `.tif`, `.tiff` | `II`/`MM` + 42 | `image` |
| WebP | `.webp` | `RIFF....WEBP` | `image` |

### Audio

| Format | Extensions | Magic bytes | Feature |
|--------|-----------|-------------|---------|
| MP3 | `.mp3` | `ID3` or MPEG sync | `audio` |
| FLAC | `.flac` | `fLaC` | `audio` |
| WAV | `.wav`, `.wave` | `RIFF....WAVE` | `audio` |
| OGG | `.ogg`, `.oga` | `OggS` | `audio` |
| AAC/M4A | `.aac`, `.m4a` | `ftyp` + extension | `audio` |

### Video

| Format | Extensions | Magic bytes | Feature |
|--------|-----------|-------------|---------|
| MP4 | `.mp4`, `.m4v` | `ftyp` box | `video` |
| Matroska | `.mkv`, `.mka` | EBML `1A 45 DF A3` | `video` |
| AVI | `.avi` | `RIFF....AVI ` | `video` |
| WebM | `.webm` | EBML + extension | `video` |

### Archive / compression (detection only unless noted)

| Format | Extensions | Magic bytes | Backend |
|--------|-----------|-------------|---------|
| ZIP | `.zip`, `.jar`, `.war`, `.ear`, `.epub`, `.apk` | `PK\x03\x04` | `zip` feature |
| Tar | `.tar` | `ustar` at offset 257 | detection only |
| Gzip | `.gz`, `.gzip`, `.tgz` | `1F 8B` | detection only |
| BZip2 | `.bz2`, `.tbz2` | `BZh` | detection only |
| XZ | `.xz`, `.txz` | `FD 37 7A 58 5A 00` | detection only |
| Zstd | `.zst`, `.zstd` | `28 B5 2F FD` | detection only |
| 7-Zip | `.7z` | `37 7A BC AF 27 1C` | detection only |
| RAR | `.rar` | `Rar!\x1A\x07` | detection only |

## Extracted schema

All backends produce a unified `FileMetadata` struct. Top-level scalar fields are promoted for efficient PostgreSQL JSONB indexing.

### `FileMetadata`

```rust
pub struct FileMetadata {
    pub format: FileFormat,             // Detected file format
    pub mime_type: Option<String>,      // MIME type (e.g. "text/csv")

    // Promoted scalars (JSONB-indexable)
    pub num_rows: Option<u64>,          // Row count (tabular/spreadsheet)
    pub num_columns: Option<u64>,       // Column/field count
    pub file_size_bytes: Option<u64>,   // File size on disk

    // Filesystem metadata
    pub file_name: Option<String>,      // File name without directory path
    pub modified_at: Option<DateTime<Utc>>,  // Last modification time
    pub created_at: Option<DateTime<Utc>>,   // Creation time (platform-dependent)
    pub readonly: bool,                 // Read-only flag
    pub unix_mode: Option<u32>,         // Unix permission bits (Unix only)

    // Queryable shortcuts
    pub column_names: Vec<String>,      // Flat list for `? 'col_name'` queries

    // Structural data
    pub dimensions: Vec<Dimension>,     // Named dimensions with sizes
    pub columns: Vec<ColumnInfo>,       // Full column schema with types

    // Extensible key-value metadata
    pub attributes: HashMap<String, AttributeValue>,

    // Format-specific details (tagged enum)
    pub format_specific: Option<FormatMetadata>,

    // Content preview (tabular formats only)
    pub preview: Option<ContentPreview>,

    // Encryption status
    pub encrypted: Option<bool>,        // None=N/A, Some(false)=checked, Some(true)=encrypted

    // File integrity
    pub checksum: Option<ChecksumInfo>,

    // Schema fingerprint (auto-computed by extract_metadata)
    pub schema_fingerprint: Option<SchemaFingerprint>,

    // Data quality scores (computed via compute_quality after statistics)
    pub data_quality: Option<DataQualityReport>,

    pub extracted_at: DateTime<Utc>,
}
```

Filesystem fields (`file_name`, `modified_at`, `created_at`, `readonly`, `unix_mode`) are populated automatically by `extract_metadata()`. When using a backend's `MetadataExtractor::extract()` directly, call `meta.populate_fs_metadata(path)` on the result.

`mime_type` is set automatically by `extract_metadata()` based on the detected format.

### `ColumnInfo`

Per-column schema extracted from tabular and scientific formats:

```rust
pub struct ColumnInfo {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub metadata: HashMap<String, AttributeValue>,
    pub statistics: Option<ColumnStatistics>,    // Populated by compute_statistics()
    pub classifications: Vec<ClassificationTag>, // Populated by classify_columns() and/or classify_semantic()
}
```

### `DataType`

Cross-format type system aligned with Arrow's vocabulary:

| Type | Description |
|------|-------------|
| `Boolean` | True/false |
| `Int8`..`Int64`, `UInt8`..`UInt64` | Signed/unsigned integers |
| `Float32`, `Float64` | IEEE floating point |
| `String` | UTF-8 text |
| `Binary` | Variable-length bytes |
| `Timestamp { timezone }` | Timestamp with optional timezone |
| `Date` | Calendar date |
| `Time` | Time of day |
| `Duration` | Time interval |
| `List(inner)` | Ordered list of a single element type |
| `Struct(fields)` | Named fields (nested) |
| `Dictionary { index, value }` | Dictionary-encoded |
| `Unknown(name)` | Unclassified type |

Type inference for CSV and Excel samples the first 100 rows (configurable). Widening rules: Int + Float -> Float, mixed -> String.

### `FormatMetadata` variants

The `format_specific` field carries typed metadata per format. Serialized as `{"format": "Csv", "details": {...}}` for clean JSONB queries.

#### `Csv`

```json
{
  "delimiter": ",",
  "quote_char": "\"",
  "has_header": true,
  "encoding": "utf-8",
  "comment_lines": 0
}
```

#### `Parquet`

```json
{
  "num_row_groups": 4,
  "num_rows": 1000000,
  "compression": "SNAPPY",
  "created_by": "parquet-rs version 53.0.0",
  "version": 2,
  "row_groups": [
    { "num_rows": 250000, "total_byte_size": 1048576, "columns": [...] }
  ]
}
```

#### `Arrow`

```json
{
  "num_record_batches": 3,
  "schema_fields": ["id", "name", "value"]
}
```

#### `Spreadsheet` (Excel, ODS)

Top-level `num_rows`/`num_columns`/`column_names`/`columns` are populated from the first sheet. Per-sheet details are in `format_specific`:

```json
{
  "num_sheets": 2,
  "sheets": [
    { "name": "Sheet1", "num_rows": 100, "num_columns": 5, "column_names": ["id", "name"] },
    { "name": "Sheet2", "num_rows": 50, "num_columns": 3 }
  ]
}
```

#### `NetCdf`

```json
{
  "conventions": "CF-1.8",
  "unlimited_dimensions": ["time"],
  "variables": ["temperature", "pressure", "humidity"]
}
```

Dimensions are promoted to top-level `dimensions`. Variables become `columns` with NetCDF dtype mapped to `DataType`. Root-level attributes are extracted to `attributes`.

#### `Hdf5`

```json
{
  "groups": [
    {
      "path": "/",
      "datasets": ["temperature", "pressure"],
      "attributes": [["conventions", "CF-1.8"]]
    },
    {
      "path": "/measurements",
      "datasets": ["voltage", "current"],
      "attributes": []
    }
  ]
}
```

Opened through `libnetcdf`'s NC4 driver. Hierarchical group structure, dataset names, and group-level attributes are extracted. Datasets become `columns` with HDF5 dtype mapped to `DataType`. Dimensions are derived from the first dataset's shape.

#### `Fits`

```json
{
  "num_hdus": 3,
  "primary_bitpix": -32,
  "header_cards": [
    { "keyword": "SIMPLE", "value": "T", "comment": "Standard FITS" },
    { "keyword": "NAXIS", "value": "2", "comment": "Number of axes" }
  ]
}
```

FITS format is detected via magic bytes (`SIMPLE` prefix) and extensions (`.fits`, `.fit`, `.fts`). Metadata types are defined but no extractor backend is implemented yet.

#### `Zarr`

```json
{
  "zarr_version": 3,
  "num_arrays": 2,
  "num_groups": 1,
  "hierarchy": [
    {
      "path": "/",
      "is_array": false,
      "array_meta": null,
      "attributes": []
    },
    {
      "path": "/temperature",
      "is_array": true,
      "array_meta": {
        "shape": [100, 200],
        "data_type": "float32",
        "chunk_shape": [10, 20],
        "codecs": ["bytes", "gzip"],
        "fill_value": "0",
        "dimension_names": ["lat", "lon"]
      },
      "attributes": [["units", "kelvin"]]
    }
  ]
}
```

Arrays become `columns` (name = Zarr path). Dimensions are derived from the first array's shape and dimension names. `num_rows` is the first dimension of the first array.

#### `Vtk`

```json
{
  "version": "4.2",
  "title": "simulation mesh",
  "dataset_type": "UnstructuredGrid",
  "num_points": 1024,
  "num_cells": 2048,
  "point_data": [
    { "name": "pressure", "num_components": 1, "num_tuples": 1024, "data_type": "float64" },
    { "name": "velocity", "num_components": 3, "num_tuples": 1024, "data_type": "float32" }
  ],
  "cell_data": [
    { "name": "temperature", "num_components": 1, "num_tuples": 2048, "data_type": "float64" }
  ]
}
```

Data arrays become `columns` with prefixed names (`point:pressure`, `cell:temperature`). Dimensions include `points` and `cells` counts. Handles all VTK dataset types: UnstructuredGrid, PolyData, StructuredGrid, RectilinearGrid, ImageData, Field.

#### `Image`

```json
{
  "width": 1920,
  "height": 1080,
  "color_space": "RGB",
  "bit_depth": 8,
  "channels": 3,
  "animated": false,
  "dpi": 72.0,
  "compression": "lossy"
}
```

#### `Audio`

```json
{
  "duration_secs": 245.5,
  "sample_rate": 44100,
  "channels": 2,
  "bit_depth": 16,
  "bitrate_kbps": 320,
  "codec": "mp3"
}
```

#### `Video`

```json
{
  "width": 3840,
  "height": 2160,
  "duration_secs": 7200.0,
  "fps": 23.976,
  "video_codec": "h265",
  "audio_codec": "aac",
  "bitrate_kbps": 15000,
  "audio_tracks": 2,
  "subtitle_tracks": 3
}
```

#### `Toml`

```json
{
  "num_tables": 3,
  "num_keys": 12,
  "max_depth": 2
}
```

Top-level keys become `columns` with inferred types (Int64, Float64, Boolean, String, List, Struct). Arrays of tables produce tabular mode with `num_rows` = array length.

#### `Yaml`

```json
{
  "num_documents": 1,
  "num_keys": 8,
  "max_depth": 3,
  "has_anchors": false
}
```

Multi-document YAML (`---` separated) tracks document count. Sequences of mappings produce tabular mode. Anchor usage (`&`) is detected from raw content.

#### `Markdown`

```json
{
  "headings": [
    { "level": 1, "text": "Introduction" },
    { "level": 2, "text": "Getting Started" }
  ],
  "word_count": 1250,
  "line_count": 89,
  "code_blocks": 3,
  "code_languages": ["rust", "bash"],
  "link_count": 5,
  "image_count": 2,
  "has_front_matter": true
}
```

Non-tabular. Words are counted outside code blocks. YAML front matter (`---` delimited) is detected and stripped before parsing.

#### `Xml`

```json
{
  "root_element": "catalog",
  "namespaces": [["", "http://example.com/ns"]],
  "num_elements": 42,
  "num_attributes": 15,
  "max_depth": 4,
  "processing_instructions": ["xml-stylesheet type=\"text/xsl\" href=\"style.xsl\""]
}
```

Non-tabular. Streaming parser counts elements, attributes, and tracks nesting depth. Namespace declarations (`xmlns`) and processing instructions are extracted.

#### `Html`

```json
{
  "title": "My Page",
  "num_headings": 5,
  "num_links": 12,
  "num_scripts": 3,
  "num_stylesheets": 2,
  "num_images": 8,
  "num_forms": 1,
  "num_tables": 2,
  "meta_tags": [
    ["description", "A sample page"],
    ["viewport", "width=device-width"]
  ]
}
```

Non-tabular. DOM parser extracts structural counts and meta tag name/content pairs.

#### `Ini`

```json
{
  "num_sections": 3,
  "section_names": ["database", "server", "logging"],
  "num_keys": 10,
  "num_comments": 2
}
```

Keys are stored as `section.key` in `columns` with inferred types. Global keys (before any section header) have no prefix.

#### `Env`

```json
{
  "num_variables": 5,
  "num_comments": 2
}
```

Variable names become `columns` (all `DataType::String`). Quoted values are unquoted during parsing.

#### `Archive` (ZIP)

```json
{
  "num_entries": 42,
  "total_uncompressed_size": 52428800,
  "total_compressed_size": 20971520,
  "compression": "deflate",
  "encrypted": false,
  "comment": null,
  "entries": [
    {
      "path": "data/report.csv",
      "uncompressed_size": 1024,
      "compressed_size": 512,
      "compression": "deflate",
      "is_dir": false,
      "format": "csv",
      "modified_at": "2024-01-15T10:30:00Z",
      "encrypted": false
    }
  ]
}
```

## Column statistics

`compute_statistics` computes per-column statistics for tabular formats (CSV, JSON, Parquet, Excel):

```rust
use fmeta::{extract_metadata, compute_statistics, StatisticsOptions};

let mut meta = extract_metadata(Path::new("data.csv"))?;
compute_statistics(Path::new("data.csv"), &mut meta, &StatisticsOptions::default())?;

for col in &meta.columns {
    if let Some(stats) = &col.statistics {
        println!("{}: min={:?} max={:?} nulls={:?}",
            col.name, stats.min, stats.max, stats.null_count);
    }
}
```

Or use the convenience function:

```rust
use fmeta::{extract_metadata_with_statistics, StatisticsOptions};

let meta = extract_metadata_with_statistics(
    Path::new("data.csv"),
    &StatisticsOptions::default(),
)?;
```

`StatisticsOptions` controls what to compute:

```rust
pub struct StatisticsOptions {
    pub compute_null_count: bool,      // default: true
    pub compute_distinct_count: bool,  // default: true
    pub compute_min_max: bool,         // default: true
    pub compute_mean: bool,            // default: true
    pub top_k: Option<usize>,         // Most frequent values (default: None)
    pub max_sample_rows: Option<usize>, // Row limit for sampling
}
```

Parquet aggregates statistics from row group metadata (null count, min/max) without reading data. Other formats use a single-pass accumulator.

## PII classification

With the `classify` feature, `classify_columns` scans string columns for sensitive data patterns:

```rust
use fmeta::{extract_metadata, classify_columns, ClassificationOptions};

let mut meta = extract_metadata(Path::new("users.csv"))?;
classify_columns(Path::new("users.csv"), &mut meta, &ClassificationOptions::default())?;

for col in &meta.columns {
    for tag in &col.classifications {
        println!("{}: {} (confidence: {:.0}%)", col.name, tag.category, tag.confidence * 100.0);
    }
}
```

Built-in detection categories:

| PII | Format patterns |
|-----|-----------------|
| `email`, `phone`, `ip_address`, `credit_card`, `ssn`, `url` | `uuid`, `iso_date`, `iso_datetime`, `hex_color`, `semver`, `file_path`, `json_string`, `base64`, `md5_hash`, `sha256_hash`, `ipv6_address`, `mac_address` |

Each tag includes `confidence` (match ratio over sampled rows), `sample_count`, and `match_count`.

### Semantic / domain classification

`classify_semantic` infers domain tags from column name, data type, and statistics (min/max range) — no file I/O required:

```rust
use fmeta::{
    classify_semantic, compute_statistics, extract_metadata,
    ClassificationOptions, StatisticsOptions,
};

let mut meta = extract_metadata(Path::new("geo.csv"))?;
compute_statistics(Path::new("geo.csv"), &mut meta, &StatisticsOptions::default())?;
classify_semantic(&mut meta, &ClassificationOptions::default())?;

for col in &meta.columns {
    for tag in &col.classifications {
        println!("{}: {} (confidence: {:.0}%)", col.name, tag.category, tag.confidence * 100.0);
    }
}
```

Detected semantic types: `latitude`, `longitude`, `percentage`, `unix_timestamp`, `boolean_int`, `year`, `age`.

Confidence scoring:
- Name + type + range all match → **0.9**
- Type + range match, no name match → **0.6**
- Name + type match, no statistics → **0.5**

`ClassificationOptions` controls both pattern-based and semantic classification:

```rust
pub struct ClassificationOptions {
    pub min_confidence: f64,           // Minimum confidence to report (default: 0.5)
    pub max_sample_rows: usize,        // Rows to sample (default: 1000)
    pub categories: Option<Vec<String>>, // Restrict to specific categories
}
```

## Duplicate detection

`find_duplicates` groups files by checksum to identify exact duplicates:

```rust
use fmeta::{extract_all, find_duplicates, ExtractAllOptions, ChecksumAlgorithm};

let results = extract_all(
    Path::new("./data"),
    &ExtractAllOptions::new().with_checksum(ChecksumAlgorithm::Sha256),
)?;
let dupes = find_duplicates(&results);

for group in &dupes {
    println!("Duplicate ({} copies): {:?}", group.paths.len(), group.paths);
}
```

Returns `Vec<DuplicateGroup>`, each containing the shared digest, algorithm, file paths (always >= 2), and file size.

## Schema fingerprinting

Every call to `extract_metadata()` auto-computes a deterministic schema fingerprint — a 16-char hex digest of the column schema (names, types, nullable flags). Two files with identical column definitions produce the same digest regardless of column order.

```rust
let meta = extract_metadata(Path::new("data.csv"))?;
let fp = meta.schema_fingerprint.as_ref().unwrap();
println!("{}", fp.digest); // e.g. "a1b2c3d4e5f67890"
```

Uses FNV-1a 64-bit hash on a canonical form: columns sorted alphabetically as `name:type:nullable`, joined by `|`. No external dependencies.

```sql
-- Find files with the same schema
SELECT a.path, b.path
FROM files a JOIN files b ON
  a.metadata->'schema_fingerprint'->>'digest' = b.metadata->'schema_fingerprint'->>'digest'
WHERE a.path < b.path;
```

## Data quality scores

`compute_quality` derives per-column quality metrics from pre-computed statistics. No file I/O — it reads existing `ColumnStatistics` on the metadata.

```rust
use fmeta::{extract_metadata_with_quality, StatisticsOptions};

let meta = extract_metadata_with_quality(Path::new("data.csv"), &StatisticsOptions::default())?;

if let Some(quality) = &meta.data_quality {
    println!("overall completeness: {:.0}%", quality.completeness * 100.0);
    for col in &quality.column_scores {
        println!("  {}: completeness={:.2}, distinctness={:.2}, score={:.2}",
            col.column_name, col.completeness, col.distinctness, col.score);
    }
}
```

Metrics per column:
- **completeness** — `1.0 - (null_count / row_count)`
- **distinctness** — `distinct_count / non_null_count`
- **score** — `(completeness + distinctness) / 2.0`

The aggregate `completeness` on `DataQualityReport` is the mean across all scored columns.

## Content preview

`extract_metadata_with_preview` extracts metadata and includes the first N rows for tabular formats (CSV, JSON, Parquet, Excel, Arrow):

```rust
use fmeta::{extract_metadata_with_preview, PreviewOptions};

let meta = extract_metadata_with_preview(
    Path::new("data.csv"),
    &PreviewOptions::new().with_max_rows(5),
)?;

if let Some(preview) = &meta.preview {
    println!("columns: {:?}", preview.columns);
    println!("rows: {} of {:?}", preview.preview_row_count, preview.total_row_count);
    for row in &preview.rows {
        println!("  {:?}", row);
    }
}
```

Non-tabular formats (images, audio, video, archives) return `preview: None`.

## Schema comparison

`diff_schema` compares two `FileMetadata` values and reports structural changes:

```rust
use fmeta::{diff_schema, extract_metadata};

let old = extract_metadata(Path::new("data_v1.csv"))?;
let new = extract_metadata(Path::new("data_v2.csv"))?;
let diff = diff_schema(&old, &new);

if diff.is_identical {
    println!("Schemas match");
} else {
    for col_diff in &diff.column_diffs {
        println!("{}: {:?}", col_diff.column_name, col_diff.changes);
    }
    if let Some((old_rows, new_rows)) = diff.row_count_change {
        println!("Row count: {old_rows} -> {new_rows}");
    }
}
```

Detected change types: `Added`, `Removed`, `TypeChanged`, `NullabilityChanged`.

## Checksums

Compute SHA-256 or BLAKE3 file checksums (feature-gated):

```rust
use fmeta::{compute_checksum, ChecksumAlgorithm};

let info = compute_checksum(Path::new("data.csv"), ChecksumAlgorithm::Sha256)?;
println!("{}: {}", info.algorithm, info.digest); // "sha256: a948904f..."
```

Checksums are not auto-populated in `FileMetadata`. To include checksums in batch extraction:

```rust
use fmeta::{extract_all, ExtractAllOptions, ChecksumAlgorithm};

let results = extract_all(
    Path::new("./data"),
    &ExtractAllOptions::new().with_checksum(ChecksumAlgorithm::Sha256),
)?;
// Each FileResult.result.checksum is now populated
```

## Format detection

`detect_format(path)` uses a three-phase strategy:

1. **Directory detection** -- if the path is a directory, checks for Zarr store markers (`zarr.json` for V3, `.zgroup`/`.zarray` for V2).
2. **Magic bytes** -- reads the first 264 bytes and matches binary signatures. Shared containers are disambiguated by extension (ZIP vs XLSX/ODS, EBML vs MKV/WebM, ftyp vs MP4/M4A, OLE2 vs XLS). VTK legacy files match `# vtk DataFile Version`; VTK XML files match `<VTKFile type="...">`. HDF5 signature (`\x89HDF`) is disambiguated by extension between HDF5 and NetCDF. `.zarr.zip` files are detected as Zarr stores.
3. **Extension fallback** -- if no magic bytes match, the file extension determines the format.

You can also call `detect_from_extension(path)` directly for extension-only detection, `detect_format_from_bytes(bytes)` for magic-byte detection from an in-memory buffer, or `is_zarr_directory(path)` to check if a path is a Zarr store.

## Batch extraction

`extract_all` recursively walks a directory and extracts metadata from every recognized file:

```rust
use fmeta::{extract_all, ExtractAllOptions};

let results = extract_all(
    Path::new("./data"),
    &ExtractAllOptions::new()
        .with_max_depth(3)
        .include_hidden(),
)?;

for file_result in &results {
    match &file_result.result {
        Ok(meta) => println!("{}: {:?}", file_result.path.display(), meta.format),
        Err(e) => eprintln!("{}: {e}", file_result.path.display()),
    }
}
```

- Hidden files (starting with `.`) are skipped by default
- Zarr store directories are treated as single extractable units (no recursion into internal structure)
- Results are sorted by path for deterministic output
- Per-file errors are captured in `FileResult.result` without stopping the walk
- Unreadable directories are silently skipped

### Path collection

`collect_paths` returns the list of files that `extract_all` would process, without extracting metadata. Useful for previewing or filtering before extraction:

```rust
use fmeta::{collect_paths, ExtractAllOptions};

let paths = collect_paths(Path::new("./data"), &ExtractAllOptions::new().with_max_depth(2))?;
println!("found {} files", paths.len());
```

### Parallel extraction

With the `rayon` feature, `extract_all_parallel` processes files concurrently:

```rust
use fmeta::{extract_all_parallel, ExtractAllOptions};

let results = extract_all_parallel(Path::new("./data"), &ExtractAllOptions::default())?;
```

Results are sorted by path for deterministic output regardless of processing order. Same API as `extract_all`.

## Reader API

`extract_metadata_from_reader` extracts metadata from any `Read + Seek` source (in-memory buffers, network streams, etc.) without requiring a file path:

```rust
use fmeta::{extract_metadata_from_reader, FormatHint, FileFormat};
use std::io::Cursor;

let data = b"id,name\n1,alice\n2,bob\n";
let cursor = Cursor::new(data.to_vec());

let meta = extract_metadata_from_reader(
    cursor,
    &FormatHint::new().with_format(FileFormat::Csv),
)?;
```

Format is resolved from (in priority order): explicit `hint.format`, magic bytes from the reader, `hint.extension` or extension from `hint.file_name`.

Supported formats: CSV, JSON, Arrow IPC, ZIP, Parquet. Image, audio, video, and Excel require file paths.

## Async API

With the `tokio` feature, async wrappers offload blocking I/O to Tokio's blocking thread pool:

```rust
use fmeta::{extract_metadata_async, extract_all_async, ExtractAllOptions};

let meta = extract_metadata_async(Path::new("data.csv")).await?;
let results = extract_all_async(Path::new("./data"), ExtractAllOptions::default()).await?;
```

With both `tokio` and `rayon`, `extract_all_parallel_async` runs parallel extraction on the blocking pool.

## CLI

The `fmeta` binary provides command-line access to all extraction features. Build with the `cli` feature:

```bash
cargo install fmeta --features cli
```

```bash
# Single file, pretty JSON
fmeta --pretty data.csv

# Directory, recursive
fmeta -r --pretty ./data/

# With checksum and preview
fmeta --pretty --checksum sha256 --preview 5 data.parquet

# Compact JSONL output
fmeta --compact -r ./data/

# Column statistics for tabular formats
fmeta --pretty --statistics data.csv

# PII/pattern classification
fmeta --pretty --classify data.csv

# Find duplicate files (requires --checksum and -r)
fmeta -r --checksum sha256 --find-duplicates ./data/

# Control recursion depth, include hidden files
fmeta -r --max-depth 2 --include-hidden ./data/
```

## PostgreSQL JSONB integration

The schema is designed for direct JSONB storage. Example queries:

```sql
-- Find all CSV files with > 1000 rows
SELECT * FROM files
WHERE metadata->>'format' = 'csv'
  AND (metadata->>'num_rows')::bigint > 1000;

-- Find files containing a 'temperature' column
SELECT * FROM files
WHERE metadata->'column_names' ? 'temperature';

-- Query format-specific details
SELECT metadata->'format_specific'->'details'->>'compression'
FROM files
WHERE metadata->'format_specific'->>'format' = 'Parquet';

-- Find spreadsheets with multiple sheets
SELECT * FROM files
WHERE metadata->'format_specific'->>'format' = 'Spreadsheet'
  AND (metadata->'format_specific'->'details'->>'num_sheets')::int > 1;

-- Find Zarr stores with specific arrays
SELECT * FROM files
WHERE metadata->'format_specific'->>'format' = 'Zarr'
  AND (metadata->'format_specific'->'details'->>'num_arrays')::int > 5;

-- Find VTK meshes with many cells
SELECT * FROM files
WHERE metadata->'format_specific'->>'format' = 'Vtk'
  AND (metadata->'format_specific'->'details'->>'num_cells')::bigint > 100000;

-- Find columns flagged as containing email addresses
SELECT f.path, c->>'name' AS column_name
FROM files f, jsonb_array_elements(f.metadata->'columns') AS c
WHERE c->'classifications' @> '[{"category": "email"}]';

-- Filter by MIME type
SELECT * FROM files
WHERE metadata->>'mime_type' = 'text/csv';

-- Verify file integrity
SELECT * FROM files
WHERE metadata->'checksum'->>'digest' = 'a948904f...';

-- Find files with the same schema fingerprint
SELECT a.path, b.path
FROM files a JOIN files b ON
  a.metadata->'schema_fingerprint'->>'digest' = b.metadata->'schema_fingerprint'->>'digest'
WHERE a.path < b.path;

-- Find files with low data quality
SELECT path, (metadata->'data_quality'->>'completeness')::float AS completeness
FROM files
WHERE (metadata->'data_quality'->>'completeness')::float < 0.9;

-- Find columns tagged as latitude/longitude
SELECT f.path, c->>'name' AS column_name, tag->>'category' AS category
FROM files f,
     jsonb_array_elements(f.metadata->'columns') AS c,
     jsonb_array_elements(c->'classifications') AS tag
WHERE tag->>'category' IN ('latitude', 'longitude');
```

## Architecture

```
src/
  lib.rs               -- Public API: extract_metadata, extract_all, re-exports
  async_api.rs         -- Async wrappers (tokio feature)
  types.rs             -- FileMetadata, ColumnInfo, Dimension, AttributeValue
  format.rs            -- FileFormat enum, FormatMetadata variants, mime_type()
  data_type.rs         -- DataType enum (Arrow-aligned type system)
  detect.rs            -- Magic bytes + extension + directory format detection
  error.rs             -- MetadataError (FileNotFound, Io, UnsupportedFormat, ParseError)
  extractor.rs         -- MetadataExtractor trait
  statistics.rs        -- ColumnStatistics, compute_statistics (per-column stats)
  classify.rs          -- ClassificationTag, classify_columns, classify_semantic (classify feature)
  fingerprint.rs       -- SchemaFingerprint, compute_schema_fingerprint (auto-computed, no deps)
  quality.rs           -- DataQualityReport, ColumnQuality, compute_quality (no deps)
  duplicates.rs        -- DuplicateGroup, find_duplicates (checksum-based dedup)
  checksum.rs          -- ChecksumAlgorithm, ChecksumInfo, compute_checksum
  diff.rs              -- SchemaDiff, ColumnDiff, ColumnChange, diff_schema
  preview.rs           -- ContentPreview, PreviewOptions, per-format preview extraction
  reader_extractor.rs  -- FormatHint, extract_metadata_from_reader (Read+Seek API)
  bin/fmeta.rs         -- CLI binary (cli feature)
  backends/
    media_common.rs    -- Shared symphonia helpers for audio/video backends
    csv.rs             -- CsvExtractor
    json.rs            -- JsonExtractor
    parquet.rs         -- ParquetExtractor
    arrow.rs           -- ArrowExtractor
    image.rs           -- ImageExtractor
    audio.rs           -- AudioExtractor
    video.rs           -- VideoExtractor
    zip.rs             -- ZipExtractor
    excel.rs           -- ExcelExtractor
    netcdf.rs          -- NetCdfExtractor, Hdf5Extractor (netcdf feature, system deps)
    zarr.rs            -- ZarrExtractor (zarr feature, pure Rust)
    vtk.rs             -- VtkExtractor (vtk feature, pure Rust)
    toml.rs            -- TomlExtractor (toml feature, pure Rust)
    yaml.rs            -- YamlExtractor (yaml feature)
    markdown.rs        -- MarkdownExtractor (markdown feature)
    xml.rs             -- XmlExtractor (xml feature, pure Rust)
    html.rs            -- HtmlExtractor (html feature)
    ini.rs             -- IniExtractor (ini feature, no deps)
    env.rs             -- EnvExtractor (env feature, no deps)
```

## Contributing

Issues and pull requests are welcome. Please open an issue to discuss substantial changes before starting work.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work shall be licensed as Apache-2.0, without any additional terms or conditions.
