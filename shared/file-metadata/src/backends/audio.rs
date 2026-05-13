use std::path::Path;

use symphonia::core::codecs;

use crate::detect::detect_from_extension;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{AudioMetadata, FileFormat, FormatMetadata};
use crate::types::FileMetadata;

use super::media_common::{codec_name, collect_all_tags, probe_media};

/// Audio metadata extractor.
///
/// Uses symphonia to probe audio files and extract:
/// - Sample rate, channels, bit depth
/// - Duration and bitrate
/// - Codec identification
/// - Metadata tags (ID3, Vorbis comments, etc.)
///
/// Supports MP3, FLAC, WAV, OGG/Vorbis, AAC, AIFF, and more.
pub struct AudioExtractor;

impl AudioExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AudioExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for AudioExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        if !path.exists() {
            return Err(MetadataError::FileNotFound(path.to_path_buf()));
        }

        let (mut format_reader, mut probe_metadata) = probe_media(path)?;

        // Extract track parameters in a block to release the immutable borrow
        let track_info = {
            let tracks = format_reader.tracks();
            let track = tracks
                .iter()
                .find(|t| t.codec_params.codec != codecs::CODEC_TYPE_NULL)
                .or_else(|| tracks.first());

            track.map(|t| {
                let p = &t.codec_params;
                (
                    p.sample_rate,
                    p.channels.map(|c| c.count() as u32),
                    p.bits_per_sample,
                    p.n_frames,
                    p.time_base,
                    p.codec,
                )
            })
        };

        let (sample_rate, channels, bit_depth, duration_secs, codec_str) = match track_info {
            Some((sr, ch, bd, n_frames, time_base, codec)) => {
                let dur = n_frames.and_then(|frames| {
                    time_base.map(|tb| frames as f64 * tb.numer as f64 / tb.denom as f64)
                });
                (sr, ch, bd, dur, codec_name(codec))
            }
            None => (None, None, None, None, None),
        };

        // Collect metadata tags (needs &mut format_reader)
        let attributes = collect_all_tags(&mut probe_metadata, &mut *format_reader);

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());
        let bitrate_kbps = file_size.zip(duration_secs).and_then(|(size, dur)| {
            if dur > 0.0 {
                Some((size as f64 * 8.0 / dur / 1000.0) as u32)
            } else {
                None
            }
        });

        let format = detect_from_extension(path).unwrap_or(FileFormat::Unknown("audio".into()));

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
            dimensions: vec![],
            columns: vec![],
            attributes,
            format_specific: Some(FormatMetadata::Audio(AudioMetadata {
                duration_secs,
                sample_rate,
                channels,
                bit_depth,
                bitrate_kbps,
                codec: codec_str,
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
        FileFormat::Mp3
    }

    fn extensions(&self) -> &[&str] {
        &[
            "mp3", "flac", "wav", "wave", "ogg", "oga", "aac", "m4a", "aiff", "aif",
        ]
    }
}
