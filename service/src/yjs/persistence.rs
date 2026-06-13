use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::{Any, Array, Doc, Map, ReadTxn, StateVector, Transact, Update};

use crate::models::template::WorkflowGraph;
use crate::yjs::doc_ops;

const COMPACTION_THRESHOLD: i64 = 100;

#[derive(Debug, thiserror::Error)]
pub enum YjsPersistenceError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("yrs decoding error: {0}")]
    Decode(String),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct YjsPersistence {
    pool: PgPool,
}

impl YjsPersistence {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Load all raw update data from the database for a template.
    /// Returns (optional_snapshot_data, incremental_updates_after_snapshot).
    pub async fn load_raw_updates(
        &self,
        template_id: Uuid,
    ) -> Result<(Option<Vec<u8>>, Vec<Vec<u8>>), YjsPersistenceError> {
        let snapshot: Option<(Vec<u8>, i64)> = sqlx::query_as(
            "SELECT snapshot_data, snapshot_seq FROM yjs_snapshots WHERE template_id = $1",
        )
        .bind(template_id)
        .fetch_optional(&self.pool)
        .await?;

        let after_seq = snapshot.as_ref().map(|(_, seq)| *seq).unwrap_or(0);

        let updates: Vec<(Vec<u8>,)> = sqlx::query_as(
            "SELECT update_data FROM yjs_documents WHERE template_id = $1 AND seq > $2 ORDER BY seq ASC",
        )
        .bind(template_id)
        .bind(after_seq)
        .fetch_all(&self.pool)
        .await?;

        let snapshot_data = snapshot.map(|(data, _)| data);
        let update_data: Vec<Vec<u8>> = updates.into_iter().map(|(d,)| d).collect();

        Ok((snapshot_data, update_data))
    }

    /// Build a yrs::Doc from raw snapshot + incremental updates.
    /// This is a synchronous operation -- must be called from a sync context or spawn_blocking.
    pub fn build_doc_from_raw(
        snapshot: Option<&[u8]>,
        updates: &[Vec<u8>],
    ) -> Result<Doc, YjsPersistenceError> {
        let doc = Doc::new();

        if let Some(snapshot_data) = snapshot {
            let update = Update::decode_v1(snapshot_data)
                .map_err(|e| YjsPersistenceError::Decode(e.to_string()))?;
            let mut txn = doc.transact_mut();
            txn.apply_update(update)
                .map_err(|e| YjsPersistenceError::Decode(e.to_string()))?;
        }

        if !updates.is_empty() {
            let mut txn = doc.transact_mut();
            for update_data in updates {
                let update = Update::decode_v1(update_data)
                    .map_err(|e| YjsPersistenceError::Decode(e.to_string()))?;
                txn.apply_update(update)
                    .map_err(|e| YjsPersistenceError::Decode(e.to_string()))?;
            }
        }

        Ok(doc)
    }

    /// Load a Yjs Doc from the database.
    /// Returns the Doc -- caller must use it in a sync context (or spawn_blocking).
    pub async fn load_doc(&self, template_id: Uuid) -> Result<Doc, YjsPersistenceError> {
        let (snapshot, updates) = self.load_raw_updates(template_id).await?;
        Self::build_doc_from_raw(snapshot.as_deref(), &updates)
    }

    /// Store an incremental update. Returns the sequence number.
    /// Triggers compaction when the update count exceeds the threshold.
    pub async fn store_update(
        &self,
        template_id: Uuid,
        update: &[u8],
    ) -> Result<i64, YjsPersistenceError> {
        let (seq,): (i64,) = sqlx::query_as(
            "INSERT INTO yjs_documents (template_id, update_data) VALUES ($1, $2) RETURNING seq",
        )
        .bind(template_id)
        .bind(update)
        .fetch_one(&self.pool)
        .await?;

        // Check if compaction is needed
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM yjs_documents WHERE template_id = $1")
                .bind(template_id)
                .fetch_one(&self.pool)
                .await?;

        if count > COMPACTION_THRESHOLD {
            tracing::info!(
                template_id = %template_id,
                count,
                "triggering Yjs doc compaction"
            );
            let persistence = self.clone();
            tokio::spawn(async move {
                if let Err(e) = persistence.compact_from_db(template_id).await {
                    tracing::error!(template_id = %template_id, "compaction failed: {e}");
                }
            });
        }

        Ok(seq)
    }

