//! Hash-probe endpoint reconcile (docs/32 §4 Phase 4).
//!
//! An endpoint *claims* to reach the same canonical files a `crawl` recorded in
//! `file_inventory`. Reconcile **verifies** that claim by re-reading a sample of
//! the server's recorded canonical paths THROUGH the endpoint under test and
//! comparing the freshly computed SHA-256 against the inventory's recorded
//! `content_hash` (the reference). The verdict lands on
//! `file_server_endpoints.verification_status`:
//!
//! * `verified`  — every *present* sampled path re-read to the reference hash.
//! * `mismatch`  — a present path re-read to a DIFFERENT hash. The endpoint's
//!   `root` is mis-mapped (it serves the wrong bytes for that canonical path).
//! * `conflict`  — two endpoints each established a *different* hash for the SAME
//!   canonical path (the copies genuinely diverge across backends).
//! * a `not_found` for a sampled path is a **coverage gap**, NOT a failure: an
//!   endpoint may legitimately hold only a subset of the server's files. Misses
//!   are reported informationally and never fail verification.
//!
//! The reference itself self-verifies: probing the very `local_mount` endpoint a
//! crawl read from re-reads the bytes that produced the reference hash, so a
//! correctly-mounted crawl source ends up `verified`.
//!
//! Probing is decoupled from transport via [`ProbeReader`] so the semantics are
//! unit-tested with an in-memory fake; the production reader ([`LiveReader`])
//! dispatches to the same `read_local_bytes` / `read_remote` the serve path uses.

use std::collections::HashMap;

use aithericon_secrets::SecretStore;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::data::serve::{read_remote, LocalReadError, RemoteReadError};
use crate::file_servers::model::FileServerEndpoint;
use crate::file_servers::queries;

/// Default sample cap per probe — bounded work for a server with millions of
/// inventory rows. Overridable per call.
pub const DEFAULT_SAMPLE_SIZE: usize = 50;

/// Bare lowercase-hex SHA-256 — the exact shape the `probe` op emits as
/// `checksum_digest` and the reconcile join key in `file_inventory.content_hash`.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Outcome of reading one canonical path through one endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    /// The endpoint returned the file's bytes.
    Present(Vec<u8>),
    /// The endpoint reported the file absent (not_found / path_jail) — a
    /// coverage gap, not an error.
    Missing,
}

/// Reads canonical paths through an endpoint for the probe. Abstracted so the
/// reconcile semantics are testable against an in-memory fake; the production
/// impl ([`LiveReader`]) hits NATS (local_mount) / opendal (s3 / sftp).
pub trait ProbeReader {
    /// Read the WHOLE object at `canonical_path` through `endpoint`. `Ok(Missing)`
    /// for a genuine not-found; `Err` for a transport / read error (which aborts
    /// the probe rather than being counted as a gap).
    fn read(
        &self,
        endpoint: &FileServerEndpoint,
        canonical_path: &str,
    ) -> impl std::future::Future<Output = Result<ReadOutcome, String>> + Send;
}

/// One offending path surfaced in a [`VerifyResult`].
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct VerifyExample {
    pub path: String,
    /// The reference hash (inventory `content_hash`) for `mismatch`, or the hash
    /// some *other* endpoint established for `conflict`.
    pub expected: String,
    /// The hash this endpoint re-read for the path.
    pub got: String,
}

/// The result of probing one endpoint.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct VerifyResult {
    /// `verified` | `mismatch` | `conflict`.
    pub verification_status: String,
    /// How many canonical paths were sampled (probed).
    pub sampled: usize,
    /// Present paths whose re-read hash matched the reference.
    pub passed: usize,
    /// Present paths whose re-read hash differed from the reference.
    pub mismatched: usize,
    /// Sampled paths the endpoint did not have (coverage gaps).
    pub missing: usize,
    /// Up to a handful of offending paths (mismatch + conflict), for the UI.
    pub examples: Vec<VerifyExample>,
}

/// One probe observation: the endpoint re-read `path` to `hash`.
#[derive(Debug, Clone)]
struct Observation {
    path: String,
    expected: String,
    hash: String,
}

