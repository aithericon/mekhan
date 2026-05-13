use std::path::Path;

use crate::error::MetadataError;
use crate::format::FileFormat;

/// Detect file format from the file extension.
///
/// Returns `None` if the extension is not recognized.
pub fn detect_from_extension(path: &Path) -> Option<FileFormat> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| match ext.to_lowercase().as_str() {
            // Tabular / columnar
            "csv" => FileFormat::Csv,
            "tsv" | "tab" => FileFormat::Csv,
            "parquet" | "pq" => FileFormat::Parquet,
            "json" | "jsonl" | "ndjson" => FileFormat::Json,
            "arrow" | "arrows" | "ipc" | "feather" => FileFormat::Arrow,
            // Scientific
            "h5" | "hdf5" | "he5" => FileFormat::Hdf5,
            "nc" | "nc4" | "netcdf" => FileFormat::NetCdf,
            "fits" | "fit" | "fts" => FileFormat::Fits,
            "zarr" => FileFormat::ZarrV3,
            // Mesh / VTK
            "vtk" => FileFormat::VtkLegacy,
            "vtu" => FileFormat::Vtu,
            "vtp" => FileFormat::Vtp,
            "vts" => FileFormat::Vts,
            "vtr" => FileFormat::Vtr,
            "vti" => FileFormat::Vti,
            // Image
            "jpg" | "jpeg" | "jpe" => FileFormat::Jpeg,
            "png" => FileFormat::Png,
            "tif" | "tiff" => FileFormat::Tiff,
            "webp" => FileFormat::WebP,
            "gif" => FileFormat::Gif,
            "bmp" => FileFormat::Bmp,
            // Audio
            "mp3" => FileFormat::Mp3,
            "flac" => FileFormat::Flac,
            "wav" | "wave" => FileFormat::Wav,
            "ogg" | "oga" => FileFormat::Ogg,
            "aac" | "m4a" => FileFormat::Aac,
            // Video
            "mp4" | "m4v" => FileFormat::Mp4,
            "mkv" | "mka" => FileFormat::Mkv,
            "avi" => FileFormat::Avi,
            "webm" => FileFormat::WebM,
            // Spreadsheet
            "xlsx" | "xlsm" | "xlsb" => FileFormat::Xlsx,
            "xls" => FileFormat::Xls,
            "ods" => FileFormat::Ods,
            // Archive / compression
            "zip" | "jar" | "war" | "ear" | "epub" | "apk" => FileFormat::Zip,
            "tar" => FileFormat::Tar,
            "gz" | "gzip" => FileFormat::Gzip,
            "tgz" => FileFormat::Gzip, // tar.gz with single extension
            "bz2" | "tbz2" => FileFormat::Bzip2,
            "xz" | "txz" => FileFormat::Xz,
            "zst" | "zstd" => FileFormat::Zstd,
            "7z" => FileFormat::SevenZip,
            "rar" => FileFormat::Rar,
            // Text / markup / configuration
            "toml" => FileFormat::Toml,
            "yaml" | "yml" => FileFormat::Yaml,
            "md" | "markdown" => FileFormat::Markdown,
            "xml" | "xsl" | "xsd" | "svg" => FileFormat::Xml,
            "html" | "htm" | "xhtml" => FileFormat::Html,
            "ini" => FileFormat::Ini,
            "env" => FileFormat::Env,
            "txt" | "text" | "log" => FileFormat::Txt,
            // Unknown
            other => FileFormat::Unknown(other.to_string()),
        })
}

/// Check whether a directory is a Zarr store (V2 or V3).
///
/// Returns `true` if the path is a directory containing Zarr marker files
/// (`zarr.json` for V3, `.zgroup` or `.zarray` for V2).
pub fn is_zarr_directory(path: &Path) -> bool {
    path.is_dir()
        && (path.join("zarr.json").exists()
            || path.join(".zgroup").exists()
            || path.join(".zarray").exists())
}