    /// Compact a document from DB: fold the snapshot + trailing updates into a
    /// fresh snapshot and delete the rows it now covers.
    ///
    /// The seq the snapshot is stamped with MUST be the highest seq actually
    /// folded into it — NOT a separately-read `MAX(seq)`. A concurrent
    /// `store_update` (a connected editor mid-session) can land a row between the
    /// update read and a separate `MAX(seq)`; stamping the snapshot at that
    /// higher max and then `DELETE seq <= max` erases updates the snapshot never
    /// encoded. The in-memory room keeps them, so the loss stays invisible until
    /// the room is evicted and the doc reloads from this lossy persistence —
    /// surfacing as "recent (non-published) edits vanished" on publish or
    /// reopen. Deleting strictly `seq <= max_included_seq` leaves any racing
    /// row in place for the next reconstruct/compaction.
    async fn compact_from_db(&self, template_id: Uuid) -> Result<(), YjsPersistenceError> {
        let snapshot: Option<(Vec<u8>, i64)> = sqlx::query_as(
            "SELECT snapshot_data, snapshot_seq FROM yjs_snapshots WHERE template_id = $1",
        )
        .bind(template_id)
        .fetch_optional(&self.pool)
        .await?;
        let prev_seq = snapshot.as_ref().map(|(_, s)| *s).unwrap_or(0);
        let snapshot_data = snapshot.map(|(d, _)| d);

        // Read the trailing updates WITH their seqs so the snapshot is stamped
        // at exactly the max seq it folds in (race-safe — see the doc comment).
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT seq, update_data FROM yjs_documents WHERE template_id = $1 AND seq > $2 ORDER BY seq ASC",
        )
        .bind(template_id)
        .bind(prev_seq)
        .fetch_all(&self.pool)
        .await?;

        // Nothing new layered on the existing snapshot — leave it untouched.
        if rows.is_empty() {
            return Ok(());
        }
        let max_included_seq = rows.last().map(|(s, _)| *s).unwrap_or(prev_seq);
        let updates: Vec<Vec<u8>> = rows.into_iter().map(|(_, d)| d).collect();

        // Encode the full state in spawn_blocking (yrs types are !Send)
        let state = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, YjsPersistenceError> {
            let doc = Self::build_doc_from_raw(snapshot_data.as_deref(), &updates)?;
            let txn = doc.transact();
            Ok(txn.encode_state_as_update_v1(&StateVector::default()))
        })
        .await
        .map_err(|e| YjsPersistenceError::Decode(format!("spawn_blocking: {e}")))??;

        // Upsert the snapshot, but only ever advance it: a concurrent compaction
        // may have already written a snapshot covering a higher seq, and clobbering
        // it with our lower-seq state would reintroduce the very loss this guards.
        sqlx::query(
            r#"
            INSERT INTO yjs_snapshots (template_id, snapshot_data, snapshot_seq)
            VALUES ($1, $2, $3)
            ON CONFLICT (template_id) DO UPDATE
            SET snapshot_data = $2, snapshot_seq = $3, updated_at = NOW()
            WHERE yjs_snapshots.snapshot_seq < EXCLUDED.snapshot_seq
            "#,
        )
        .bind(template_id)
        .bind(&state)
        .bind(max_included_seq)
        .execute(&self.pool)
        .await?;

        // Delete ONLY the rows this snapshot folded in. Rows that raced in with a
        // higher seq survive (covered by a later compaction); if another
        // compaction already advanced past us, these were already covered too.
        sqlx::query("DELETE FROM yjs_documents WHERE template_id = $1 AND seq <= $2")
            .bind(template_id)
            .bind(max_included_seq)
            .execute(&self.pool)
            .await?;

        tracing::info!(
            template_id = %template_id,
            snapshot_seq = max_included_seq,
            "compacted Yjs document"
        );

        Ok(())
    }

    /// Initialize a Y.Doc from an existing WorkflowGraph and persist as the first update.
    ///
    /// Y.Doc schema (must match the frontend's YjsGraphBinding):
    ///   Y.Map("nodes")    ← keyed by nodeId → Y.Map { type, label, description?, position, config (Y.Map), files (Y.Map) }
    ///   Y.Array("edges")  ← [Any { id, source, target, sourceHandle?, targetHandle?, label?, join?, type }]
    ///   Y.Map("viewport") ← { x, y, zoom }
    pub async fn init_doc_from_graph(
        &self,
        template_id: Uuid,
        graph: &WorkflowGraph,
    ) -> Result<(), YjsPersistenceError> {
        self.init_doc_from_graph_with_files(template_id, graph, &HashMap::new())
            .await
    }

    /// Same as `init_doc_from_graph` but also seeds per-node files (filename →
    /// inline contents). Used by `create_template` so seed templates
    /// (showcase, GitOps imports) land ready-to-publish.
    pub async fn init_doc_from_graph_with_files(
        &self,
        template_id: Uuid,
        graph: &WorkflowGraph,
        files: &HashMap<String, HashMap<String, String>>,
    ) -> Result<(), YjsPersistenceError> {
        let graph = graph.clone();
        let files = files.clone();

        // All yrs work in spawn_blocking (yrs types are !Send)
        let update =
            tokio::task::spawn_blocking(move || -> Result<Vec<u8>, YjsPersistenceError> {
                let doc = doc_ops::graph_to_doc_with_files(&graph, &files);
                let txn = doc.transact();
                Ok(txn.encode_state_as_update_v1(&StateVector::default()))
            })
            .await
            .map_err(|e| YjsPersistenceError::Decode(format!("spawn_blocking: {e}")))??;

        self.store_update(template_id, &update).await?;

        tracing::info!(
            template_id = %template_id,
            "initialized Y.Doc from graph"
        );

        Ok(())
    }

    /// Check whether a Yjs document exists for a template.
    pub async fn has_doc(&self, template_id: Uuid) -> Result<bool, YjsPersistenceError> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM yjs_documents WHERE template_id = $1")
                .bind(template_id)
                .fetch_one(&self.pool)
                .await?;

        if count > 0 {
            return Ok(true);
        }

        let (snap_count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM yjs_snapshots WHERE template_id = $1")
                .bind(template_id)
                .fetch_one(&self.pool)
                .await?;

        Ok(snap_count > 0)
    }
}