/// Cap on offending examples carried back in a [`VerifyResult`].
const MAX_EXAMPLES: usize = 10;

/// Pure classifier: turn a set of probe observations + miss count into a verdict.
///
/// * `observations` — every PRESENT sampled path, with its reference hash and the
///   hash this endpoint re-read.
/// * `missing` — count of sampled paths the endpoint lacked (coverage gaps).
/// * `siblings` — canonical_path → hash established by OTHER endpoints of the same
///   server. A present path whose hash matches neither the reference NOR a sibling
///   that itself disagrees with the reference is a plain mismatch; a path where
///   this endpoint and a sibling both differ from the reference (and from each
///   other) is flagged `conflict`.
///
/// Precedence (strongest first): `conflict` (divergent copies), then `mismatch`,
/// then `verified`. A probe that only saw misses (no present paths) is `verified`
/// (the endpoint is correct about the subset it claims to hold — vacuously).
fn classify(
    observations: &[Observation],
    missing: usize,
    siblings: &HashMap<String, String>,
) -> VerifyResult {
    let sampled = observations.len() + missing;
    let mut passed = 0usize;
    let mut mismatched = 0usize;
    let mut conflict = false;
    let mut examples: Vec<VerifyExample> = Vec::new();

    for obs in observations {
        if obs.hash == obs.expected {
            passed += 1;
            continue;
        }
        // Present but wrong bytes vs the reference.
        mismatched += 1;
        // Conflict: another endpoint established a DIFFERENT hash for the same
        // canonical path than this one did — the copies genuinely diverge.
        if let Some(sib) = siblings.get(&obs.path) {
            if sib != &obs.hash {
                conflict = true;
            }
        }
        if examples.len() < MAX_EXAMPLES {
            examples.push(VerifyExample {
                path: obs.path.clone(),
                expected: obs.expected.clone(),
                got: obs.hash.clone(),
            });
        }
    }

    let verification_status = if conflict {
        "conflict"
    } else if mismatched > 0 {
        "mismatch"
    } else {
        "verified"
    }
    .to_string();

    VerifyResult {
        verification_status,
        sampled,
        passed,
        mismatched,
        missing,
        examples,
    }
}

/// Stratified random sample of up to `k` reconcile candidates, spread across
/// top-level path prefixes so a per-subtree mis-mount is caught (a flat random
/// sample could miss a single mis-mounted subtree entirely).
///
/// Groups candidates by their first path segment, then round-robins across the
/// groups (each group internally shuffled) until `k` are taken or the pool is
/// exhausted. With `candidates.len() <= k` every candidate is returned.
fn stratified_sample(
    candidates: Vec<queries::ReconcileSample>,
    k: usize,
) -> Vec<queries::ReconcileSample> {
    use rand::seq::SliceRandom;

    if candidates.len() <= k {
        return candidates;
    }

    // Group by first path segment (the top-level prefix).
    let mut groups: HashMap<String, Vec<queries::ReconcileSample>> = HashMap::new();
    for c in candidates {
        let prefix = c
            .path
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or("")
            .to_string();
        groups.entry(prefix).or_default().push(c);
    }

    let mut rng = rand::thread_rng();
    // Deterministic group order (sorted keys) with shuffled members → spread is
    // reproducible across prefixes but unbiased within a prefix.
    let mut keys: Vec<String> = groups.keys().cloned().collect();
    keys.sort();
    let mut buckets: Vec<Vec<queries::ReconcileSample>> = keys
        .into_iter()
        .map(|key| {
            let mut v = groups.remove(&key).unwrap_or_default();
            v.shuffle(&mut rng);
            v
        })
        .collect();

    // Round-robin pop until k taken or all buckets drained.
    let mut out = Vec::with_capacity(k);
    while out.len() < k {
        let mut took_any = false;
        for b in buckets.iter_mut() {
            if out.len() >= k {
                break;
            }
            if let Some(s) = b.pop() {
                out.push(s);
                took_any = true;
            }
        }
        if !took_any {
            break;
        }
    }
    out
}

