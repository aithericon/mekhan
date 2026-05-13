use std::path::Path;

use symphonia::core::codecs;

use crate::detect::detect_from_extension;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, VideoMetadata};
use crate::types::FileMetadata;

use super::media_common::{codec_name, collect_all_tags, probe_media};

/// Video container metadata extractor.
///
/// Uses symphonia to probe video containers (MP4, MKV, WebM, AVI) and extract:
/// - Duration and bitrate
/// - Audio track info (codec, channels, sample rate)
/// - Track counts
/// - Metadata tags
///
/// **Limitations**: symphonia is primarily an audio library. Video-specific
/// parameters (width, height, fps, video codec) are extracted when the container
/// reader exposes them, but may not be available for all formats.
pub struct VideoExtractor;

impl VideoExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for VideoExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// Check whether a track looks like an audio track based on its codec parameters.
fn is_audio_track(params: &symphonia::core::codecs::CodecParameters) -> bool {
    params.sample_rate.is_some() || params.channels.is_some()
}

impl MetadataExtractor for VideoExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        if !path.exists() {
            return Err(MetadataError::FileNotFound(path.to_path_buf()));
        }

        let (mut format_reader, mut probe_metadata) = probe_media(path)?;

        // Classify tracks and extract info in a block to release immutable borrow
        let (audio_track_count, _video_track_count, audio_codec, duration_secs) = {
            let tracks = format_reader.tracks();

            let mut audio_count: u32 = 0;
            let mut video_count: u32 = 0;
            let mut first_audio_codec: Option<String> = None;
            let mut best_duration: Option<f64> = None;

            for track in tracks {
                let p = &track.codec_params;

                if is_audio_track(p) {
                    audio_count += 1;
                    if first_audio_codec.is_none() {
                        first_audio_codec = codec_name(p.codec);
                    }
                    // Try to get duration from audio track
                    if best_duration.is_none() {
                        best_duration = p.n_frames.and_then(|frames| {
                            p.time_base
                                .map(|tb| frames as f64 * tb.numer as f64 / tb.denom as f64)
                        });
                    }
                } else if p.codec != codecs::CODEC_TYPE_NULL {
                    video_count += 1;
                    // Try to get duration from video track if no audio duration yet
                    if best_duration.is_none() {
                        best_duration = p.n_frames.and_then(|frames| {
                            p.time_base
                                .map(|tb| frames as f64 * tb.numer as f64 / tb.denom as f64)
                        });
                    }
                }
            }

            (audio_count, video_count, first_audio_codec, best_duration)
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

        let format = detect_from_extension(path).unwrap_or(FileFormat::Unknown("video".into()));

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
            format_specific: Some(FormatMetadata::Video(VideoMetadata {
                width: None,
                height: None,
                duration_secs,
                fps: None,
                video_codec: None,
                audio_codec,
                bitrate_kbps,
                audio_tracks: if audio_track_count > 0 {
                    Some(audio_track_count)
                } else {
                    None
                },
                subtitle_tracks: None,
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
        FileFormat::Mp4
    }

    fn extensions(&self) -> &[&str] {
        &["mp4", "m4v", "mkv", "webm", "avi"]
    }
}
