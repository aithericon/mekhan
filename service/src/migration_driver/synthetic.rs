//! Synthetic-NAS fixture generator (docs/32 Phase 5, dev-only).
//!
//! Writes a temp-dir tree of known files (known bytes → known SHA-256) and
//! inserts matching `legacy_file_index` baseline rows for SOME of them, so a
//! driver run yields every reconcile class:
//!
//! | fixture                              | reconcile class | after hash-pending |
//! |-------------------------------------|-----------------|--------------------|
//! | on disk + baseline, sizes MATCH     | `verified`      | content_hash=legacy |
//! | on disk + baseline, sizes DIFFER    | `mismatch`      | mismatch (probed hash recorded) |
//! | on disk, NOT in baseline            | `orphan_disk`   | `verified`, content_hash=real sha256, catalogue row |
//! | in baseline, NOT on disk            | `orphan_db`     | (no inventory row; report-only) |
//!
//! The bytes are deterministic so the test can assert the verified file's
//! `content_hash` equals the legacy hash and the orphan_disk file's
//! `content_hash` equals the real SHA-256 of its bytes.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// One synthetic file: its NAS-relative path, its bytes, and the bare-hex
/// SHA-256 of those bytes.
#[derive(Debug, Clone)]
pub struct SyntheticFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub sha256: String,
}

impl SyntheticFile {
    fn new(path: &str, bytes: &[u8]) -> Self {
        Self {
            path: path.to_string(),
            bytes: bytes.to_vec(),
            sha256: sha256_hex(bytes),
        }
    }

    pub fn size(&self) -> i64 {
        self.bytes.len() as i64
    }
}

/// Bare lowercase-hex SHA-256 — the exact shape the probe op emits
/// (`checksum_digest`) and the reconcile join key.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex_lower(&h.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The full synthetic-NAS fixture: the four canonical files + their on-disk
/// root, plus the legacy-baseline rows that were inserted.
pub struct SyntheticNas {
    /// The on-disk root (a `tempfile::TempDir`). Dropped → tree removed.
    pub dir: tempfile::TempDir,
    /// File present on disk + baseline, matching size → `verified`.
    pub verified: SyntheticFile,
    /// File present on disk + baseline, DIFFERING size → `mismatch`.
    pub mismatch: SyntheticFile,
    /// File present on disk, NOT in baseline → `orphan_disk`.
    pub orphan_disk: SyntheticFile,
    /// Path that exists in the baseline but NOT on disk → `orphan_db`.
    pub orphan_db_path: String,
    /// The legacy hash recorded for `orphan_db_path` (never observed on disk).
    pub orphan_db_hash: String,
}

impl SyntheticNas {
    /// Absolute root path of the synthetic NAS (for the driver's `--root`).
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn root_str(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }
}

/// Build the synthetic NAS tree on disk and seed the matching
/// `legacy_file_index` baseline rows for `file_server_id`.
///
/// Writes three real files (verified / mismatch / orphan_disk) under a fresh
/// tempdir, then inserts baseline rows for: the verified file (matching size +
/// known hash), the mismatch file (a DELIBERATELY WRONG size so reconcile flags
/// it), and the orphan_db path (a baseline row with no file on disk). The
/// orphan_disk file gets NO baseline row.
pub async fn build(pool: &PgPool, file_server_id: &str) -> Result<SyntheticNas, anyhow::Error> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().to_path_buf();

    let verified = SyntheticFile::new("docs/report.txt", b"hello verified world\n");
    let mismatch = SyntheticFile::new("data/corrupt.bin", b"these-bytes-are-on-disk-now");
    let orphan_disk = SyntheticFile::new("misc/sub/found.dat", b"i-am-on-disk-but-not-in-the-baseline");

    // Write the three real files.
    write_file(&root, &verified).await?;
    write_file(&root, &mismatch).await?;
    write_file(&root, &orphan_disk).await?;

    // Baseline: verified (matching size), mismatch (WRONG size), orphan_db
    // (path with no on-disk file). orphan_disk gets NO baseline row.
    insert_legacy(pool, file_server_id, &verified.path, &verified.sha256, verified.size()).await?;
    // Deliberately record a size that DIFFERS from the on-disk size → mismatch.
    let wrong_size = mismatch.size() + 999;
    insert_legacy(pool, file_server_id, &mismatch.path, &mismatch.sha256, wrong_size).await?;

    let orphan_db_path = "archive/gone.txt".to_string();
    let orphan_db_hash = sha256_hex(b"this file was deleted from the NAS but lingers in arango");
    insert_legacy(pool, file_server_id, &orphan_db_path, &orphan_db_hash, 55).await?;

    Ok(SyntheticNas {
        dir,
        verified,
        mismatch,
        orphan_disk,
        orphan_db_path,
        orphan_db_hash,
    })
}

async fn write_file(root: &Path, f: &SyntheticFile) -> Result<(), anyhow::Error> {
    let abs: PathBuf = root.join(&f.path);
    if let Some(parent) = abs.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&abs, &f.bytes).await?;
    Ok(())
}

/// Insert one `legacy_file_index` baseline row. `legacy_key` is synthesized as
/// `{file_server_id}:{path}` so it's unique + cleanup-friendly.
async fn insert_legacy(
    pool: &PgPool,
    file_server_id: &str,
    path: &str,
    hash: &str,
    size: i64,
) -> Result<(), anyhow::Error> {
    let legacy_key = format!("{file_server_id}:{path}");
    sqlx::query(
        "INSERT INTO legacy_file_index (legacy_key, file_server_id, path, hash, size) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (legacy_key) DO UPDATE SET \
            hash = EXCLUDED.hash, size = EXCLUDED.size",
    )
    .bind(&legacy_key)
    .bind(file_server_id)
    .bind(path)
    .bind(hash)
    .bind(size)
    .execute(pool)
    .await?;
    Ok(())
}

/// Remove all synthetic state for `file_server_id`: inventory rows, baseline
/// rows, and the catalogue rows whose content_hash this server's rows reference.
pub async fn cleanup(pool: &PgPool, file_server_id: &str) -> Result<(), anyhow::Error> {
    // Catalogue rows referenced by this server's inventory (observed/legacy
    // logical rows have no execution_id, so deleting by content_hash is safe).
    sqlx::query(
        "DELETE FROM catalogue_entries ce \
         USING file_inventory fi \
         WHERE fi.file_server_id = $1 \
           AND fi.content_hash IS NOT NULL \
           AND ce.content_hash = fi.content_hash \
           AND ce.execution_id IS NULL",
    )
    .bind(file_server_id)
    .execute(pool)
    .await?;

    sqlx::query("DELETE FROM file_inventory WHERE file_server_id = $1")
        .bind(file_server_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM legacy_file_index WHERE file_server_id = $1")
        .bind(file_server_id)
        .execute(pool)
        .await?;
    Ok(())
}
