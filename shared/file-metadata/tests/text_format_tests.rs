//! Integration tests for text, markup, and configuration format backends.

use std::io::Write;

fn write_temp(suffix: &str, content: &str) -> tempfile::NamedTempFile {
    let mut tmp = tempfile::Builder::new()
        .suffix(suffix)
        .tempfile()
        .unwrap();
    tmp.write_all(content.as_bytes()).unwrap();
    tmp.flush().unwrap();
    tmp
}

// ============================================================================
// Txt
// ============================================================================

#[cfg(feature = "txt")]
mod txt_tests {
    use super::*;
    use fmeta::{extract_metadata, format::FormatMetadata, format::TxtMetadata};

    #[test]
    fn basic_txt_extraction() {
        let tmp = write_temp(".txt", "Hello world\nThis is a test\nThird line here\n");
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.format, fmeta::format::FileFormat::Txt);
        assert_eq!(meta.num_rows, None);
        assert_eq!(meta.num_columns, None);

        if let Some(FormatMetadata::Txt(TxtMetadata {
            line_count,
            word_count,
            char_count,
            max_line_length,
            has_bom,
            non_ascii,
            ..
        })) = &meta.format_specific
        {
            assert_eq!(*line_count, 3);
            assert_eq!(*word_count, 9);
            assert!(*char_count > 0);
            assert!(*max_line_length > 0);
            assert!(!has_bom);
            assert!(!non_ascii);
        } else {
            panic!("expected Txt format_specific");
        }
    }

    #[test]
    fn txt_empty_file() {
        let tmp = write_temp(".txt", "");
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Txt(txt)) = &meta.format_specific {
            assert_eq!(txt.line_count, 0);
            assert_eq!(txt.word_count, 0);
            assert_eq!(txt.char_count, 0);
            assert_eq!(txt.max_line_length, 0);
            assert_eq!(txt.avg_line_length, 0.0);
            assert!(!txt.has_bom);
            assert!(!txt.non_ascii);
        } else {
            panic!("expected Txt format_specific");
        }
    }

    #[test]
    fn txt_bom_detection() {
        let bom = b"\xEF\xBB\xBFHello BOM\n";
        let mut tmp = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile()
            .unwrap();
        tmp.write_all(bom).unwrap();
        tmp.flush().unwrap();

        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Txt(txt)) = &meta.format_specific {
            assert!(txt.has_bom);
            assert_eq!(txt.line_count, 1);
            assert_eq!(txt.word_count, 2);
        } else {
            panic!("expected Txt format_specific");
        }
    }
}

// ============================================================================
// Env
// ============================================================================

#[cfg(feature = "env")]
mod env_tests {
    use super::*;
    use fmeta::{extract_metadata, format::EnvMetadata, format::FormatMetadata};

    #[test]
    fn basic_env_extraction() {
        let tmp = write_temp(".env", "DATABASE_URL=postgres://localhost\nSECRET_KEY=abc123\n");
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.format, fmeta::format::FileFormat::Env);
        assert_eq!(meta.column_names, vec!["DATABASE_URL", "SECRET_KEY"]);
        assert_eq!(meta.num_rows, Some(2));

        if let Some(FormatMetadata::Env(EnvMetadata { num_variables, num_comments })) =
            &meta.format_specific
        {
            assert_eq!(*num_variables, 2);
            assert_eq!(*num_comments, 0);
        } else {
            panic!("expected Env format_specific");
        }
    }

    #[test]
    fn env_comments_and_empty_lines() {
        let tmp = write_temp(
            ".env",
            "# Database config\nDB_HOST=localhost\n\n# Port\nDB_PORT=5432\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.column_names, vec!["DB_HOST", "DB_PORT"]);
        if let Some(FormatMetadata::Env(env)) = &meta.format_specific {
            assert_eq!(env.num_variables, 2);
            assert_eq!(env.num_comments, 2);
        } else {
            panic!("expected Env format_specific");
        }
    }

    #[test]
    fn env_quoted_values() {
        let tmp = write_temp(".env", "KEY=\"value with spaces\"\nOTHER=simple\n");
        let meta = extract_metadata(tmp.path()).unwrap();
        assert_eq!(meta.column_names.len(), 2);
    }
}

