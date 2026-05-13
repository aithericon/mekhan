#[cfg(feature = "image")]
mod image_tests {
    use fmeta::{FileFormat, ImageExtractor, MetadataExtractor};

    /// Create a minimal valid 2x3 PNG file (RGB, 8-bit).
    /// This is the smallest valid PNG that exercises width/height parsing.
    fn create_test_png() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".png").unwrap();

        // Minimal PNG: 2x3 pixels, 8-bit RGB, uncompressed
        let mut data: Vec<u8> = Vec::new();

        // PNG signature
        data.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

        // IHDR chunk (13 bytes of data)
        let ihdr_data: Vec<u8> = vec![
            0, 0, 0, 2, // width: 2
            0, 0, 0, 3, // height: 3
            8, // bit depth: 8
            2, // color type: RGB
            0, // compression
            0, // filter
            0, // interlace
        ];
        let ihdr_crc = crc32(b"IHDR", &ihdr_data);
        data.extend_from_slice(&(ihdr_data.len() as u32).to_be_bytes()); // length
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&ihdr_data);
        data.extend_from_slice(&ihdr_crc.to_be_bytes());

        // IDAT chunk - zlib-compressed image data
        // For a 2x3 RGB image: each row = filter_byte + 2*3 = 7 bytes, 3 rows = 21 bytes
        let mut raw_image: Vec<u8> = Vec::new();
        for _ in 0..3 {
            raw_image.push(0); // filter: none
            raw_image.extend_from_slice(&[255, 0, 0, 0, 255, 0]); // 2 RGB pixels
        }
        let compressed = deflate_raw(&raw_image);
        let idat_crc = crc32(b"IDAT", &compressed);
        data.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        data.extend_from_slice(b"IDAT");
        data.extend_from_slice(&compressed);
        data.extend_from_slice(&idat_crc.to_be_bytes());

        // IEND chunk
        let iend_crc = crc32(b"IEND", &[]);
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"IEND");
        data.extend_from_slice(&iend_crc.to_be_bytes());

        std::fs::write(tmp.path(), &data).unwrap();
        tmp
    }

    /// Create a minimal valid BMP file (4x2 pixels, 24-bit).
    fn create_test_bmp() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".bmp").unwrap();

        let width: u32 = 4;
        let height: u32 = 2;
        let row_size = (width * 3).div_ceil(4) * 4; // padded to 4 bytes
        let pixel_data_size = row_size * height;
        let file_size = 54 + pixel_data_size; // header + pixels

        let mut data: Vec<u8> = Vec::new();

        // BMP file header (14 bytes)
        data.extend_from_slice(b"BM");
        data.extend_from_slice(&file_size.to_le_bytes());
        data.extend_from_slice(&[0, 0, 0, 0]); // reserved
        data.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset

        // DIB header (BITMAPINFOHEADER, 40 bytes)
        data.extend_from_slice(&40u32.to_le_bytes()); // header size
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes()); // color planes
        data.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
        data.extend_from_slice(&0u32.to_le_bytes()); // compression (none)
        data.extend_from_slice(&pixel_data_size.to_le_bytes());
        data.extend_from_slice(&2835u32.to_le_bytes()); // h resolution (72 DPI)
        data.extend_from_slice(&2835u32.to_le_bytes()); // v resolution
        data.extend_from_slice(&0u32.to_le_bytes()); // colors in palette
        data.extend_from_slice(&0u32.to_le_bytes()); // important colors

        // Pixel data (BGR, bottom-up, padded rows)
        for _ in 0..height {
            for _ in 0..width {
                data.extend_from_slice(&[0, 128, 255]); // BGR
            }
            // Pad to 4-byte boundary
            data.extend(std::iter::repeat_n(0u8, (row_size - width * 3) as usize));
        }

        std::fs::write(tmp.path(), &data).unwrap();
        tmp
    }

    /// Minimal CRC32 for PNG chunks.
    fn crc32(chunk_type: &[u8], chunk_data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &byte in chunk_type.iter().chain(chunk_data.iter()) {
            crc ^= byte as u32;
            for _ in 0..8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xEDB8_8320;
                } else {
                    crc >>= 1;
                }
            }
        }
        crc ^ 0xFFFF_FFFF
    }

    /// Minimal zlib deflate (stored blocks, no actual compression).
    fn deflate_raw(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        // zlib header: CMF=0x78 (deflate, window 32K), FLG=0x01 (no dict, check bits)
        out.push(0x78);
        out.push(0x01);
        // Single stored (uncompressed) block
        out.push(0x01); // BFINAL=1, BTYPE=00 (stored)
        let len = data.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes()); // NLEN
        out.extend_from_slice(data);
        // Adler-32 checksum
        let adler = adler32(data);
        out.extend_from_slice(&adler.to_be_bytes());
        out
    }

    fn adler32(data: &[u8]) -> u32 {
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in data {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }

    // ---- Tests ----

    #[test]
    fn extracts_png_dimensions() {
        let tmp = create_test_png();
        let meta = ImageExtractor::new().extract(tmp.path()).unwrap();

        assert_eq!(meta.format, FileFormat::Png);
        assert_eq!(meta.dimensions.len(), 2);

        let w = meta.dimensions.iter().find(|d| d.name == "width").unwrap();
        assert_eq!(w.size, Some(2));

        let h = meta.dimensions.iter().find(|d| d.name == "height").unwrap();
        assert_eq!(h.size, Some(3));
    }

    #[test]
    fn extracts_bmp_dimensions() {
        let tmp = create_test_bmp();
        let meta = ImageExtractor::new().extract(tmp.path()).unwrap();

        assert_eq!(meta.format, FileFormat::Bmp);
        assert_eq!(meta.dimensions.len(), 2);

        let w = meta.dimensions.iter().find(|d| d.name == "width").unwrap();
        assert_eq!(w.size, Some(4));

        let h = meta.dimensions.iter().find(|d| d.name == "height").unwrap();
        assert_eq!(h.size, Some(2));
    }

    #[test]
    fn populates_image_format_specific() {
        let tmp = create_test_png();
        let meta = ImageExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(fmeta::FormatMetadata::Image(img)) => {
                assert_eq!(img.width, 2);
                assert_eq!(img.height, 3);
            }
            other => panic!("expected Image format metadata, got: {other:?}"),
        }
    }

    #[test]
    fn no_columns_for_images() {
        let tmp = create_test_png();
        let meta = ImageExtractor::new().extract(tmp.path()).unwrap();

        assert!(meta.columns.is_empty());
        assert!(meta.column_names.is_empty());
        assert_eq!(meta.num_rows, None);
        assert_eq!(meta.num_columns, None);
    }

    #[test]
    fn round_trips_through_serde() {
        let tmp = create_test_png();
        let meta = ImageExtractor::new().extract(tmp.path()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.dimensions.len(), back.dimensions.len());
    }

    #[test]
    fn file_not_found_error() {
        let result = ImageExtractor::new().extract(std::path::Path::new("/nonexistent/photo.png"));
        assert!(result.is_err());
    }
}
