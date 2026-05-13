#[cfg(feature = "video")]
mod video_tests {
    use fmeta::{FileFormat, FormatMetadata, MetadataExtractor, VideoExtractor};

    /// Create a minimal valid WAV file to test the video extractor with an audio-only container.
    /// This verifies that the video extractor handles audio-only files gracefully.
    fn create_test_wav() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".wav").unwrap();

        let num_channels: u16 = 2;
        let sample_rate: u32 = 44100;
        let bits_per_sample: u16 = 16;
        let num_samples: u32 = 44100;
        let block_align = num_channels * (bits_per_sample / 8);
        let byte_rate = sample_rate * block_align as u32;
        let data_size = num_samples * block_align as u32;

        let mut data = Vec::new();

        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + data_size).to_le_bytes());
        data.extend_from_slice(b"WAVE");

        data.extend_from_slice(b"fmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&num_channels.to_le_bytes());
        data.extend_from_slice(&sample_rate.to_le_bytes());
        data.extend_from_slice(&byte_rate.to_le_bytes());
        data.extend_from_slice(&block_align.to_le_bytes());
        data.extend_from_slice(&bits_per_sample.to_le_bytes());

        data.extend_from_slice(b"data");
        data.extend_from_slice(&data_size.to_le_bytes());

        // Interleaved stereo silence
        for _ in 0..num_samples {
            data.extend_from_slice(&0i16.to_le_bytes()); // left
            data.extend_from_slice(&0i16.to_le_bytes()); // right
        }

        std::fs::write(tmp.path(), &data).unwrap();
        tmp
    }

    // ---- Tests ----

    #[test]
    fn extracts_wav_through_video_extractor() {
        // The video extractor should handle audio-only containers gracefully
        let tmp = create_test_wav();
        let meta = VideoExtractor::new().extract(tmp.path()).unwrap();

        // Format is WAV (from extension detection)
        assert_eq!(meta.format, FileFormat::Wav);

        match &meta.format_specific {
            Some(FormatMetadata::Video(video)) => {
                // Audio-only file: video-specific fields are None
                assert_eq!(video.width, None);
                assert_eq!(video.height, None);
                assert_eq!(video.video_codec, None);
                assert_eq!(video.fps, None);

                // But audio track info should be present
                assert!(video.audio_tracks.is_some());
                assert!(video.audio_codec.is_some());

                // Duration should be available from the audio track
                let dur = video.duration_secs.expect("should have duration");
                assert!(
                    (dur - 1.0).abs() < 0.01,
                    "expected ~1.0s duration, got {dur}"
                );
            }
            other => panic!("expected Video format metadata, got: {other:?}"),
        }
    }

    #[test]
    fn video_metadata_serde_round_trip() {
        let tmp = create_test_wav();
        let meta = VideoExtractor::new().extract(tmp.path()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.file_size_bytes, back.file_size_bytes);
    }

    #[test]
    fn file_not_found_error() {
        let result = VideoExtractor::new().extract(std::path::Path::new("/nonexistent/video.mp4"));
        assert!(result.is_err());
    }
}
