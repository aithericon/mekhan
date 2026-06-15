//! gz-aware streaming line source.
//!
//! The real dump collections are `*.data.json.gz` (≈2.3 GB / 3.96M lines), so
//! we MUST stream — never read the whole file. `open()` returns a buffered
//! reader that transparently inflates `.gz` paths via `flate2::MultiGzDecoder`
//! (multi-member aware, in case the dump concatenates gzip streams). The
//! caller iterates `.lines()` and parses one JSON doc per line.

use anyhow::{Context, Result};
use flate2::read::MultiGzDecoder;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// A boxed buffered reader over the (possibly decompressed) collection bytes.
pub type LineSource = Box<dyn BufRead>;

/// Open `path` for streaming line reads. If the path ends in `.gz` the bytes
/// are inflated on the fly. The returned reader is buffered.
pub fn open(path: &Path) -> Result<LineSource> {
    let file =
        File::open(path).with_context(|| format!("open collection file {}", path.display()))?;
    let is_gz = path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("gz"))
        .unwrap_or(false);

    // 1 MiB buffer over the raw file keeps syscalls down for the big dump.
    let raw: Box<dyn Read> = if is_gz {
        Box::new(MultiGzDecoder::new(BufReader::with_capacity(1 << 20, file)))
    } else {
        Box::new(file)
    };
    Ok(Box::new(BufReader::with_capacity(1 << 20, raw)))
}
