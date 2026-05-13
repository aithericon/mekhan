//! XML file metadata extractor.
//!
//! Parses XML documents, extracting structural metadata via streaming events.

use std::collections::HashMap;
use std::path::Path;

use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, XmlMetadata};
use crate::types::FileMetadata;

pub struct XmlExtractor;

impl XmlExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for XmlExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for XmlExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let content = std::fs::read_to_string(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut reader = quick_xml::Reader::from_str(&content);
        reader.config_mut().trim_text(true);

        let mut root_element = String::new();
        let mut namespaces: Vec<(String, String)> = Vec::new();
        let mut num_elements: usize = 0;
        let mut num_attributes: usize = 0;
        let mut max_depth: usize = 0;
        let mut current_depth: usize = 0;
        let mut processing_instructions: Vec<String> = Vec::new();
        let mut seen_ns: std::collections::HashSet<String> = std::collections::HashSet::new();

        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(ref e)) => {
                    num_elements += 1;
                    current_depth += 1;
                    if current_depth > max_depth {
                        max_depth = current_depth;
                    }

                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if root_element.is_empty() {
                        root_element = name;
                    }

                    for attr in e.attributes().flatten() {
                        num_attributes += 1;
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        if key.starts_with("xmlns") {
                            let prefix = if key == "xmlns" {
                                String::new()
                            } else {
                                key.strip_prefix("xmlns:").unwrap_or("").to_string()
                            };
                            let uri = String::from_utf8_lossy(&attr.value).to_string();
                            let ns_key = format!("{}={}", prefix, uri);
                            if seen_ns.insert(ns_key) {
                                namespaces.push((prefix, uri));
                            }
                        }
                    }
                }
                Ok(quick_xml::events::Event::Empty(ref e)) => {
                    num_elements += 1;
                    // Temporarily enter and leave depth for max_depth tracking.
                    let check_depth = current_depth + 1;
                    if check_depth > max_depth {
                        max_depth = check_depth;
                    }

                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if root_element.is_empty() {
                        root_element = name;
                    }

                    for attr in e.attributes().flatten() {
                        num_attributes += 1;
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        if key.starts_with("xmlns") {
                            let prefix = if key == "xmlns" {
                                String::new()
                            } else {
                                key.strip_prefix("xmlns:").unwrap_or("").to_string()
                            };
                            let uri = String::from_utf8_lossy(&attr.value).to_string();
                            let ns_key = format!("{}={}", prefix, uri);
                            if seen_ns.insert(ns_key) {
                                namespaces.push((prefix, uri));
                            }
                        }
                    }
                }
                Ok(quick_xml::events::Event::End(_)) => {
                    current_depth = current_depth.saturating_sub(1);
                }
                Ok(quick_xml::events::Event::PI(ref e)) => {
                    let text = String::from_utf8_lossy(e.as_ref()).to_string();
                    processing_instructions.push(text);
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(e) => {
                    return Err(MetadataError::ParseError {
                        format: "xml".into(),
                        path: path.to_path_buf(),
                        message: e.to_string(),
                    });
                }
                _ => {}
            }
            buf.clear();
        }

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Xml,
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
            format_specific: Some(FormatMetadata::Xml(XmlMetadata {
                root_element,
                namespaces,
                num_elements,
                num_attributes,
                max_depth,
                processing_instructions,
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
        FileFormat::Xml
    }

    fn extensions(&self) -> &[&str] {
        &["xml", "xsl", "xsd", "svg"]
    }
}
