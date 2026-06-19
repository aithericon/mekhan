//! HTTP-brokered [`BatchSink`] — the single-origin fold transport for an
//! external zero-secret runner.
//!
//! A runner enrolled over the NATS WebSocket front door can't reliably receive
//! a JetStream publish-ack for its fold batches: the batch DOES land in the
//! `INVENTORY_FOLD` stream, but the ack never returns over the WS connection,
//! so [`NatsBatchSink`](crate::fold_sink::NatsBatchSink) times out and fails
//! the crawl step (and the step failure blocks the server `adopt`, stranding
//! inventory under the nil workspace). This sink instead POSTs each batch to
//! mekhan (`POST {base}/api/storage/fold`, runner-bearer authed), where mekhan
//! folds it straight into the inventory via the same ingest path the NATS
//! consumer uses. Fold joins the storage + secret brokers as a single-origin
//! channel — NATS-publish-ack, S3, and Vault all stay off the runner.

use std::time::Duration;

use async_trait::async_trait;

use aithericon_executor_backend::traits::BatchSink;
use aithericon_executor_domain::FoldBatch;

/// Mekhan-brokered fold-batch sink (HTTP POST, no JetStream).
#[derive(Clone)]
pub struct BrokeredBatchSink {
    url: String,
    runner_token: String,
    /// Runner serve identity stamped onto every published batch — matches
    /// [`NatsBatchSink`](crate::fold_sink::NatsBatchSink) so the fold consumer
    /// persists provenance and file-server `adopt` can auto-stamp an endpoint.
    serve_group: Option<String>,
    client: reqwest::Client,
}

impl BrokeredBatchSink {
    pub fn new(base_url: String, runner_token: String, serve_group: Option<String>) -> Self {
        let url = format!("{}/api/storage/fold", base_url.trim_end_matches('/'));
        Self {
            url,
            runner_token,
            serve_group,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl BatchSink for BrokeredBatchSink {
    async fn publish(&self, batch: &FoldBatch) -> Result<(), String> {
        // Stamp the runner identity without forcing the caller to know it
        // (same precedence as `NatsBatchSink`).
        let mut batch = batch.clone();
        if batch.serve_group.is_none() {
            batch.serve_group = self.serve_group.clone();
        }

        // In-sink retry with exponential backoff (1s..32s) — mekhan may be
        // briefly unavailable (rolling deploy). The fold ingest's upserts are
        // idempotent on `(file_server_id, path)`, so a retried POST is harmless,
        // and the crawl cursor only advances on a 2xx — same durability contract
        // as the NATS sink's publish-ack.
        let mut delay = Duration::from_secs(1);
        let mut last_err = String::new();
        for attempt in 1..=6u32 {
            match self
                .client
                .post(&self.url)
                .bearer_auth(&self.runner_token)
                .json(&batch)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    last_err = format!("fold broker {} -> {status}: {body}", self.url);
                }
                Err(e) => last_err = format!("fold broker POST {}: {e}", self.url),
            }
            if attempt < 6 {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(32));
            }
        }
        Err(last_err)
    }
}
