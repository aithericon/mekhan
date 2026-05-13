//! Shared symphonia helpers for audio and video backends.

use std::collections::HashMap;
use std::path::Path;

use symphonia::core::codecs;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::error::MetadataError;
use crate::types::AttributeValue;

/// Map a symphonia codec type constant to a human-readable codec name.
pub fn codec_name(codec: codecs::CodecType) -> Option<String> {
    let name = match codec {
        c if c == codecs::CODEC_TYPE_NULL => return None,
        c if c == codecs::CODEC_TYPE_MP3 => "mp3",
        c if c == codecs::CODEC_TYPE_FLAC => "flac",
        c if c == codecs::CODEC_TYPE_AAC => "aac",
        c if c == codecs::CODEC_TYPE_VORBIS => "vorbis",
        c if c == codecs::CODEC_TYPE_PCM_S16LE || c == codecs::CODEC_TYPE_PCM_S16BE => "pcm_s16",
        c if c == codecs::CODEC_TYPE_PCM_S24LE || c == codecs::CODEC_TYPE_PCM_S24BE => "pcm_s24",
        c if c == codecs::CODEC_TYPE_PCM_S32LE || c == codecs::CODEC_TYPE_PCM_S32BE => "pcm_s32",
        c if c == codecs::CODEC_TYPE_PCM_F32LE || c == codecs::CODEC_TYPE_PCM_F32BE => "pcm_f32",
        c if c == codecs::CODEC_TYPE_PCM_F64LE || c == codecs::CODEC_TYPE_PCM_F64BE => "pcm_f64",
        c if c == codecs::CODEC_TYPE_PCM_U8 => "pcm_u8",
        c if c == codecs::CODEC_TYPE_PCM_ALAW => "pcm_alaw",
        c if c == codecs::CODEC_TYPE_PCM_MULAW => "pcm_mulaw",
        other => return Some(format!("unknown({other:?})")),
    };
    Some(name.to_string())
}

/// Convert a symphonia tag value to a string representation.
fn tag_value_to_string(value: &symphonia::core::meta::Value) -> String {
    use symphonia::core::meta::Value;
    match value {
        Value::Binary(b) => format!("<{} bytes>", b.len()),
        Value::Boolean(b) => b.to_string(),
        Value::Flag => "true".to_string(),
        Value::Float(f) => f.to_string(),
        Value::SignedInt(i) => i.to_string(),
        Value::String(s) => s.clone(),
        Value::UnsignedInt(u) => u.to_string(),
    }
}

/// Extract tag key-value pairs from a slice of symphonia tags.
fn collect_tags(tags: &[symphonia::core::meta::Tag]) -> Vec<(String, AttributeValue)> {
    tags.iter()
        .map(|tag| {
            let key = tag
                .std_key
                .map(|sk| format!("{sk:?}").to_lowercase())
                .unwrap_or_else(|| tag.key.clone());
            let value = AttributeValue::String(tag_value_to_string(&tag.value));
            (key, value)
        })
        .collect()
}

/// Probe a media file with symphonia and return the format reader and probe metadata.
pub fn probe_media(
    path: &Path,
) -> Result<
    (
        Box<dyn symphonia::core::formats::FormatReader>,
        symphonia::core::probe::ProbedMetadata,
    ),
    MetadataError,
> {
    let file = std::fs::File::open(path).map_err(|e| MetadataError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probe_result = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| MetadataError::ParseError {
            format: "media".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

    Ok((probe_result.format, probe_result.metadata))
}

/// Collect all metadata tags from both the probe metadata and the format reader.
pub fn collect_all_tags(
    probe_metadata: &mut symphonia::core::probe::ProbedMetadata,
    format_reader: &mut dyn symphonia::core::formats::FormatReader,
) -> HashMap<String, AttributeValue> {
    let mut attributes = HashMap::new();

    // Read from probe metadata (e.g., ID3 tags found before the container)
    if let Some(meta) = probe_metadata.get() {
        if let Some(rev) = meta.current() {
            for (k, v) in collect_tags(rev.tags()) {
                attributes.insert(k, v);
            }
        }
    }

    // Read from format reader metadata (container-level tags)
    {
        let meta = format_reader.metadata();
        if let Some(rev) = meta.current() {
            for (k, v) in collect_tags(rev.tags()) {
                attributes.insert(k, v);
            }
        }
    }

    attributes
}
