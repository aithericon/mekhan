# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2025-02-03

### Added

- Core metadata types (`FileMetadata`, `ColumnInfo`, `DataType`, `FormatMetadata`)
- Format detection via magic bytes and file extensions (`detect_format`)
- **Tabular backends**: CSV/TSV, JSON/JSONL, Parquet, Arrow IPC
- **Spreadsheet backends**: XLSX, XLS, ODS (via calamine)
- **Scientific backends**: HDF5, NetCDF (via libnetcdf), Zarr V2/V3 (pure Rust), FITS
- **Mesh backends**: VTK Legacy, VTU, VTP, VTS, VTR, VTI (pure Rust)
- **Image backend**: JPEG, PNG, TIFF, WebP, GIF, BMP (dimensions + EXIF)
- **Audio backend**: MP3, FLAC, WAV, OGG, AAC (via symphonia)
- **Video backend**: MP4, MKV, AVI, WebM (via symphonia)
- **Archive backend**: ZIP (entry catalog, compression stats)
- **Text/config backends**: TOML, YAML, Markdown, XML, HTML, INI, .env, plain text
- Column-level statistics (`compute_statistics`) for CSV, JSON, Parquet, Excel
- Content preview extraction (`extract_metadata_with_preview`) for tabular formats
- Data quality scoring (`compute_quality`)
- Schema fingerprinting (`compute_schema_fingerprint`)
- Schema diffing (`diff_schema`)
- Duplicate file detection (`find_duplicates`)
- PII/semantic column classification (`classify_columns`, `classify_semantic`)
- File checksums (SHA-256, BLAKE3)
- Batch extraction (`extract_all`, `extract_all_parallel`)
- Async wrappers (`extract_metadata_async`, `extract_all_async`)
- CLI tool (`fmeta`) with JSON/compact output
- All backends individually feature-gated for minimal dependency footprint