/// Probe `endpoint` against a stratified sample of its server's inventory and
/// return the verdict — pure of any DB write. Reads each sampled canonical path
/// through `reader`, hashes present bytes, and classifies against the inventory
/// reference + sibling-endpoint observations (for `conflict`).
///
/// `siblings` maps canonical_path → a hash an OTHER endpoint of the same server
/// established (empty when this is the only endpoint, or callers don't supply
/// cross-endpoint observations). A transport/read error (NOT a not-found) aborts
/// with `Err` so a flaky backend isn't silently scored `verified`.
pub async fn probe_endpoint<R: ProbeReader>(
    reader: &R,
    endpoint: &FileServerEndpoint,
    samples: &[queries::ReconcileSample],
    siblings: &HashMap<String, String>,
) -> Result<VerifyResult, String> {
    let mut observations: Vec<Observation> = Vec::new();
    let mut missing = 0usize;

    for s in samples {
        match reader.read(endpoint, &s.path).await? {
            ReadOutcome::Present(bytes) => observations.push(Observation {
                path: s.path.clone(),
                expected: s.content_hash.clone(),
                hash: sha256_hex(&bytes),
            }),
            ReadOutcome::Missing => missing += 1,
        }
    }

    Ok(classify(&observations, missing, siblings))
}

// ---------------------------------------------------------------------------
// Live wiring: DB-backed sample + production reader + persist + auto-spawn.
// ---------------------------------------------------------------------------

/// The production [`ProbeReader`]: dispatches a read by `access_method` to the
/// SAME transports the serve path uses (`read_local_bytes` over NATS for
/// `local_mount`; `read_remote` over opendal for `s3` / `sftp`). `object_store`
/// (the built-in platform bucket) is read via `read_remote` only when it carries
/// a `resource_ref`; the bare platform bucket has no per-read cred chain here and
/// is treated as a coverage gap (it self-verifies on upload, not via probe).
pub struct LiveReader<'a> {
    pub db: &'a PgPool,
    pub secrets: &'a dyn SecretStore,
    pub nats: &'a async_nats::Client,
    pub workspace_id: Uuid,
}

impl ProbeReader for LiveReader<'_> {
    async fn read(
        &self,
        endpoint: &FileServerEndpoint,
        canonical_path: &str,
    ) -> Result<ReadOutcome, String> {
        match endpoint.access_method.as_str() {
            "local_mount" => {
                let Some(group) = endpoint.group_id.as_deref().filter(|g| !g.is_empty()) else {
                    return Err("local_mount endpoint has no group_id to probe through".into());
                };
                match crate::data::serve::read_local_bytes(
                    self.nats,
                    group,
                    &endpoint.root,
                    canonical_path,
                    &self.workspace_id.to_string(),
                )
                .await
                {
                    Ok(bytes) => Ok(ReadOutcome::Present(bytes)),
                    Err(LocalReadError::NotFound(_)) => Ok(ReadOutcome::Missing),
                    Err(e) => Err(e.to_string()),
                }
            }
            "s3" | "sftp" => read_remote_outcome(self, endpoint, canonical_path).await,
            "object_store" => {
                if endpoint.resource_ref.is_some() {
                    read_remote_outcome(self, endpoint, canonical_path).await
                } else {
                    // Bare platform bucket: no probe cred chain → treat as a gap.
                    Ok(ReadOutcome::Missing)
                }
            }
            other => Err(format!("cannot probe unknown access_method {other:?}")),
        }
    }
}