fn detect_directory_format(path: &Path) -> Result<FileFormat, MetadataError> {
    if path.join("zarr.json").exists() {
        return Ok(FileFormat::ZarrV3);
    }
    if path.join(".zgroup").exists() || path.join(".zarray").exists() {
        return Ok(FileFormat::ZarrV2);
    }
    detect_from_extension(path).ok_or_else(|| MetadataError::DetectionFailed(path.to_path_buf()))
}

/// Detect file format using magic bytes, falling back to extension.
///
/// Reads the first 264 bytes of the file to identify binary formats
/// (264 to cover tar's "ustar" magic at offset 257), then falls back
/// to extension-based detection. Also detects Zarr directories.
pub fn detect_format(path: &Path) -> Result<FileFormat, MetadataError> {
    // Zarr stores are directories, not files
    if path.is_dir() {
        return detect_directory_format(path);
    }

    // Try magic bytes for binary formats
    if let Ok(file) = std::fs::File::open(path) {
        use std::io::Read;
        let mut buf = [0u8; 264];
        let mut reader = std::io::BufReader::new(file);
        if let Ok(n) = reader.read(&mut buf) {
            let bytes = &buf[..n];

            // === Tabular / scientific / columnar ===

            // Parquet: starts with "PAR1"
            if bytes.starts_with(b"PAR1") {
                return Ok(FileFormat::Parquet);
            }
            // HDF5: starts with \x89HDF\r\n\x1a\n
            // NetCDF4 also uses HDF5 storage — disambiguate by extension
            if bytes.len() >= 8 && bytes[..8] == [0x89, b'H', b'D', b'F', 0x0D, 0x0A, 0x1A, 0x0A] {
                return Ok(
                    match path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_lowercase())
                    {
                        Some(ref ext) if matches!(ext.as_str(), "nc" | "nc4" | "netcdf") => {
                            FileFormat::NetCdf
                        }
                        _ => FileFormat::Hdf5,
                    },
                );
            }
            // NetCDF classic: starts with "CDF"
            if bytes.starts_with(b"CDF") {
                return Ok(FileFormat::NetCdf);
            }
            // FITS: starts with "SIMPLE  ="
            if bytes.starts_with(b"SIMPLE") {
                return Ok(FileFormat::Fits);
            }
            // Arrow IPC: starts with "ARROW1"
            if bytes.starts_with(b"ARROW1") {
                return Ok(FileFormat::Arrow);
            }

            // === Images ===

            // PNG: \x89PNG\r\n\x1a\n
            if bytes.len() >= 8 && bytes[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
                return Ok(FileFormat::Png);
            }
            // JPEG: starts with \xFF\xD8\xFF
            if bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF] {
                return Ok(FileFormat::Jpeg);
            }
            // GIF: starts with "GIF87a" or "GIF89a"
            if bytes.starts_with(b"GIF8") {
                return Ok(FileFormat::Gif);
            }
            // BMP: starts with "BM"
            if bytes.starts_with(b"BM") {
                return Ok(FileFormat::Bmp);
            }
            // TIFF: "II" (little-endian) or "MM" (big-endian) + magic 42
            if bytes.len() >= 4
                && ((bytes[0] == b'I' && bytes[1] == b'I' && bytes[2] == 42 && bytes[3] == 0)
                    || (bytes[0] == b'M' && bytes[1] == b'M' && bytes[2] == 0 && bytes[3] == 42))
            {
                return Ok(FileFormat::Tiff);
            }

            // === RIFF containers (must check subtype at bytes 8-12) ===

            if bytes.len() >= 12 && bytes[..4] == *b"RIFF" {
                if bytes[8..12] == *b"WEBP" {
                    return Ok(FileFormat::WebP);
                }
                if bytes[8..12] == *b"WAVE" {
                    return Ok(FileFormat::Wav);
                }
                if bytes[8..12] == *b"AVI " {
                    return Ok(FileFormat::Avi);
                }
            }

            // === ISO BMFF (MP4 family): "ftyp" box at offset 4 ===

            if bytes.len() >= 8 && bytes[4..8] == *b"ftyp" {
                // Check extension for M4A (audio in MP4 container)
                return Ok(match path.extension().and_then(|e| e.to_str()) {
                    Some("m4a") => FileFormat::Aac,
                    _ => FileFormat::Mp4,
                });
            }

            // === EBML (Matroska / WebM) ===

            if bytes.len() >= 4 && bytes[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
                // Both MKV and WebM use EBML; distinguish by extension
                return Ok(match path.extension().and_then(|e| e.to_str()) {
                    Some("webm") => FileFormat::WebM,
                    _ => FileFormat::Mkv,
                });
            }

            // === Audio ===

            // FLAC: starts with "fLaC"
            if bytes.starts_with(b"fLaC") {
                return Ok(FileFormat::Flac);
            }
            // Ogg: starts with "OggS"
            if bytes.starts_with(b"OggS") {
                return Ok(FileFormat::Ogg);
            }
            // MP3: ID3 tag or MPEG frame sync
            if bytes.starts_with(b"ID3")
                || (bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0)
            {
                return Ok(FileFormat::Mp3);
            }

            // === VTK ===

            // VTK Legacy: starts with "# vtk DataFile Version"
            if bytes.starts_with(b"# vtk DataFile Version") {
                return Ok(FileFormat::VtkLegacy);
            }

            // VTK XML: starts with <?xml or <VTKFile — parse type= attribute
            if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<VTKFile") {
                let header = std::str::from_utf8(bytes).unwrap_or("");
                if let Some(vtk_type) = extract_vtk_xml_type(header) {
                    return Ok(match vtk_type {
                        "UnstructuredGrid" => FileFormat::Vtu,
                        "PolyData" => FileFormat::Vtp,
                        "StructuredGrid" => FileFormat::Vts,
                        "RectilinearGrid" => FileFormat::Vtr,
                        "ImageData" => FileFormat::Vti,
                        _ => FileFormat::VtkLegacy,
                    });
                }
                // Not VTK — generic XML
                return Ok(FileFormat::Xml);
            }

            // HTML: <!DOCTYPE html or <html
            if bytes.len() >= 5 {
                let prefix = std::str::from_utf8(&bytes[..bytes.len().min(30)])
                    .unwrap_or("");
                let lower = prefix.to_lowercase();
                if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
                    return Ok(FileFormat::Html);
                }
            }

            // === Archives / compression ===

            // OLE2 Compound Document: shared by XLS, DOC, PPT — disambiguate by extension
            if bytes.len() >= 8 && bytes[..8] == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
                return Ok(
                    match path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_lowercase())
                    {
                        Some(ref ext) if ext == "xls" => FileFormat::Xls,
                        _ => FileFormat::Unknown("ole2".into()),
                    },
                );
            }

            // ZIP: local file header "PK\x03\x04" — check extension for spreadsheet/zarr containers
            if bytes.len() >= 4 && bytes[..4] == [0x50, 0x4B, 0x03, 0x04] {
                // Check for .zarr.zip (stem ends with ".zarr")
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if stem.ends_with(".zarr") {
                        return Ok(FileFormat::ZarrV3);
                    }
                }
                return Ok(
                    match path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_lowercase())
                    {
                        Some(ref ext) if matches!(ext.as_str(), "xlsx" | "xlsm" | "xlsb") => {
                            FileFormat::Xlsx
                        }
                        Some(ref ext) if ext == "ods" => FileFormat::Ods,
                        _ => FileFormat::Zip,
                    },
                );
            }
            // ZIP: empty archive / spanned "PK\x05\x06" or "PK\x07\x08"
            if bytes.len() >= 4
                && bytes[0] == 0x50
                && bytes[1] == 0x4B
                && (bytes[2] == 0x05 || bytes[2] == 0x07)
            {
                return Ok(FileFormat::Zip);
            }
            // 7z: "7z\xBC\xAF\x27\x1C"
            if bytes.len() >= 6 && bytes[..6] == [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C] {
                return Ok(FileFormat::SevenZip);
            }
            // RAR: "Rar!\x1A\x07"
            if bytes.len() >= 6 && bytes[..6] == [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07] {
                return Ok(FileFormat::Rar);
            }
            // Gzip: \x1F\x8B
            if bytes.len() >= 2 && bytes[..2] == [0x1F, 0x8B] {
                return Ok(FileFormat::Gzip);
            }
            // XZ: \xFD\x37\x7A\x58\x5A\x00 ("\xFD7zXZ\0")
            if bytes.len() >= 6 && bytes[..6] == [0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00] {
                return Ok(FileFormat::Xz);
            }
            // Zstd: \x28\xB5\x2F\xFD
            if bytes.len() >= 4 && bytes[..4] == [0x28, 0xB5, 0x2F, 0xFD] {
                return Ok(FileFormat::Zstd);
            }
            // BZip2: "BZh"
            if bytes.len() >= 3 && bytes[..3] == [0x42, 0x5A, 0x68] {
                return Ok(FileFormat::Bzip2);
            }
            // Tar: POSIX "ustar" magic at offset 257
            if bytes.len() >= 262 && bytes[257..262] == *b"ustar" {
                return Ok(FileFormat::Tar);
            }
        }
    }

    // Fall back to extension
    detect_from_extension(path).ok_or_else(|| MetadataError::DetectionFailed(path.to_path_buf()))
}