// ============================================================================
// INI
// ============================================================================

#[cfg(feature = "ini")]
mod ini_tests {
    use super::*;
    use fmeta::{extract_metadata, format::FormatMetadata, format::IniMetadata};

    #[test]
    fn basic_ini_extraction() {
        let tmp = write_temp(
            ".ini",
            "[database]\nhost = localhost\nport = 5432\n\n[app]\ndebug = true\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.format, fmeta::format::FileFormat::Ini);
        assert_eq!(meta.column_names.len(), 3);
        assert!(meta.column_names.contains(&"database.host".to_string()));
        assert!(meta.column_names.contains(&"database.port".to_string()));
        assert!(meta.column_names.contains(&"app.debug".to_string()));

        if let Some(FormatMetadata::Ini(IniMetadata { num_sections, section_names, num_keys, .. })) =
            &meta.format_specific
        {
            assert_eq!(*num_sections, 2);
            assert_eq!(*num_keys, 3);
            assert_eq!(section_names, &["database", "app"]);
        } else {
            panic!("expected Ini format_specific");
        }
    }

    #[test]
    fn ini_type_inference() {
        let tmp = write_temp(
            ".ini",
            "[types]\nflag = true\ncount = 42\nratio = 3.14\nname = hello\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        let find = |name: &str| meta.columns.iter().find(|c| c.name.ends_with(name)).unwrap();
        assert_eq!(find("flag").data_type, fmeta::DataType::Boolean);
        assert_eq!(find("count").data_type, fmeta::DataType::Int64);
        assert_eq!(find("ratio").data_type, fmeta::DataType::Float64);
        assert_eq!(find("name").data_type, fmeta::DataType::String);
    }

    #[test]
    fn ini_global_section() {
        let tmp = write_temp(".ini", "key = value\n[section]\nother = 1\n");
        let meta = extract_metadata(tmp.path()).unwrap();
        assert!(meta.column_names.contains(&"key".to_string()));
        assert!(meta.column_names.contains(&"section.other".to_string()));
    }
}

// ============================================================================
// TOML
// ============================================================================

#[cfg(feature = "toml")]
mod toml_tests {
    use super::*;
    use fmeta::{extract_metadata, format::FormatMetadata, format::TomlMetadata};

    #[test]
    fn basic_toml_extraction() {
        let tmp = write_temp(".toml", "name = \"test\"\nversion = \"1.0\"\nenabled = true\n");
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.format, fmeta::format::FileFormat::Toml);
        assert_eq!(meta.column_names.len(), 3);
        assert!(meta.column_names.contains(&"name".to_string()));
        assert_eq!(meta.num_rows, Some(1));
    }

    #[test]
    fn toml_array_of_tables() {
        let tmp = write_temp(
            ".toml",
            "[[items]]\nid = 1\nname = \"a\"\n\n[[items]]\nid = 2\nname = \"b\"\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        // Array of tables → tabular mode
        assert_eq!(meta.num_rows, Some(2));
        assert!(meta.column_names.contains(&"id".to_string()));
        assert!(meta.column_names.contains(&"name".to_string()));
    }

    #[test]
    fn toml_nested_depth() {
        let tmp = write_temp(
            ".toml",
            "[a]\n[a.b]\n[a.b.c]\nkey = 1\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Toml(TomlMetadata { max_depth, .. })) = &meta.format_specific {
            assert!(*max_depth >= 3, "depth should be at least 3, got {}", max_depth);
        } else {
            panic!("expected Toml format_specific");
        }
    }

    #[test]
    fn toml_type_inference() {
        let tmp = write_temp(
            ".toml",
            "flag = true\ncount = 42\nratio = 3.14\nname = \"hello\"\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        let find = |name: &str| meta.columns.iter().find(|c| c.name == name).unwrap();
        assert_eq!(find("flag").data_type, fmeta::DataType::Boolean);
        assert_eq!(find("count").data_type, fmeta::DataType::Int64);
        assert_eq!(find("ratio").data_type, fmeta::DataType::Float64);
        assert_eq!(find("name").data_type, fmeta::DataType::String);
    }
}

// ============================================================================
// YAML
// ============================================================================

#[cfg(feature = "yaml")]
mod yaml_tests {
    use super::*;
    use fmeta::{extract_metadata, format::FormatMetadata, format::YamlMetadata};

    #[test]
    fn basic_yaml_extraction() {
        let tmp = write_temp(".yaml", "name: test\nversion: 1\nenabled: true\n");
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.format, fmeta::format::FileFormat::Yaml);
        assert_eq!(meta.column_names.len(), 3);
        assert_eq!(meta.num_rows, Some(1));
    }

    #[test]
    fn yaml_multi_document() {
        let tmp = write_temp(".yaml", "---\na: 1\n---\nb: 2\n");
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Yaml(YamlMetadata { num_documents, .. })) =
            &meta.format_specific
        {
            assert_eq!(*num_documents, 2);
        } else {
            panic!("expected Yaml format_specific");
        }
    }

    #[test]
    fn yaml_sequence_of_mappings() {
        let tmp = write_temp(
            ".yaml",
            "- id: 1\n  name: alice\n- id: 2\n  name: bob\n- id: 3\n  name: carol\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.num_rows, Some(3));
        assert!(meta.column_names.contains(&"id".to_string()));
        assert!(meta.column_names.contains(&"name".to_string()));
    }

    #[test]
    fn yaml_anchors_detected() {
        let tmp = write_temp(
            ".yaml",
            "defaults: &defaults\n  color: red\nitem:\n  <<: *defaults\n  size: large\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Yaml(yaml)) = &meta.format_specific {
            assert!(yaml.has_anchors, "should detect anchors");
        } else {
            panic!("expected Yaml format_specific");
        }
    }
}

// ============================================================================
// XML
// ============================================================================

#[cfg(feature = "xml")]
mod xml_tests {
    use super::*;
    use fmeta::{extract_metadata, format::FormatMetadata, format::XmlMetadata};

    #[test]
    fn basic_xml_extraction() {
        let tmp = write_temp(
            ".xml",
            "<?xml version=\"1.0\"?>\n<root><child>text</child><child>more</child></root>",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.format, fmeta::format::FileFormat::Xml);
        assert!(meta.num_rows.is_none());

        if let Some(FormatMetadata::Xml(XmlMetadata {
            root_element,
            num_elements,
            max_depth,
            ..
        })) = &meta.format_specific
        {
            assert_eq!(root_element, "root");
            assert_eq!(*num_elements, 3); // root + 2 children
            assert_eq!(*max_depth, 2);
        } else {
            panic!("expected Xml format_specific");
        }
    }

    #[test]
    fn xml_namespaces() {
        let tmp = write_temp(
            ".xml",
            "<root xmlns:ns=\"http://example.com\" xmlns=\"http://default.com\"><ns:item/></root>",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Xml(xml)) = &meta.format_specific {
            assert_eq!(xml.namespaces.len(), 2);
        } else {
            panic!("expected Xml format_specific");
        }
    }

    #[test]
    fn xml_attributes_counted() {
        let tmp = write_temp(
            ".xml",
            "<root id=\"1\" class=\"main\"><item type=\"a\"/></root>",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Xml(xml)) = &meta.format_specific {
            assert_eq!(xml.num_attributes, 3);
        } else {
            panic!("expected Xml format_specific");
        }
    }
}

// ============================================================================
// Markdown
// ============================================================================

#[cfg(feature = "markdown")]
mod markdown_tests {
    use super::*;
    use fmeta::{
        extract_metadata, format::FormatMetadata,
    };

    #[test]
    fn basic_markdown_extraction() {
        let tmp = write_temp(
            ".md",
            "# Title\n\nSome text here with words.\n\n## Section\n\nMore words.\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.format, fmeta::format::FileFormat::Markdown);
        assert!(meta.num_rows.is_none());

        if let Some(FormatMetadata::Markdown(md)) = &meta.format_specific {
            assert_eq!(md.headings.len(), 2);
            assert_eq!(md.headings[0].level, 1);
            assert_eq!(md.headings[0].text, "Title");
            assert_eq!(md.headings[1].level, 2);
            assert!(md.word_count > 0);
            assert!(md.line_count > 0);
        } else {
            panic!("expected Markdown format_specific");
        }
    }

    #[test]
    fn markdown_code_blocks() {
        let tmp = write_temp(
            ".md",
            "# Code\n\n```rust\nfn main() {}\n```\n\n```python\nprint('hi')\n```\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Markdown(md)) = &meta.format_specific {
            assert_eq!(md.code_blocks, 2);
            assert!(md.code_languages.contains(&"rust".to_string()));
            assert!(md.code_languages.contains(&"python".to_string()));
        } else {
            panic!("expected Markdown format_specific");
        }
    }

    #[test]
    fn markdown_links_and_images() {
        let tmp = write_temp(
            ".md",
            "# Links\n\n[link1](http://a.com)\n[link2](http://b.com)\n\n![img](img.png)\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Markdown(md)) = &meta.format_specific {
            assert_eq!(md.link_count, 2);
            assert_eq!(md.image_count, 1);
        } else {
            panic!("expected Markdown format_specific");
        }
    }

    #[test]
    fn markdown_front_matter() {
        let tmp = write_temp(
            ".md",
            "---\ntitle: Test\ndate: 2024-01-01\n---\n\n# Content\n\nBody text.\n",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Markdown(md)) = &meta.format_specific {
            assert!(md.has_front_matter);
            // Content after front matter should still be parsed
            assert_eq!(md.headings.len(), 1);
        } else {
            panic!("expected Markdown format_specific");
        }
    }
}

// ============================================================================
// HTML
// ============================================================================

#[cfg(feature = "html")]
mod html_tests {
    use super::*;
    use fmeta::{extract_metadata, format::FormatMetadata};

    #[test]
    fn basic_html_extraction() {
        let tmp = write_temp(
            ".html",
            "<!DOCTYPE html>\n<html><head><title>Test Page</title></head>\n\
             <body><h1>Hello</h1><h2>World</h2></body></html>",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.format, fmeta::format::FileFormat::Html);
        assert!(meta.num_rows.is_none());

        if let Some(FormatMetadata::Html(html)) = &meta.format_specific {
            assert_eq!(html.title.as_deref(), Some("Test Page"));
            assert_eq!(html.num_headings, 2);
        } else {
            panic!("expected Html format_specific");
        }
    }

    #[test]
    fn html_meta_tags() {
        let tmp = write_temp(
            ".html",
            "<html><head>\n\
             <meta name=\"description\" content=\"A test page\">\n\
             <meta name=\"author\" content=\"Test\">\n\
             </head><body></body></html>",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Html(html)) = &meta.format_specific {
            assert_eq!(html.meta_tags.len(), 2);
            assert!(html.meta_tags.iter().any(|(k, _)| k == "description"));
            assert!(html.meta_tags.iter().any(|(k, _)| k == "author"));
        } else {
            panic!("expected Html format_specific");
        }
    }

    #[test]
    fn html_element_counts() {
        let tmp = write_temp(
            ".html",
            "<!DOCTYPE html><html><head>\n\
             <link rel=\"stylesheet\" href=\"style.css\">\n\
             <script src=\"app.js\"></script>\n\
             </head><body>\n\
             <a href=\"/\">Home</a>\n\
             <a href=\"/about\">About</a>\n\
             <img src=\"photo.jpg\">\n\
             <form action=\"/submit\"><input></form>\n\
             <table><tr><td>data</td></tr></table>\n\
             </body></html>",
        );
        let meta = extract_metadata(tmp.path()).unwrap();

        if let Some(FormatMetadata::Html(html)) = &meta.format_specific {
            assert_eq!(html.num_links, 2);
            assert_eq!(html.num_scripts, 1);
            assert_eq!(html.num_images, 1);
            assert_eq!(html.num_forms, 1);
            assert_eq!(html.num_tables, 1);
            assert!(html.num_stylesheets >= 1);
        } else {
            panic!("expected Html format_specific");
        }
    }
}
