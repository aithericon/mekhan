//! Markdown file metadata extractor.
//!
//! Parses Markdown documents, extracting heading structure, word/link/image
//! counts, code block languages, and front matter detection.

use std::collections::HashMap;
use std::path::Path;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, MarkdownHeading, MarkdownMetadata};
use crate::types::FileMetadata;

pub struct MarkdownExtractor;

impl MarkdownExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MarkdownExtractor {
    fn default() -> Self {
        Self::new()
    }
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Strip YAML front matter from markdown content.
/// Returns (body without front matter, has_front_matter).
fn strip_front_matter(content: &str) -> (&str, bool) {
    if let Some(rest) = content.strip_prefix("---") {
        // Find closing ---
        if let Some(end) = rest.find("\n---") {
            let after = end + 4; // skip "\n---"
            let body = if after < rest.len() {
                &rest[after..]
            } else {
                ""
            };
            return (body, true);
        }
    }
    (content, false)
}

impl MetadataExtractor for MarkdownExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let content = std::fs::read_to_string(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let line_count = content.lines().count();
        let (body, has_front_matter) = strip_front_matter(&content);

        let mut headings = Vec::new();
        let mut word_count: usize = 0;
        let mut code_blocks: usize = 0;
        let mut code_languages: Vec<String> = Vec::new();
        let mut link_count: usize = 0;
        let mut image_count: usize = 0;

        let mut in_heading = false;
        let mut heading_level: u8 = 0;
        let mut heading_text = String::new();
        let mut in_code_block = false;

        let parser = Parser::new_ext(body, Options::all());

        for event in parser {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    in_heading = true;
                    heading_level = heading_level_to_u8(level);
                    heading_text.clear();
                }
                Event::End(TagEnd::Heading(_)) => {
                    in_heading = false;
                    headings.push(MarkdownHeading {
                        level: heading_level,
                        text: heading_text.clone(),
                    });
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    in_code_block = true;
                    code_blocks += 1;
                    if let CodeBlockKind::Fenced(lang) = kind {
                        let lang = lang.trim().to_string();
                        if !lang.is_empty() && !code_languages.contains(&lang) {
                            code_languages.push(lang);
                        }
                    }
                }
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                }
                Event::Start(Tag::Link { .. }) => {
                    link_count += 1;
                }
                Event::Start(Tag::Image { .. }) => {
                    image_count += 1;
                }
                Event::Text(text) => {
                    if in_heading {
                        heading_text.push_str(&text);
                    }
                    if !in_code_block {
                        word_count += text.split_whitespace().count();
                    }
                }
                Event::Code(text) => {
                    if in_heading {
                        heading_text.push_str(&text);
                    }
                    // Inline code counts as words.
                    word_count += text.split_whitespace().count();
                }
                _ => {}
            }
        }

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Markdown,
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
            format_specific: Some(FormatMetadata::Markdown(MarkdownMetadata {
                headings,
                word_count,
                line_count,
                code_blocks,
                code_languages,
                link_count,
                image_count,
                has_front_matter,
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
        FileFormat::Markdown
    }

    fn extensions(&self) -> &[&str] {
        &["md", "markdown"]
    }
}