/// Extract the `type` attribute from a `<VTKFile type="...">` header.
fn extract_vtk_xml_type(header: &str) -> Option<&str> {
    let vtk_start = header.find("<VTKFile")?;
    let rest = &header[vtk_start..];
    let type_start = rest.find("type=\"")? + 6;
    let rest = &rest[type_start..];
    let type_end = rest.find('"')?;
    Some(&rest[..type_end])
}

/// Detect file format from raw bytes (magic bytes only, no extension fallback).
///
/// Applies the same magic-byte detection logic as [`detect_format`] but
/// operates on a byte slice instead of a file path. Returns `None` if no
/// known signature is found.
pub fn detect_format_from_bytes(bytes: &[u8]) -> Option<FileFormat> {
    if bytes.starts_with(b"PAR1") {
        return Some(FileFormat::Parquet);
    }
    if bytes.len() >= 8 && bytes[..8] == [0x89, b'H', b'D', b'F', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some(FileFormat::Hdf5);
    }
    if bytes.starts_with(b"CDF") {
        return Some(FileFormat::NetCdf);
    }
    if bytes.starts_with(b"SIMPLE") {
        return Some(FileFormat::Fits);
    }
    if bytes.starts_with(b"ARROW1") {
        return Some(FileFormat::Arrow);
    }
    if bytes.len() >= 8 && bytes[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some(FileFormat::Png);
    }
    if bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF] {
        return Some(FileFormat::Jpeg);
    }
    if bytes.starts_with(b"GIF8") {
        return Some(FileFormat::Gif);
    }
    if bytes.starts_with(b"BM") {
        return Some(FileFormat::Bmp);
    }
    if bytes.len() >= 4
        && ((bytes[0] == b'I' && bytes[1] == b'I' && bytes[2] == 42 && bytes[3] == 0)
            || (bytes[0] == b'M' && bytes[1] == b'M' && bytes[2] == 0 && bytes[3] == 42))
    {
        return Some(FileFormat::Tiff);
    }
    if bytes.len() >= 12 && bytes[..4] == *b"RIFF" {
        if bytes[8..12] == *b"WEBP" {
            return Some(FileFormat::WebP);
        }
        if bytes[8..12] == *b"WAVE" {
            return Some(FileFormat::Wav);
        }
        if bytes[8..12] == *b"AVI " {
            return Some(FileFormat::Avi);
        }
    }
    if bytes.len() >= 8 && bytes[4..8] == *b"ftyp" {
        return Some(FileFormat::Mp4);
    }
    if bytes.len() >= 4 && bytes[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return Some(FileFormat::Mkv);
    }
    if bytes.starts_with(b"fLaC") {
        return Some(FileFormat::Flac);
    }
    if bytes.starts_with(b"OggS") {
        return Some(FileFormat::Ogg);
    }
    if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0)
    {
        return Some(FileFormat::Mp3);
    }
    // VTK Legacy
    if bytes.starts_with(b"# vtk DataFile Version") {
        return Some(FileFormat::VtkLegacy);
    }
    // VTK XML
    if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<VTKFile") {
        let header = std::str::from_utf8(bytes).unwrap_or("");
        if let Some(vtk_type) = extract_vtk_xml_type(header) {
            return Some(match vtk_type {
                "UnstructuredGrid" => FileFormat::Vtu,
                "PolyData" => FileFormat::Vtp,
                "StructuredGrid" => FileFormat::Vts,
                "RectilinearGrid" => FileFormat::Vtr,
                "ImageData" => FileFormat::Vti,
                _ => FileFormat::VtkLegacy,
            });
        }
        // Not VTK — generic XML
        return Some(FileFormat::Xml);
    }
    // HTML
    if bytes.len() >= 5 {
        let prefix = std::str::from_utf8(&bytes[..bytes.len().min(30)])
            .unwrap_or("");
        let lower = prefix.to_lowercase();
        if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
            return Some(FileFormat::Html);
        }
    }
    if bytes.len() >= 4 && bytes[..4] == [0x50, 0x4B, 0x03, 0x04] {
        return Some(FileFormat::Zip);
    }
    if bytes.len() >= 6 && bytes[..6] == [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C] {
        return Some(FileFormat::SevenZip);
    }
    if bytes.len() >= 6 && bytes[..6] == [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07] {
        return Some(FileFormat::Rar);
    }
    if bytes.len() >= 2 && bytes[..2] == [0x1F, 0x8B] {
        return Some(FileFormat::Gzip);
    }
    if bytes.len() >= 6 && bytes[..6] == [0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00] {
        return Some(FileFormat::Xz);
    }
    if bytes.len() >= 4 && bytes[..4] == [0x28, 0xB5, 0x2F, 0xFD] {
        return Some(FileFormat::Zstd);
    }
    if bytes.len() >= 3 && bytes[..3] == [0x42, 0x5A, 0x68] {
        return Some(FileFormat::Bzip2);
    }
    if bytes.len() >= 262 && bytes[257..262] == *b"ustar" {
        return Some(FileFormat::Tar);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn extension_detection() {
        let cases = vec![
            ("data.csv", FileFormat::Csv),
            ("data.tsv", FileFormat::Csv),
            ("data.parquet", FileFormat::Parquet),
            ("data.pq", FileFormat::Parquet),
            ("data.h5", FileFormat::Hdf5),
            ("data.nc", FileFormat::NetCdf),
            ("data.fits", FileFormat::Fits),
            ("photo.jpg", FileFormat::Jpeg),
            ("photo.jpeg", FileFormat::Jpeg),
            ("image.png", FileFormat::Png),
            ("image.tiff", FileFormat::Tiff),
            ("image.webp", FileFormat::WebP),
            ("image.gif", FileFormat::Gif),
            ("image.bmp", FileFormat::Bmp),
            ("song.mp3", FileFormat::Mp3),
            ("music.flac", FileFormat::Flac),
            ("audio.wav", FileFormat::Wav),
            ("sound.ogg", FileFormat::Ogg),
            ("track.aac", FileFormat::Aac),
            ("track.m4a", FileFormat::Aac),
            ("video.mp4", FileFormat::Mp4),
            ("movie.mkv", FileFormat::Mkv),
            ("clip.avi", FileFormat::Avi),
            ("stream.webm", FileFormat::WebM),
            // Zarr (extension only — actual detection uses directory markers)
            ("data.zarr", FileFormat::ZarrV3),
            // VTK
            ("mesh.vtk", FileFormat::VtkLegacy),
            ("mesh.vtu", FileFormat::Vtu),
            ("mesh.vtp", FileFormat::Vtp),
            ("mesh.vts", FileFormat::Vts),
            ("mesh.vtr", FileFormat::Vtr),
            ("mesh.vti", FileFormat::Vti),
            // Spreadsheet
            ("data.xlsx", FileFormat::Xlsx),
            ("data.xlsm", FileFormat::Xlsx),
            ("data.xlsb", FileFormat::Xlsx),
            ("data.xls", FileFormat::Xls),
            ("data.ods", FileFormat::Ods),
            // Archives
            ("data.zip", FileFormat::Zip),
            ("lib.jar", FileFormat::Zip),
            ("book.epub", FileFormat::Zip),
            ("backup.tar", FileFormat::Tar),
            ("backup.gz", FileFormat::Gzip),
            ("backup.tgz", FileFormat::Gzip),
            ("backup.bz2", FileFormat::Bzip2),
            ("backup.xz", FileFormat::Xz),
            ("backup.zst", FileFormat::Zstd),
            ("backup.7z", FileFormat::SevenZip),
            ("backup.rar", FileFormat::Rar),
        ];

        for (filename, expected) in cases {
            let path = PathBuf::from(filename);
            let detected = detect_from_extension(&path);
            assert_eq!(detected, Some(expected), "failed for extension: {filename}");
        }
    }

    #[test]
    fn unknown_extension() {
        let path = PathBuf::from("data.xyz");
        let detected = detect_from_extension(&path);
        assert_eq!(detected, Some(FileFormat::Unknown("xyz".into())));
    }

    #[test]
    fn no_extension() {
        let path = PathBuf::from("Makefile");
        let detected = detect_from_extension(&path);
        assert_eq!(detected, None);
    }

    // Magic bytes detection tests using temp files

    fn write_temp(suffix: &str, data: &[u8]) -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(suffix).unwrap();
        std::fs::write(tmp.path(), data).unwrap();
        tmp
    }

    #[test]
    fn magic_mp4_ftyp() {
        // Minimal ftyp box: size(8) + "ftyp" + major_brand "isom"
        let mut data = vec![0u8; 16];
        data[0..4].copy_from_slice(&12u32.to_be_bytes()); // box size
        data[4..8].copy_from_slice(b"ftyp");
        data[8..12].copy_from_slice(b"isom");
        let tmp = write_temp(".dat", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Mp4);
    }

    #[test]
    fn magic_mp4_m4a_extension() {
        // ftyp box but with .m4a extension -> Aac
        let mut data = vec![0u8; 16];
        data[0..4].copy_from_slice(&12u32.to_be_bytes());
        data[4..8].copy_from_slice(b"ftyp");
        data[8..12].copy_from_slice(b"M4A ");
        let tmp = write_temp(".m4a", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Aac);
    }

    #[test]
    fn magic_mkv_ebml() {
        let mut data = vec![0u8; 32];
        data[0..4].copy_from_slice(&[0x1A, 0x45, 0xDF, 0xA3]);
        let tmp = write_temp(".mkv", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Mkv);
    }

    #[test]
    fn magic_webm_ebml() {
        // Same EBML magic but .webm extension -> WebM
        let mut data = vec![0u8; 32];
        data[0..4].copy_from_slice(&[0x1A, 0x45, 0xDF, 0xA3]);
        let tmp = write_temp(".webm", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::WebM);
    }

    #[test]
    fn magic_avi_riff() {
        let mut data = vec![0u8; 16];
        data[0..4].copy_from_slice(b"RIFF");
        data[4..8].copy_from_slice(&1000u32.to_le_bytes());
        data[8..12].copy_from_slice(b"AVI ");
        let tmp = write_temp(".dat", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Avi);
    }

    #[test]
    fn magic_zip() {
        let data = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00, 0x00, 0x00];
        let tmp = write_temp(".dat", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Zip);
    }

    #[test]
    fn magic_gzip() {
        let data = [0x1F, 0x8B, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00];
        let tmp = write_temp(".dat", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Gzip);
    }

    #[test]
    fn magic_7z() {
        let data = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0x00, 0x04];
        let tmp = write_temp(".dat", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::SevenZip);
    }

    #[test]
    fn magic_rar() {
        let data = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x01, 0x00];
        let tmp = write_temp(".dat", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Rar);
    }

    #[test]
    fn magic_xz() {
        let data = [0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00, 0x00, 0x01];
        let tmp = write_temp(".dat", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Xz);
    }

    #[test]
    fn magic_zstd() {
        let data = [0x28, 0xB5, 0x2F, 0xFD, 0x00, 0x00, 0x00, 0x00];
        let tmp = write_temp(".dat", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Zstd);
    }

    #[test]
    fn magic_bzip2() {
        let data = [0x42, 0x5A, 0x68, 0x39, 0x31, 0x41, 0x59, 0x26];
        let tmp = write_temp(".dat", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Bzip2);
    }

    #[test]
    fn magic_tar_ustar() {
        let mut data = vec![0u8; 264];
        data[257..262].copy_from_slice(b"ustar");
        let tmp = write_temp(".dat", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Tar);
    }

    #[test]
    fn magic_ole2_xls() {
        let data = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0x00, 0x00];
        let tmp = write_temp(".xls", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Xls);
    }

    #[test]
    fn magic_ole2_unknown() {
        // OLE2 with non-xls extension → Unknown
        let data = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0x00, 0x00];
        let tmp = write_temp(".doc", &data);
        assert_eq!(
            detect_format(tmp.path()).unwrap(),
            FileFormat::Unknown("ole2".into())
        );
    }

    #[test]
    fn magic_zip_xlsx_disambiguation() {
        // ZIP magic with .xlsx extension → Xlsx, not Zip
        let data = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00, 0x00, 0x00];
        let tmp = write_temp(".xlsx", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Xlsx);
    }

    #[test]
    fn magic_zip_ods_disambiguation() {
        // ZIP magic with .ods extension → Ods, not Zip
        let data = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00, 0x00, 0x00];
        let tmp = write_temp(".ods", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Ods);
    }

    #[test]
    fn magic_zip_still_detects_zip() {
        // ZIP magic with .zip extension → still Zip
        let data = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00, 0x00, 0x00];
        let tmp = write_temp(".zip", &data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Zip);
    }

    #[test]
    fn magic_vtk_legacy() {
        let data = b"# vtk DataFile Version 4.2\nASCII\nDATASET UNSTRUCTURED_GRID\n";
        let tmp = write_temp(".dat", data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::VtkLegacy);
    }

    #[test]
    fn magic_vtk_xml_vtu() {
        let data = br#"<?xml version="1.0"?>
<VTKFile type="UnstructuredGrid" version="0.1">"#;
        let tmp = write_temp(".dat", data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Vtu);
    }

    #[test]
    fn magic_vtk_xml_vtp() {
        let data = br#"<VTKFile type="PolyData" version="0.1">"#;
        let tmp = write_temp(".dat", data);
        assert_eq!(detect_format(tmp.path()).unwrap(), FileFormat::Vtp);
    }

    #[test]
    fn magic_vtk_bytes_detection() {
        let data = b"# vtk DataFile Version 3.0\nBINARY\n";
        assert_eq!(detect_format_from_bytes(data), Some(FileFormat::VtkLegacy));

        let xml = br#"<VTKFile type="ImageData" version="0.1">"#;
        assert_eq!(detect_format_from_bytes(xml), Some(FileFormat::Vti));
    }

    #[test]
    fn zarr_v3_directory_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("zarr.json"), r#"{"zarr_format":3}"#).unwrap();
        assert_eq!(detect_format(dir.path()).unwrap(), FileFormat::ZarrV3);
        assert!(is_zarr_directory(dir.path()));
    }

    #[test]
    fn zarr_v2_directory_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".zgroup"), r#"{"zarr_format":2}"#).unwrap();
        assert_eq!(detect_format(dir.path()).unwrap(), FileFormat::ZarrV2);
        assert!(is_zarr_directory(dir.path()));
    }

    #[test]
    fn zarr_v2_zarray_directory_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".zarray"), "{}").unwrap();
        assert_eq!(detect_format(dir.path()).unwrap(), FileFormat::ZarrV2);
        assert!(is_zarr_directory(dir.path()));
    }

    #[test]
    fn non_zarr_directory_not_detected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_zarr_directory(dir.path()));
    }

    #[test]
    fn zarr_zip_disambiguation() {
        // ZIP magic with stem ending in .zarr → ZarrV3
        let data = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00, 0x00, 0x00];
        let dir = tempfile::tempdir().unwrap();
        let zarr_zip = dir.path().join("data.zarr.zip");
        std::fs::write(&zarr_zip, data).unwrap();
        assert_eq!(detect_format(&zarr_zip).unwrap(), FileFormat::ZarrV3);
    }
}
