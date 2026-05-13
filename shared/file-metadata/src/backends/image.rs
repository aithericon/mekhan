use std::collections::HashMap;
use std::path::Path;

use crate::detect::detect_from_extension;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, ImageMetadata};
use crate::types::{AttributeValue, Dimension, FileMetadata};

/// Image metadata extractor.
///
/// Uses `imagesize` for dimensions (supports 26+ formats) and
/// `kamadak-exif` for EXIF data (JPEG, TIFF, PNG, WebP, HEIF).
pub struct ImageExtractor;

impl ImageExtractor {
    pub fn new() -> Self {
        Self
    }

    /// Attempt to read EXIF data from the file and return it as a HashMap.
    fn read_exif(path: &Path) -> HashMap<String, AttributeValue> {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return HashMap::new(),
        };
        let mut bufreader = std::io::BufReader::new(file);
        let exif = match exif::Reader::new().read_from_container(&mut bufreader) {
            Ok(e) => e,
            Err(_) => return HashMap::new(),
        };

        let mut attrs = HashMap::new();
        for field in exif.fields() {
            let tag_name = field.tag.to_string();
            let value_str = field.display_value().with_unit(&exif).to_string();
            attrs.insert(tag_name, AttributeValue::String(value_str));
        }
        attrs
    }

    /// Map imagesize format to our FileFormat.
    fn map_format(img_type: imagesize::ImageType) -> FileFormat {
        match img_type {
            imagesize::ImageType::Jpeg => FileFormat::Jpeg,
            imagesize::ImageType::Png => FileFormat::Png,
            imagesize::ImageType::Gif => FileFormat::Gif,
            imagesize::ImageType::Bmp => FileFormat::Bmp,
            imagesize::ImageType::Tiff => FileFormat::Tiff,
            imagesize::ImageType::Webp => FileFormat::WebP,
            _ => FileFormat::Unknown(format!("{img_type:?}")),
        }
    }
}

impl Default for ImageExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for ImageExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        if !path.exists() {
            return Err(MetadataError::FileNotFound(path.to_path_buf()));
        }

        let size = imagesize::size(path).map_err(|e| MetadataError::ParseError {
            format: "image".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

        let width = size.width as u32;
        let height = size.height as u32;

        // Detect specific image format from header bytes, fall back to extension
        let format = {
            use std::io::Read;
            let mut header = [0u8; 32];
            std::fs::File::open(path)
                .ok()
                .and_then(|mut f| f.read(&mut header).ok())
                .and_then(|n| imagesize::image_type(&header[..n]).ok())
                .map(Self::map_format)
                .or_else(|| detect_from_extension(path))
                .unwrap_or(FileFormat::Unknown("image".into()))
        };

        // Read EXIF data (best-effort, won't fail if absent)
        let attributes = Self::read_exif(path);

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format,
            mime_type: None,
            num_rows: None,
            num_columns: None,
            file_size_bytes: file_size,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: vec![],
            dimensions: vec![
                Dimension {
                    name: "width".into(),
                    size: Some(width as u64),
                },
                Dimension {
                    name: "height".into(),
                    size: Some(height as u64),
                },
            ],
            columns: vec![],
            attributes,
            format_specific: Some(FormatMetadata::Image(ImageMetadata {
                width,
                height,
                color_space: None, // not available from imagesize alone
                bit_depth: None,
                channels: None,
                animated: false,
                frame_count: None,
                dpi: None,
                compression: None,
            })),
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        })
    }

    fn format(&self) -> FileFormat {
        FileFormat::Jpeg // primary, but handles all image formats
    }

    fn extensions(&self) -> &[&str] {
        &[
            "jpg", "jpeg", "jpe", "png", "gif", "bmp", "tif", "tiff", "webp",
        ]
    }
}
