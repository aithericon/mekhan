#[cfg(feature = "audio")]
mod audio_tests {
    use fmeta::{AudioExtractor, FileFormat, FormatMetadata, MetadataExtractor};

    /// Create a minimal valid WAV file (mono, 16-bit, 44100 Hz, 1 second of 440 Hz sine).
    fn create_test_wav() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".wav").unwrap();

        let num_channels: u16 = 1;
        let sample_rate: u32 = 44100;
        let bits_per_sample: u16 = 16;
        let num_samples: u32 = 44100; // 1 second
        let block_align = num_channels * (bits_per_sample / 8);
        let byte_rate = sample_rate * block_align as u32;
        let data_size = num_samples * block_align as u32;

        let mut data = Vec::new();

        // RIFF header
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + data_size).to_le_bytes());
        data.extend_from_slice(b"WAVE");

        // fmt chunk
        data.extend_from_slice(b"fmt ");
        data.extend_from_slice(&16u32.to_le_bytes()); // chunk size
        data.extend_from_slice(&1u16.to_le_bytes()); // PCM format
        data.extend_from_slice(&num_channels.to_le_bytes());
        data.extend_from_slice(&sample_rate.to_le_bytes());
        data.extend_from_slice(&byte_rate.to_le_bytes());
        data.extend_from_slice(&block_align.to_le_bytes());
        data.extend_from_slice(&bits_per_sample.to_le_bytes());

        // data chunk
        data.extend_from_slice(b"data");
        data.extend_from_slice(&data_size.to_le_bytes());

        // Generate a 440 Hz sine wave
        for i in 0..num_samples {
            let t = i as f64 / sample_rate as f64;
            let sample = (t * 440.0 * 2.0 * std::f64::consts::PI).sin() * 16000.0;
            data.extend_from_slice(&(sample as i16).to_le_bytes());
        }

        std::fs::write(tmp.path(), &data).unwrap();
        tmp
    }

    /// Create a stereo WAV file (2 channels, 16-bit, 48000 Hz).
    fn create_test_stereo_wav() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".wav").unwrap();

        let num_channels: u16 = 2;
        let sample_rate: u32 = 48000;
        let bits_per_sample: u16 = 16;
        let num_samples: u32 = 48000; // 1 second per channel
        let block_align = num_channels * (bits_per_sample / 8);
        let byte_rate = sample_rate * block_align as u32;
        let data_size = num_samples * block_align as u32;

        let mut data = Vec::new();

        // RIFF header
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + data_size).to_le_bytes());
        data.extend_from_slice(b"WAVE");

        // fmt chunk
        data.extend_from_slice(b"fmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes()); // PCM
        data.extend_from_slice(&num_channels.to_le_bytes());
        data.extend_from_slice(&sample_rate.to_le_bytes());
        data.extend_from_slice(&byte_rate.to_le_bytes());
        data.extend_from_slice(&block_align.to_le_bytes());
        data.extend_from_slice(&bits_per_sample.to_le_bytes());

        // data chunk
        data.extend_from_slice(b"data");
        data.extend_from_slice(&data_size.to_le_bytes());

        // Interleaved stereo samples (left = sine, right = silence)
        for i in 0..num_samples {
            let t = i as f64 / sample_rate as f64;
            let left = (t * 440.0 * 2.0 * std::f64::consts::PI).sin() * 16000.0;
            data.extend_from_slice(&(left as i16).to_le_bytes()); // left
            data.extend_from_slice(&0i16.to_le_bytes()); // right
        }

        std::fs::write(tmp.path(), &data).unwrap();
        tmp
    }

    // ---- Tests ----

    #[test]
    fn extracts_wav_metadata() {
        let tmp = create_test_wav();
        let meta = AudioExtractor::new().extract(tmp.path()).unwrap();

        assert_eq!(meta.format, FileFormat::Wav);
        assert!(meta.file_size_bytes.is_some());
        assert!(meta.num_rows.is_none());
        assert!(meta.num_columns.is_none());
        assert!(meta.columns.is_empty());
        assert!(meta.column_names.is_empty());
    }

    #[test]
    fn extracts_wav_audio_params() {
        let tmp = create_test_wav();
        let meta = AudioExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Audio(audio)) => {
                assert_eq!(audio.sample_rate, Some(44100));
                assert_eq!(audio.channels, Some(1));
                assert_eq!(audio.bit_depth, Some(16));
                assert!(audio.codec.as_deref().unwrap().contains("pcm"));
            }
            other => panic!("expected Audio format metadata, got: {other:?}"),
        }
    }

    #[test]
    fn extracts_wav_duration() {
        let tmp = create_test_wav();
        let meta = AudioExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Audio(audio)) => {
                let dur = audio.duration_secs.expect("should have duration");
                // Should be approximately 1 second (44100 samples at 44100 Hz)
                assert!(
                    (dur - 1.0).abs() < 0.01,
                    "expected ~1.0s duration, got {dur}"
                );
            }
            other => panic!("expected Audio format metadata, got: {other:?}"),
        }
    }

    #[test]
    fn extracts_stereo_channels() {
        let tmp = create_test_stereo_wav();
        let meta = AudioExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Audio(audio)) => {
                assert_eq!(audio.sample_rate, Some(48000));
                assert_eq!(audio.channels, Some(2));
            }
            other => panic!("expected Audio format metadata, got: {other:?}"),
        }
    }

    #[test]
    fn calculates_bitrate() {
        let tmp = create_test_wav();
        let meta = AudioExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Audio(audio)) => {
                // For 44100 Hz, 16-bit, mono PCM WAV, bitrate should be ~705 kbps
                // (44100 * 16 * 1 = 705600 bps = ~705 kbps)
                // Plus WAV header overhead, actual might be slightly higher
                let bitrate = audio.bitrate_kbps.expect("should have bitrate");
                assert!(
                    bitrate > 600 && bitrate < 800,
                    "expected bitrate ~705, got {bitrate}"
                );
            }
            other => panic!("expected Audio format metadata, got: {other:?}"),
        }
    }

    #[test]
    fn round_trips_through_serde() {
        let tmp = create_test_wav();
        let meta = AudioExtractor::new().extract(tmp.path()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.file_size_bytes, back.file_size_bytes);
    }

    #[test]
    fn file_not_found_error() {
        let result = AudioExtractor::new().extract(std::path::Path::new("/nonexistent/audio.wav"));
        assert!(result.is_err());
    }
}
