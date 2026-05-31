//! HTML file metadata extractor.
//!
//! Parses HTML documents, extracting structural metadata: title, headings,
//! links, scripts, stylesheets, images, forms, tables, and meta tags.

use std::collections::HashMap;
use std::path::Path;

use scraper::{Html, Selector};

use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, HtmlMetadata};
use crate::types::FileMetadata;

pub struct HtmlExtractor;

impl HtmlExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HtmlExtractor {
    fn default() -> Self {
        Self::new()
    }
}

fn count_selector(doc: &Html, selector_str: &str) -> usize {
    Selector::parse(selector_str)
        .map(|sel| doc.select(&sel).count())
        .unwrap_or(0)
}

impl MetadataExtractor for HtmlExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let content = std::fs::read_to_string(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let document = Html::parse_document(&content);

        // Title
        let title = Selector::parse("title")
            .ok()
            .and_then(|sel| document.select(&sel).next())
            .map(|el| el.text().collect::<String>().trim().to_string())
            .filter(|t| !t.is_empty());

        // Headings (h1-h6)
        let num_headings = count_selector(&document, "h1, h2, h3, h4, h5, h6");

        // Links
        let num_links = count_selector(&document, "a[href]");

        // Scripts
        let num_scripts = count_selector(&document, "script");

        // Stylesheets
        let num_stylesheets = count_selector(
            &document,
            "link[rel='stylesheet'], link[rel=\"stylesheet\"]",
        );

        // Images
        let num_images = count_selector(&document, "img");

        // Forms
        let num_forms = count_selector(&document, "form");

        // Tables
        let num_tables = count_selector(&document, "table");

        // Meta tags
        let mut meta_tags = Vec::new();
        if let Ok(sel) = Selector::parse("meta") {
            for el in document.select(&sel) {
                let name = el
                    .value()
                    .attr("name")
                    .or_else(|| el.value().attr("property"))
                    .unwrap_or("")
                    .to_string();
                let meta_content = el.value().attr("content").unwrap_or("").to_string();
                if !name.is_empty() {
                    meta_tags.push((name, meta_content));
                }
            }
        }

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Html,
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
            dimensions: vec![],
            columns: vec![],
            attributes: HashMap::new(),
            format_specific: Some(FormatMetadata::Html(HtmlMetadata {
                title,
                num_headings,
                num_links,
                num_scripts,
                num_stylesheets,
                num_images,
                num_forms,
                num_tables,
                meta_tags,
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
        FileFormat::Html
    }

    fn extensions(&self) -> &[&str] {
        &["html", "htm", "xhtml"]
    }
}