// ---------------------------------------------------------------------------
// Helpers for Y.Doc ↔ JSON conversion
// ---------------------------------------------------------------------------

/// Convert serde_json::Value → yrs Any for storage in Y.Doc.
pub fn json_value_to_any(value: &serde_json::Value) -> Any {
    match value {
        serde_json::Value::Null => Any::Null,
        serde_json::Value::Bool(b) => Any::Bool(*b),
        serde_json::Value::Number(n) => Any::Number(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Any::String(Arc::from(s.as_str())),
        serde_json::Value::Array(arr) => Any::Array(Arc::from(
            arr.iter().map(json_value_to_any).collect::<Vec<_>>(),
        )),
        serde_json::Value::Object(obj) => {
            let map: HashMap<String, Any> = obj
                .iter()
                .map(|(k, v)| (k.clone(), json_value_to_any(v)))
                .collect();
            Any::Map(Arc::from(map))
        }
    }
}

/// Convert yrs Any → serde_json::Value for reconstruction.
/// Yjs has a single numeric type (IEEE-754 double): an integer written by the
/// editor (e.g. `retryPolicy.baseDelayMs = 0`) round-trips through yrs as
/// `Any::Number(0.0)`. `serde_json` will not deserialize a float literal into
/// an integer model field (`u32`/`u64`/…), so a whole-valued double must be
/// emitted as a JSON integer. Deserializing an integer into an `f64` field
/// (positions, zoom) is lossless, so this coercion is safe in both directions.
fn json_number_from_f64(n: f64) -> serde_json::Value {
    if n.is_finite() && n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
        serde_json::Value::Number((n as i64).into())
    } else {
        serde_json::json!(n)
    }
}

pub fn any_to_json_value(value: &Any) -> serde_json::Value {
    match value {
        Any::Null | Any::Undefined => serde_json::Value::Null,
        Any::Bool(b) => serde_json::Value::Bool(*b),
        Any::Number(n) => json_number_from_f64(*n),
        Any::BigInt(n) => serde_json::json!(*n),
        Any::String(s) => serde_json::Value::String(s.to_string()),
        Any::Buffer(_) => serde_json::Value::Null,
        Any::Array(arr) => serde_json::Value::Array(arr.iter().map(any_to_json_value).collect()),
        Any::Map(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), any_to_json_value(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
    }
}

/// Convert a yrs Value (from map.get / array.iter) → serde_json::Value.
pub fn yrs_value_to_json(value: &yrs::Out, txn: &impl ReadTxn) -> serde_json::Value {
    match value {
        yrs::Out::Any(any) => any_to_json_value(any),
        yrs::Out::YMap(map) => {
            let mut obj = serde_json::Map::new();
            for (key, val) in map.iter(txn) {
                obj.insert(key.to_string(), yrs_value_to_json(&val, txn));
            }
            serde_json::Value::Object(obj)
        }
        yrs::Out::YArray(arr) => {
            serde_json::Value::Array(arr.iter(txn).map(|v| yrs_value_to_json(&v, txn)).collect())
        }
        _ => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{BackoffKind, RetryPolicy};

    #[test]
    fn json_number_from_f64_coerces_whole_values() {
        assert_eq!(json_number_from_f64(0.0), serde_json::json!(0));
        assert_eq!(json_number_from_f64(3.0), serde_json::json!(3));
        assert_eq!(json_number_from_f64(-2.0), serde_json::json!(-2));
        // Fractional values stay floats.
        assert_eq!(json_number_from_f64(1.5), serde_json::json!(1.5));
    }

    /// Regression: Yjs stores every number as an IEEE-754 double, so an
    /// integer model field round-trips as `Any::Number(0.0)`. Before the
    /// coercion this failed reconstruction with
    /// "invalid type: floating point `0.0`, expected u64", causing
    /// `publish_template` to silently fall back to the stale DB graph.
    #[test]
    fn retry_policy_deserializes_through_yjs_number_roundtrip() {
        let original = RetryPolicy {
            max_retries: 2,
            backoff: BackoffKind::Exponential,
            base_delay_ms: 1000,
        };
        // graph → JSON → Any (Yjs storage: ints become f64) → JSON → model,
        // exactly the path `doc_to_graph` takes for a node's `config`.
        let as_json = serde_json::to_value(original).unwrap();
        let as_any = json_value_to_any(&as_json);
        let back = any_to_json_value(&as_any);
        let restored: RetryPolicy =
            serde_json::from_value(back).expect("must deserialize from Yjs float numbers");
        assert_eq!(restored, original);
    }
}
