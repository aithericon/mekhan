#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! NATS JetStream storage for Apalis jobs.
//!
//! - Priority queues (high/medium/low)
//! - DLQ routing on abort errors or after max deliveries
//! - At-least-once delivery, configurable retries with backoff
//! - Optional OpenTelemetry W3C trace propagation
//! - Long-running jobs: progress heartbeats to extend `ack_wait`
//!
//! Basic usage
//! ```rust,no_run
//! use apalis::prelude::*;
//! use apalis_nats::NatsStorage;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Clone, Deserialize, Serialize)]
//! struct Email { to: String }
//!
//! async fn send_email(job: Email) -> Result<(), Error> {
//!     println!("Sending email to: {}", job.to);
//!     Ok(())
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let client = apalis_nats::connect("nats://localhost:4222").await?;
//!     let mut storage = NatsStorage::new(client).await?;
//!
//!     storage.push(Email { to: "user@example.com".into() }).await?;
//!
//!     let worker = WorkerBuilder::new("email-worker")
//!         .backend(storage.clone())
//!         .build_fn(send_email);
//!
//!     Monitor::new().register(worker).run().await?;
//!     Ok(())
//! }
//! ```
//!
//! Priorities
//! ```rust,no_run
//! use apalis::prelude::*;
//! use apalis_nats::{NatsStorage, Priority};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Clone, Deserialize, Serialize)]
//! struct Job { name: String }
//!
//! async fn handle(job: Job) -> Result<(), Error> {
//!     println!("processing {}", job.name);
//!     Ok(())
//! }
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let client = apalis_nats::connect("nats://localhost:4222").await?;
//! let storage = NatsStorage::<Job>::new(client).await?;
//!
//! // Enqueue with explicit priority
//! storage.push_with_priority(Job { name: "high".into() }, Priority::High).await?;
//! storage.push_with_priority(Job { name: "medium".into() }, Priority::Medium).await?;
//! storage.push_with_priority(Job { name: "low".into() }, Priority::Low).await?;
//!
//! let worker = WorkerBuilder::new("priority-worker")
//!     .concurrency(1)
//!     .backend(storage.clone())
//!     .build_fn(handle);
//!
//! Monitor::new().register(worker).run().await?;
//! # Ok(()) }
//! ```
//!
//! Catch Panics (Abort → DLQ/Term)
//! ```rust,no_run
//! use apalis::prelude::*;
//! use apalis_nats::NatsStorage;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Clone, Deserialize, Serialize)]
//! struct Job { panic: bool }
//!
//! async fn handler(job: Job) -> Result<(), Error> {
//!     if job.panic { panic!("boom"); }
//!     Ok(())
//! }
//!
//! # async fn demo2() -> Result<(), Box<dyn std::error::Error>> {
//! let client = apalis_nats::connect("nats://localhost:4222").await?;
//! let storage = NatsStorage::<Job>::new(client).await?;
//! let builder = WorkerBuilder::new("panic-aware");
//! #[cfg(feature = "catch-panic")]
//! let builder = builder.catch_panic();
//! let worker = builder
//!     .backend(storage.clone())
//!     .build_fn(handler);
//!
//! Monitor::new().register(worker).run().await?;
//! # Ok(()) }
//! ```
//!
//! Production Guide
//! - Delivery semantics: at-least-once. Handlers should be idempotent.
//! - Streams: one per priority plus optional DLQ, all under the same `namespace`.
//! - Consumers: shared durable pull consumers per priority provide work-queue semantics.
//! - Heartbeats: for jobs exceeding `ack_wait`, use `NatsContext::progress()` or `ProgressHeartbeatLayer`.
//! - Tracing: logs use `tracing`; enable OpenTelemetry via the `otel` feature. Use `TracingLayer` for automatic CONSUMER span creation with proper parent linkage.
//!
//! Configuration Options (Config)
//! - `namespace: String`
//!   The logical prefix for streams/subjects, e.g., `my_app` creates streams `my_app_high|medium|low` and `my_app_dlq`.
//! - `max_deliver: i64`
//!   Max delivery attempts before routing to DLQ for transient failures. Typical: 3–10.
//! - `ack_wait: Duration`
//!   How long JetStream waits for an ack before redelivery. Must exceed your progress/heartbeat interval.
//!   Typical: 60–120s for long-running jobs; shorter for fast jobs.
//! - `num_replicas: usize`
//!   Stream replicas for HA. Typical: 1 (dev), 3 (prod).
//! - `enable_dlq: bool`
//!   Whether to move failed jobs to `{namespace}.dlq` subject in the `{namespace}_dlq` stream.
//! - `max_ack_pending: i64`
//!   Limits unacked messages per consumer. Tune to match worker concurrency (e.g., 2–4x concurrency).
//! - `nak_backoff: Vec<Duration>`
//!   Backoff schedule for transient errors (Nak with delay). The last value is reused once attempts exceed the list.
//!   Typical: `[100ms, 200ms, 500ms, 1s, 2s, 5s]`.
//! - `enable_tracing` (only with `otel` feature)
//!   When true, inject/extract W3C trace context in NATS headers and link spans across producer/consumer.
//!
//! Recommended Starting Point
//! ```rust
//! # use std::time::Duration;
//! # use apalis_nats::Config;
//! let config = Config {
//!     namespace: "my_app".into(),
//!     max_deliver: 5,
//!     ack_wait: Duration::from_secs(90),
//!     num_replicas: 3,
//!     enable_dlq: true,
//!     max_ack_pending: 200,
//!     nak_backoff: vec![
//!         Duration::from_millis(100),
//!         Duration::from_millis(200),
//!         Duration::from_millis(500),
//!         Duration::from_secs(1),
//!         Duration::from_secs(2),
//!         Duration::from_secs(5),
//!     ],
//!     #[cfg(feature = "otel")]
//!     enable_tracing: true,
//! };
//! ```
//!
//! Operational Tips
//! - Scale workers horizontally; consumers are shared and ensure one-delivery-per-message.
//! - Use `.catch_panic()` so panics become `Error::Abort`, which are Term/DLQ’d deterministically.
//! - Keep handlers idempotent; duplicates can occur (at-least-once).
//! - Monitor JetStream metrics (ack pending, redeliveries, storage) and adjust `ack_wait`, `max_ack_pending`, and backoff.
//! - For scheduling/delays, use an external scheduler or NATS KV with TTL; builtin scheduling isn’t implemented yet.
//!
//! Long-running jobs (auto-heartbeat layer)
//! ```rust,no_run
//! use apalis::prelude::*;
//! use apalis_nats::{NatsStorage, ProgressHeartbeatLayer, Config};
//! use serde::{Deserialize, Serialize};
//! use std::time::Duration;
//!
//! #[derive(Debug, Clone, Deserialize, Serialize)]
//! struct Heavy { size: u64 }
//!
//! async fn do_work(job: Heavy) -> Result<(), Error> {
//!     // Handler does not call progress() explicitly; the layer handles it.
//!     tokio::time::sleep(Duration::from_secs(45)).await;
//!     Ok(())
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let client = apalis_nats::connect("nats://localhost:4222").await?;
//!     let storage = NatsStorage::new_with_config(client, Config { ack_wait: Duration::from_secs(60), ..Default::default() }).await?;
//!
//!     let worker = WorkerBuilder::new("heavy-worker")
//!         .option_layer(Some(ProgressHeartbeatLayer::new(Duration::from_secs(15))))
//!         .backend(storage.clone())
//!         .build_fn(do_work);
//!
//!     Monitor::new().register(worker).run().await?;
//!     Ok(())
//! }
//! ```

mod expose;
mod layers;
#[cfg(feature = "otel")]
mod otel;
mod storage;

pub use crate::layers::ProgressHeartbeatLayer;
#[cfg(feature = "otel")]
pub use crate::layers::TracingLayer;
#[cfg(feature = "otel")]
pub use crate::otel::{attach_span_context, NatsHeaderExtractor, NatsHeaderInjector};
pub use async_nats::{Client, ConnectError, ConnectOptions};
pub use storage::{
    connect, connect_with_credentials, connect_with_options, connect_with_user_pass, Config,
    ConsumerMode, NatsContext, NatsPollError, NatsQueueInfo, NatsStorage, Priority,
};
