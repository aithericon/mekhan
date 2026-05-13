//! Constant-memory streaming pipeline with optional compression.
//!
//! Used by copy and move operations when cross-backend transfer or
//! compression/decompression is needed. Data flows through the pipeline
//! without ever being fully buffered in memory:
//!
//! ```text
//! reader (AsyncBufRead)
//!   → [decoder (AsyncRead)]
//!     → [BufReader (AsyncBufRead)]
//!       → [encoder (AsyncRead)]
//!         → writer (AsyncWrite)
//! ```
//!
//! Brackets indicate optional stages controlled by the `decompress` and
//! `compress` parameters.

use std::pin::Pin;

use futures::io::{AsyncRead, AsyncBufRead, BufReader};
use opendal::Operator;

use crate::config::Compression;

use super::FileOpsError;

/// Stream data from source to destination with optional decompression/compression.
///
/// Pipeline: `reader (AsyncBufRead) → [decoder] → [BufReader] → [encoder] → writer (AsyncWrite)`
///
/// Returns the number of bytes written to the destination.
pub async fn stream_copy(
    src_op: &Operator,
    src_path: &str,
    dst_op: &Operator,
    dst_path: &str,
    decompress: Option<Compression>,
    compress: Option<Compression>,
) -> Result<u64, FileOpsError> {
    // Build the read side: FuturesAsyncReader implements AsyncBufRead
    let reader = src_op.reader(src_path).await?;
    let async_reader = reader.into_futures_async_read(..).await?;

    // Build the transform pipeline
    let mut pipeline: Pin<Box<dyn AsyncRead + Send>> = match (decompress, compress) {
        (None, None) => {
            // Plain streaming, no transforms
            Box::pin(async_reader)
        }
        (Some(dec), None) => {
            // Decompress only
            wrap_decoder(async_reader, dec)
        }
        (None, Some(enc)) => {
            // Compress only — encoder needs AsyncBufRead, async_reader already is
            wrap_encoder(async_reader, enc)
        }
        (Some(dec), Some(enc)) => {
            // Transcode: decompress → BufReader → compress
            let decoded = wrap_decoder(async_reader, dec);
            let buffered = BufReader::new(decoded);
            wrap_encoder(buffered, enc)
        }
    };

    // Build the write side
    let writer = dst_op.writer(dst_path).await?;
    let mut async_writer = writer.into_futures_async_write();

    // Stream data through the pipeline
    let bytes_written = futures::io::copy(&mut pipeline, &mut async_writer).await?;

    // Flush and finalize
    futures::io::AsyncWriteExt::close(&mut async_writer).await?;

    Ok(bytes_written)
}

/// Wrap a buffered reader in a decompression decoder.
///
/// Decoders consume `AsyncBufRead` and produce `AsyncRead`. The concrete
/// type is erased behind `Pin<Box<dyn AsyncRead + Send>>` so callers can
/// chain decoders and encoders uniformly.
fn wrap_decoder(
    reader: impl AsyncBufRead + Send + 'static,
    algo: Compression,
) -> Pin<Box<dyn AsyncRead + Send>> {
    match algo {
        Compression::Gzip => Box::pin(
            async_compression::futures::bufread::GzipDecoder::new(reader),
        ),
        Compression::Zstd => Box::pin(
            async_compression::futures::bufread::ZstdDecoder::new(reader),
        ),
    }
}

/// Wrap a buffered reader in a compression encoder.
///
/// Encoders consume `AsyncBufRead` and produce `AsyncRead`. When
/// transcoding (decompress + compress), a [`BufReader`] bridge is needed
/// between the decoder output (`AsyncRead`) and the encoder input
/// (`AsyncBufRead`).
fn wrap_encoder(
    reader: impl AsyncBufRead + Send + 'static,
    algo: Compression,
) -> Pin<Box<dyn AsyncRead + Send>> {
    match algo {
        Compression::Gzip => Box::pin(
            async_compression::futures::bufread::GzipEncoder::new(reader),
        ),
        Compression::Zstd => Box::pin(
            async_compression::futures::bufread::ZstdEncoder::new(reader),
        ),
    }
}