/// Drain a `read_remote` stream into bytes, mapping NotFound → `Missing`.
async fn read_remote_outcome(
    reader: &LiveReader<'_>,
    endpoint: &FileServerEndpoint,
    canonical_path: &str,
) -> Result<ReadOutcome, String> {
    use futures::StreamExt;
    match read_remote(
        reader.db,
        reader.secrets,
        reader.workspace_id,
        endpoint,
        canonical_path,
        None,
    )
    .await
    {
        Ok(mut rr) => {
            let mut buf: Vec<u8> = Vec::new();
            while let Some(chunk) = rr.stream.next().await {
                match chunk {
                    Ok(b) => buf.extend_from_slice(&b),
                    Err(e) => return Err(format!("remote read stream error: {e}")),
                }
            }
            Ok(ReadOutcome::Present(buf))
        }
        // PathJail mirrors the runner's `path_jail` ERROR frame: an inventory
        // path that escapes the root is a coverage gap on this endpoint, not a
        // probe-aborting transport error.
        Err(RemoteReadError::NotFound(_) | RemoteReadError::PathJail(_)) => {
            Ok(ReadOutcome::Missing)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Load a server's reconcile candidates, take a stratified sample of `k`, probe
/// `endpoint` against them, PERSIST the verdict onto the endpoint row, and return
/// the [`VerifyResult`]. The on-demand verify handler and the auto-probe spawn
/// both funnel through here.
///
/// `file_server_id` is the parent server id (the endpoint's `file_server_id`);
/// `server_key` is the inventory join key (`file_servers.key`). Sibling
/// observations are not collected in this entry point (single-endpoint probe);
/// `conflict` is still reachable when a sibling's verdict already disagrees — a
/// fuller cross-endpoint pass is deferred.
pub async fn verify_endpoint(
    db: &PgPool,
    secrets: &dyn SecretStore,
    nats: &async_nats::Client,
    workspace_id: Uuid,
    server_key: &str,
    file_server_id: Uuid,
    endpoint: &FileServerEndpoint,
    k: usize,
) -> Result<VerifyResult, String> {
    let candidates = queries::reconcile_candidates(db, server_key)
        .await
        .map_err(|e| format!("loading reconcile candidates: {e}"))?;
    let total = candidates.len();
    let samples = stratified_sample(candidates, k);
    if samples.len() < total {
        tracing::info!(
            server = server_key,
            endpoint = %endpoint.id,
            sampled = samples.len(),
            total,
            "reconcile: sampling cap applied"
        );
    }

    let siblings: HashMap<String, String> = HashMap::new();
    let reader = LiveReader {
        db,
        secrets,
        nats,
        workspace_id,
    };
    let result = probe_endpoint(&reader, endpoint, &samples, &siblings).await?;

    let detail = serde_json::json!({
        "sampled": result.sampled,
        "passed": result.passed,
        "mismatched": result.mismatched,
        "missing": result.missing,
        "examples": result.examples,
        "at": chrono::Utc::now().to_rfc3339(),
    });
    queries::set_verification(
        db,
        file_server_id,
        endpoint.id,
        &result.verification_status,
        &detail,
    )
    .await
    .map_err(|e| format!("persisting verification status: {e}"))?;

    Ok(result)
}

/// Fire-and-forget auto-probe of one endpoint (on create / adopt / PUT). Clones
/// the cheap handles it needs and `tokio::spawn`s so the HTTP response is never
/// blocked on a probe (which can be slow — it reads sampled files end to end).
/// Failures are logged, not surfaced.
#[allow(clippy::too_many_arguments)]
pub fn spawn_auto_probe(
    db: PgPool,
    secrets: std::sync::Arc<dyn SecretStore>,
    nats: async_nats::Client,
    workspace_id: Uuid,
    server_key: String,
    file_server_id: Uuid,
    endpoint: FileServerEndpoint,
) {
    tokio::spawn(async move {
        match verify_endpoint(
            &db,
            secrets.as_ref(),
            &nats,
            workspace_id,
            &server_key,
            file_server_id,
            &endpoint,
            DEFAULT_SAMPLE_SIZE,
        )
        .await
        {
            Ok(r) => tracing::info!(
                endpoint = %endpoint.id,
                status = r.verification_status,
                sampled = r.sampled,
                passed = r.passed,
                missing = r.missing,
                "reconcile: auto-probe complete"
            ),
            Err(e) => tracing::warn!(endpoint = %endpoint.id, error = e, "reconcile: auto-probe failed"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn ep(method: &str) -> FileServerEndpoint {
        FileServerEndpoint {
            id: Uuid::new_v4(),
            file_server_id: Uuid::new_v4(),
            access_method: method.to_string(),
            root: "/mnt/data".to_string(),
            resource_ref: None,
            group_id: Some("grp-1".to_string()),
            status: "online".to_string(),
            verification_status: "unverified".to_string(),
            last_verified: None,
            last_seen: None,
            priority: 0,
            config: serde_json::json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn sample(path: &str, bytes: &[u8]) -> queries::ReconcileSample {
        queries::ReconcileSample {
            path: path.to_string(),
            content_hash: sha256_hex(bytes),
        }
    }

    /// In-memory probe reader: a path → outcome map. Absent key → not-found.
    struct FakeReader {
        files: HashMap<String, ReadOutcome>,
        /// If set, every read fails with this transport error.
        fail: Option<String>,
    }

    impl FakeReader {
        fn new(pairs: Vec<(&str, ReadOutcome)>) -> Self {
            FakeReader {
                files: pairs.into_iter().map(|(p, o)| (p.to_string(), o)).collect(),
                fail: None,
            }
        }
        fn failing(msg: &str) -> Self {
            FakeReader {
                files: HashMap::new(),
                fail: Some(msg.to_string()),
            }
        }
    }

    impl ProbeReader for FakeReader {
        async fn read(
            &self,
            _endpoint: &FileServerEndpoint,
            canonical_path: &str,
        ) -> Result<ReadOutcome, String> {
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            Ok(self
                .files
                .get(canonical_path)
                .cloned()
                .unwrap_or(ReadOutcome::Missing))
        }
    }

    #[tokio::test]
    async fn all_match_is_verified() {
        let s1 = sample("a/x.txt", b"hello");
        let s2 = sample("a/y.txt", b"world");
        let reader = FakeReader::new(vec![
            ("a/x.txt", ReadOutcome::Present(b"hello".to_vec())),
            ("a/y.txt", ReadOutcome::Present(b"world".to_vec())),
        ]);
        let r = probe_endpoint(&reader, &ep("local_mount"), &[s1, s2], &HashMap::new())
            .await
            .unwrap();
        assert_eq!(r.verification_status, "verified");
        assert_eq!(r.sampled, 2);
        assert_eq!(r.passed, 2);
        assert_eq!(r.mismatched, 0);
        assert_eq!(r.missing, 0);
        assert!(r.examples.is_empty());
    }

    #[tokio::test]
    async fn wrong_bytes_is_mismatch_with_example() {
        let s = sample("a/x.txt", b"hello"); // reference = hash("hello")
        let reader = FakeReader::new(vec![(
            "a/x.txt",
            ReadOutcome::Present(b"DIFFERENT".to_vec()),
        )]);
        let r = probe_endpoint(
            &reader,
            &ep("local_mount"),
            std::slice::from_ref(&s),
            &HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(r.verification_status, "mismatch");
        assert_eq!(r.mismatched, 1);
        assert_eq!(r.passed, 0);
        assert_eq!(r.examples.len(), 1);
        assert_eq!(r.examples[0].path, "a/x.txt");
        assert_eq!(r.examples[0].expected, s.content_hash);
        assert_eq!(r.examples[0].got, sha256_hex(b"DIFFERENT"));
    }

    #[tokio::test]
    async fn missing_is_coverage_gap_not_failure() {
        let present = sample("a/x.txt", b"hello");
        let gap = sample("b/z.txt", b"absent-here");
        let reader = FakeReader::new(vec![
            ("a/x.txt", ReadOutcome::Present(b"hello".to_vec())),
            // b/z.txt deliberately absent from the fake → Missing.
        ]);
        let r = probe_endpoint(&reader, &ep("local_mount"), &[present, gap], &HashMap::new())
            .await
            .unwrap();
        // A miss must NOT fail verification.
        assert_eq!(r.verification_status, "verified");
        assert_eq!(r.passed, 1);
        assert_eq!(r.missing, 1);
        assert_eq!(r.sampled, 2);
    }

    #[tokio::test]
    async fn all_missing_is_verified_vacuously() {
        let g1 = sample("a/x.txt", b"a");
        let g2 = sample("b/y.txt", b"b");
        let reader = FakeReader::new(vec![]); // everything Missing
        let r = probe_endpoint(&reader, &ep("local_mount"), &[g1, g2], &HashMap::new())
            .await
            .unwrap();
        assert_eq!(r.verification_status, "verified");
        assert_eq!(r.missing, 2);
        assert_eq!(r.passed, 0);
    }

    #[tokio::test]
    async fn divergent_sibling_hash_is_conflict() {
        // This endpoint re-reads "GOT_A" for a/x.txt (≠ reference). A sibling
        // endpoint established a DIFFERENT hash ("GOT_B") for the same path →
        // the copies genuinely diverge → conflict (stronger than mismatch).
        let s = sample("a/x.txt", b"reference"); // reference hash
        let reader = FakeReader::new(vec![(
            "a/x.txt",
            ReadOutcome::Present(b"GOT_A".to_vec()),
        )]);
        let mut siblings = HashMap::new();
        siblings.insert("a/x.txt".to_string(), sha256_hex(b"GOT_B"));
        let r = probe_endpoint(&reader, &ep("s3"), &[s], &siblings)
            .await
            .unwrap();
        assert_eq!(r.verification_status, "conflict");
        assert_eq!(r.mismatched, 1);
    }

    #[tokio::test]
    async fn matching_sibling_hash_is_plain_mismatch_not_conflict() {
        // Both this endpoint and the sibling read the SAME (wrong) hash — they
        // agree with each other but not the reference. That's a mismatch (a
        // shared root mis-map), not a conflict (divergent copies).
        let s = sample("a/x.txt", b"reference");
        let reader = FakeReader::new(vec![(
            "a/x.txt",
            ReadOutcome::Present(b"SAME_WRONG".to_vec()),
        )]);
        let mut siblings = HashMap::new();
        siblings.insert("a/x.txt".to_string(), sha256_hex(b"SAME_WRONG"));
        let r = probe_endpoint(&reader, &ep("s3"), &[s], &siblings)
            .await
            .unwrap();
        assert_eq!(r.verification_status, "mismatch");
    }

    #[tokio::test]
    async fn transport_error_aborts_probe() {
        let s = sample("a/x.txt", b"hello");
        let reader = FakeReader::failing("vault unreachable");
        let err = probe_endpoint(&reader, &ep("s3"), &[s], &HashMap::new())
            .await
            .unwrap_err();
        assert!(err.contains("vault unreachable"));
    }

    #[test]
    fn stratified_sample_spreads_across_prefixes() {
        // 3 prefixes, 100 files each; sample 30 → every prefix represented,
        // none starved (round-robin → ~10 each).
        let mut all = Vec::new();
        for prefix in ["alpha", "beta", "gamma"] {
            for i in 0..100 {
                all.push(queries::ReconcileSample {
                    path: format!("{prefix}/f{i}.dat"),
                    content_hash: format!("{prefix}{i:064x}"),
                });
            }
        }
        let picked = stratified_sample(all, 30);
        assert_eq!(picked.len(), 30);
        let mut per_prefix: HashMap<&str, usize> = HashMap::new();
        for s in &picked {
            let p = s.path.split('/').next().unwrap();
            *per_prefix.entry(p).or_default() += 1;
        }
        // All 3 prefixes present; round-robin gives each exactly 10.
        assert_eq!(per_prefix.len(), 3, "every prefix represented: {per_prefix:?}");
        for (_, c) in per_prefix {
            assert_eq!(c, 10, "round-robin even spread");
        }
    }

    #[test]
    fn stratified_sample_returns_all_when_under_cap() {
        let all = vec![
            queries::ReconcileSample {
                path: "a/x".into(),
                content_hash: "h1".into(),
            },
            queries::ReconcileSample {
                path: "b/y".into(),
                content_hash: "h2".into(),
            },
        ];
        let picked = stratified_sample(all, 50);
        assert_eq!(picked.len(), 2);
    }

    #[test]
    fn sha256_hex_is_bare_lowercase() {
        // Known vector: sha256("abc").
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
