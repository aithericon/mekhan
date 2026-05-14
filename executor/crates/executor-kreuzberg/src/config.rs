//! Re-export of wire-format Kreuzberg config types from the shared
//! backend-configs crate, plus a small extension that builds the native
//! `kreuzberg::ExtractionConfig`.
//!
//! Types live in `aithericon-executor-backend-configs::kreuzberg` so the
//! mekhan compiler and the executor share a single source of truth for the
//! JSON shape that crosses the wire. The `build_extraction_config` method
//! stays here because it depends on the heavy `kreuzberg` crate.

pub use aithericon_executor_backend_configs::kreuzberg::{
    ExtractionMode, KreuzbergConfig, OcrSettings, PdfSettings, ResolvedKreuzbergConfig,
};

/// Extension trait providing `build_extraction_config` on the shared
/// [`KreuzbergConfig`]. Kept here (not in backend-configs) because the
/// `kreuzberg` crate is a heavy dependency we don't want compile-time
/// validation in mekhan to pull in.
pub trait KreuzbergConfigExt {
    fn build_extraction_config(&self) -> kreuzberg::ExtractionConfig;
}

impl KreuzbergConfigExt for KreuzbergConfig {
    fn build_extraction_config(&self) -> kreuzberg::ExtractionConfig {
        let ocr = self.ocr.as_ref().map(|ocr| kreuzberg::OcrConfig {
            backend: ocr.backend.clone(),
            language: ocr.language.clone(),
            tesseract_config: if ocr.enable_table_detection {
                Some(kreuzberg::TesseractConfig {
                    enable_table_detection: true,
                    ..Default::default()
                })
            } else {
                None
            },
            output_format: None,
        });

        #[allow(unused_mut)]
        let mut config = kreuzberg::ExtractionConfig {
            force_ocr: self.force_ocr,
            ocr,
            ..Default::default()
        };

        #[cfg(feature = "pdf")]
        if let Some(ref pdf) = self.pdf {
            config.pdf_options = Some(kreuzberg::PdfConfig {
                passwords: pdf.passwords.clone(),
                ..Default::default()
            });
        }

        config
    }
}
