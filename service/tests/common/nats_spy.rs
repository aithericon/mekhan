//! A NATS spy over the runner model-command subjects the placement loop publishes
//! on, so a placement test can assert "exactly these Load/Unload/Pull commands were
//! published" without a real runner agent on the other end.
//!
//! ## The subject pattern (confirmed against `service/src/runner_commands.rs`)
//!
//! [`publish_model_command`](mekhan_service::runner_commands::publish_model_command)
//! sends on the runner-scoped CORE-NATS subject `runner.{runner_id}.{verb}`, where
//! `verb` is `load` | `unload` | `pull` (derived from the [`ModelCommand`] variant).
//! It is a fire-and-forget CORE publish (`nats.client().publish`), NOT JetStream —
//! it rides the `runner.{id}.>` SUBSCRIBE grant the runner JWT already carries. The
//! payload is the wire-identical `ModelCommand` JSON
//! (`{kind, target:{Base|Lora}}`).
//!
//! This spy therefore subscribes to the CORE wildcards `runner.*.load`,
//! `runner.*.unload`, and `runner.*.pull` on the SAME NATS the service under test
//! publishes to, parses the runner id out of the SUBJECT and the body into a
//! [`ModelCommand`], and collects the pairs into a shared `Vec` behind a mutex.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::Mutex;
use uuid::Uuid;

use mekhan_service::runner_commands::ModelCommand;

/// One captured model command: the runner id parsed from the subject, the raw
/// subject, and the decoded [`ModelCommand`] body.
#[derive(Clone, Debug)]
pub struct CapturedCommand {
    /// Runner id from the `runner.{id}.{verb}` subject.
    pub runner_id: Uuid,
    /// The full NATS subject the command arrived on.
    pub subject: String,
    /// The decoded command (wire-identical to what the runner agent would consume).
    pub command: ModelCommand,
}

/// A live spy on the runner model-command plane. Holds a background task draining
/// the merged `runner.*.{load,unload,pull}` subscriptions into [`Self::captured`].
pub struct NatsCommandSpy {
    captured: Arc<Mutex<Vec<CapturedCommand>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl NatsCommandSpy {
    /// Subscribe to the runner model-command subjects on `client` and start
    /// draining them into a shared buffer. Connect `client` to the SAME NATS the
    /// service-under-test publishes on (e.g. `async_nats::connect(&common::nats_url())`
    /// or `TestNats::connect().client`).
    ///
    /// The three core subscriptions are established BEFORE this returns, so a
    /// command published immediately after `start` is not missed (CORE NATS has no
    /// replay — a subscription must exist when the message is published).
    pub async fn start(client: async_nats::Client) -> Self {
        let captured: Arc<Mutex<Vec<CapturedCommand>>> = Arc::new(Mutex::new(Vec::new()));

        // Establish all three subscriptions up front (await each) so none races the
        // first publish.
        let load = client
            .subscribe("runner.*.load")
            .await
            .expect("subscribe runner.*.load");
        let unload = client
            .subscribe("runner.*.unload")
            .await
            .expect("subscribe runner.*.unload");
        let pull = client
            .subscribe("runner.*.pull")
            .await
            .expect("subscribe runner.*.pull");

        let buf = captured.clone();
        let task = tokio::spawn(async move {
            // Merge the three streams; each message is one fire-and-forget command.
            let mut merged = futures::stream::select_all(vec![load, unload, pull]);
            while let Some(msg) = merged.next().await {
                let subject = msg.subject.as_str().to_string();
                let Some(runner_id) = parse_runner_id(&subject) else {
                    continue;
                };
                let Ok(command) = serde_json::from_slice::<ModelCommand>(&msg.payload) else {
                    continue;
                };
                buf.lock().await.push(CapturedCommand {
                    runner_id,
                    subject,
                    command,
                });
            }
        });

        Self {
            captured,
            _task: task,
        }
    }

    /// Snapshot all commands captured so far (cloned; the buffer is left intact).
    pub async fn all(&self) -> Vec<CapturedCommand> {
        self.captured.lock().await.clone()
    }

    /// How many commands have been captured so far.
    pub async fn len(&self) -> usize {
        self.captured.lock().await.len()
    }

    /// Drain + return every captured command, clearing the buffer (so a later
    /// assertion only sees commands published after this call).
    pub async fn drain(&self) -> Vec<CapturedCommand> {
        let mut guard = self.captured.lock().await;
        std::mem::take(&mut *guard)
    }

    /// Await until AT LEAST `n` commands have been captured, or `timeout` elapses.
    /// Returns the captured commands on success (a snapshot, buffer left intact);
    /// `Err(count)` with the count seen so far on timeout. Polls on a short tick —
    /// the publish path is fire-and-forget, so there is no completion signal to
    /// await directly.
    pub async fn wait_for(
        &self,
        n: usize,
        timeout: Duration,
    ) -> Result<Vec<CapturedCommand>, usize> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let snapshot = self.all().await;
            if snapshot.len() >= n {
                return Ok(snapshot);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(snapshot.len());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Parse the runner UUID out of a `runner.{id}.{verb}` subject. `None` on any
/// structural mismatch (wrong arity, non-`runner` head, unparseable uuid).
fn parse_runner_id(subject: &str) -> Option<Uuid> {
    let parts: Vec<&str> = subject.split('.').collect();
    // runner.{id}.{load|unload|pull}
    if parts.len() != 3 || parts[0] != "runner" {
        return None;
    }
    if !matches!(parts[2], "load" | "unload" | "pull") {
        return None;
    }
    Uuid::parse_str(parts[1]).ok()
}
