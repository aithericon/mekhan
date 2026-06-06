//! NATS integration testing support.
//!
//! Provides isolated NATS JetStream contexts for parallel test execution.
//! Uses a shared testcontainer for NATS JetStream — one container per test binary.

use async_nats::jetstream::{self, stream::Config as StreamConfig};
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers::ImageExt;
use testcontainers_modules::nats::{Nats, NatsServerCmd};
use tokio::sync::OnceCell;

// ---------------------------------------------------------------------------
// Shared NATS testcontainer (one per test binary)
// ---------------------------------------------------------------------------

struct SharedNats {
    url: String,
    jetstream: jetstream::Context,
    _container: testcontainers::ContainerAsync<Nats>,
}

static SHARED_NATS: OnceCell<SharedNats> = OnceCell::const_new();

async fn shared_nats() -> &'static SharedNats {
    SHARED_NATS
        .get_or_init(|| async {
            let cmd = NatsServerCmd::default().with_jetstream();
            let container = Nats::default()
                .with_cmd(&cmd)
                .start()
                .await
                .expect("Failed to start NATS testcontainer");

            let host = container.get_host().await.expect("get_host");
            let port = container.get_host_port_ipv4(4222).await.expect("get_port");
            let url = format!("nats://{host}:{port}");

            let client = async_nats::connect(&url)
                .await
                .expect("connect to shared NATS testcontainer");
            let jetstream = jetstream::new(client);

            SharedNats {
                url,
                jetstream,
                _container: container,
            }
        })
        .await
}

/// Returns the NATS URL for the shared testcontainer, starting it on first call.
pub async fn shared_nats_url() -> &'static str {
    &shared_nats().await.url
}

/// Returns a JetStream context from the shared testcontainer.
///
/// Starts the container on first call. The returned context is cloned from
/// the shared static — `jetstream::Context` is cheap to clone (Arc-wrapped).
pub async fn shared_jetstream() -> jetstream::Context {
    shared_nats().await.jetstream.clone()
}

/// Create or get the `PETRI_GLOBAL` stream with robust configuration.
///
/// Uses `get_or_create_stream` with fallback to `get_stream` — if the stream
/// already exists with a different config (e.g., from a running engine), the
/// existing stream is reused rather than failing.
pub async fn ensure_global_stream(
    jetstream: &jetstream::Context,
) -> Result<async_nats::jetstream::stream::Stream, Box<dyn std::error::Error + Send + Sync>> {
    let config = StreamConfig {
        name: "PETRI_GLOBAL".to_string(),
        subjects: vec!["petri.>".to_string()],
        max_age: Duration::from_secs(300),
        duplicate_window: Duration::from_secs(120),
        ..Default::default()
    };

    match jetstream.get_or_create_stream(config).await {
        Ok(stream) => Ok(stream),
        Err(_) => {
            // Stream may exist with different config — reuse it
            Ok(jetstream.get_stream("PETRI_GLOBAL").await?)
        }
    }
}

/// Test context with isolated NATS streams.
///
/// Each test run gets unique stream names to avoid conflicts when running
/// tests in parallel or when previous test runs left data behind.
///
/// # Example
///
/// ```ignore
/// use petri_test_harness::nats::{shared_nats_url, NatsTestContext};
///
/// #[tokio::test]
/// async fn test_with_nats() {
///     let url = shared_nats_url().await;
///     let ctx = NatsTestContext::with_url(url).await.unwrap();
///     // Use ctx.jetstream, ctx.events_stream, etc.
///     ctx.cleanup().await.unwrap();
/// }
/// ```
pub struct NatsTestContext {
    /// JetStream context
    pub jetstream: jetstream::Context,

    /// Unique prefix for this test run (e.g., "test_abc123")
    pub prefix: String,

    /// Events stream name (e.g., "TEST_EVENTS_test_abc123")
    pub events_stream: String,

    /// Commands stream name (e.g., "TEST_COMMANDS_test_abc123")
    pub commands_stream: String,

    /// Resources stream name (e.g., "TEST_RESOURCES_test_abc123")
    pub resources_stream: String,

    /// Signals stream name (e.g., "TEST_SIGNALS_test_abc123")
    pub signals_stream: String,

    /// Events subject pattern (e.g., "tns.test_abc123.events.>")
    pub events_subject: String,

    /// Commands subject (e.g., "tns.test_abc123.commands.inject.token")
    pub inject_subject: String,

    /// Resources subject pattern (e.g., "tns.test_abc123.resources.>")
    pub resources_subject: String,

    /// Signals subject pattern (e.g., "tns.test_abc123.signals.>")
    pub signals_subject: String,

    /// NATS URL used for this context
    nats_url: String,
}

impl NatsTestContext {
    /// Create a new test context connecting to a specific NATS URL.
    pub async fn with_url(url: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let client = async_nats::connect(url).await?;
        let jetstream = jetstream::new(client);

        // Generate unique prefix for this test run.
        // Uses `tns` (test namespace) instead of `petri` to avoid subject
        // overlap with the PETRI_GLOBAL stream (`petri.>`) that integration
        // tests create for cross-net bridge and signal testing.
        let prefix = format!("test_{}", uuid::Uuid::new_v4().simple());

        let events_stream = format!("TEST_EVENTS_{}", prefix);
        let commands_stream = format!("TEST_COMMANDS_{}", prefix);
        let resources_stream = format!("TEST_RESOURCES_{}", prefix);
        let signals_stream = format!("TEST_SIGNALS_{}", prefix);
        let events_subject = format!("tns.{}.events.>", prefix);
        let inject_subject = format!("tns.{}.commands.inject.token", prefix);
        let resources_subject = format!("tns.{}.resources.>", prefix);
        let signals_subject = format!("tns.{}.signals.>", prefix);

        let ctx = Self {
            jetstream,
            prefix,
            events_stream,
            commands_stream,
            resources_stream,
            signals_stream,
            events_subject,
            inject_subject,
            resources_subject,
            signals_subject,
            nats_url: url.to_string(),
        };

        // Create isolated streams
        ctx.create_streams().await?;

        Ok(ctx)
    }

    /// Create the test streams.
    async fn create_streams(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Events stream
        self.jetstream
            .create_stream(StreamConfig {
                name: self.events_stream.clone(),
                subjects: vec![format!("tns.{}.events.>", self.prefix)],
                max_age: Duration::from_secs(3600), // 1 hour TTL for test data
                ..Default::default()
            })
            .await?;

        // Commands stream
        self.jetstream
            .create_stream(StreamConfig {
                name: self.commands_stream.clone(),
                subjects: vec![format!("tns.{}.commands.>", self.prefix)],
                max_age: Duration::from_secs(3600),
                ..Default::default()
            })
            .await?;

        // Resources stream (for resource token events)
        self.jetstream
            .create_stream(StreamConfig {
                name: self.resources_stream.clone(),
                subjects: vec![format!("tns.{}.resources.>", self.prefix)],
                max_age: Duration::from_secs(3600),
                ..Default::default()
            })
            .await?;

        // Signals stream (for adapter responses)
        self.jetstream
            .create_stream(StreamConfig {
                name: self.signals_stream.clone(),
                subjects: vec![format!("tns.{}.signals.>", self.prefix)],
                max_age: Duration::from_secs(3600),
                ..Default::default()
            })
            .await?;

        tracing::debug!(
            events_stream = %self.events_stream,
            commands_stream = %self.commands_stream,
            resources_stream = %self.resources_stream,
            signals_stream = %self.signals_stream,
            "Created test streams"
        );

        Ok(())
    }

    /// Get the NATS URL used for this context.
    pub fn nats_url(&self) -> &str {
        &self.nats_url
    }

    /// Get the subject for a specific event type.
    pub fn event_subject(&self, event_type: &str) -> String {
        format!("tns.{}.events.{}", self.prefix, event_type)
    }

    /// Get the subject for a resource token.created event.
    ///
    /// Uses the test prefix for isolation: `tns.{prefix}.resources.{workflow_id}.{place_name}.token.created`
    pub fn resource_subject(&self, workflow_id: uuid::Uuid, place_name: &str) -> String {
        format!(
            "tns.{}.resources.{}.{}.token.created",
            self.prefix, workflow_id, place_name
        )
    }

    /// Get the subject pattern for watching a resource place.
    ///
    /// Returns: `tns.{prefix}.resources.*.{place_name}.token.created`
    pub fn resource_watch_subject(&self, place_name: &str) -> String {
        format!(
            "tns.{}.resources.*.{}.token.created",
            self.prefix, place_name
        )
    }

    /// Clean up test streams.
    ///
    /// Called automatically on drop, but can be called explicitly for immediate cleanup.
    pub async fn cleanup(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Delete streams (ignoring errors if they don't exist)
        let _ = self.jetstream.delete_stream(&self.events_stream).await;
        let _ = self.jetstream.delete_stream(&self.commands_stream).await;
        let _ = self.jetstream.delete_stream(&self.resources_stream).await;
        let _ = self.jetstream.delete_stream(&self.signals_stream).await;

        tracing::debug!(
            events_stream = %self.events_stream,
            commands_stream = %self.commands_stream,
            resources_stream = %self.resources_stream,
            signals_stream = %self.signals_stream,
            "Cleaned up test streams"
        );

        Ok(())
    }

    /// Get stream info for assertions.
    pub async fn events_stream_info(
        &self,
    ) -> Result<jetstream::stream::Info, Box<dyn std::error::Error + Send + Sync>> {
        let mut stream = self.jetstream.get_stream(&self.events_stream).await?;
        Ok(stream.info().await?.clone())
    }

    /// Get message count in events stream.
    pub async fn events_message_count(
        &self,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let info = self.events_stream_info().await?;
        Ok(info.state.messages)
    }

    /// Get message count in resources stream.
    pub async fn resources_message_count(
        &self,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let mut stream = self.jetstream.get_stream(&self.resources_stream).await?;
        let info = stream.info().await?;
        Ok(info.state.messages)
    }

    /// Create a consumer for the events stream.
    pub async fn create_events_consumer(
        &self,
        name: &str,
    ) -> Result<
        async_nats::jetstream::consumer::PullConsumer,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let stream = self.jetstream.get_stream(&self.events_stream).await?;
        let consumer = stream
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(format!("{}_{}", self.prefix, name)),
                filter_subject: format!("tns.{}.events.>", self.prefix),
                ..Default::default()
            })
            .await?;
        Ok(consumer)
    }

    /// Create a consumer for the commands stream (for injection).
    pub async fn create_commands_consumer(
        &self,
        name: &str,
    ) -> Result<
        async_nats::jetstream::consumer::PullConsumer,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let stream = self.jetstream.get_stream(&self.commands_stream).await?;
        let consumer = stream
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(format!("{}_{}", self.prefix, name)),
                filter_subject: self.inject_subject.clone(),
                ..Default::default()
            })
            .await?;
        Ok(consumer)
    }

    /// Create a consumer for the resources stream.
    ///
    /// The filter_subject can be used to filter by workflow_id and/or place_name.
    /// Use `None` to consume all resource events.
    pub async fn create_resources_consumer(
        &self,
        name: &str,
        filter_subject: Option<&str>,
    ) -> Result<
        async_nats::jetstream::consumer::PullConsumer,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let stream = self.jetstream.get_stream(&self.resources_stream).await?;
        let filter = filter_subject
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.resources_subject.clone());
        let consumer = stream
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(format!("{}_{}", self.prefix, name)),
                filter_subject: filter,
                ..Default::default()
            })
            .await?;
        Ok(consumer)
    }

    /// Publish a message to the resources stream.
    ///
    /// Useful for simulating resource events in tests.
    pub async fn publish_resource_event(
        &self,
        subject: &str,
        payload: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.jetstream
            .publish(subject.to_string(), payload.to_vec().into())
            .await?
            .await?;
        Ok(())
    }

    /// Get the subject for a signal message.
    ///
    /// Format: `tns.{prefix}.signals.{workflow_id}.{place_name}`
    pub fn signal_subject(&self, workflow_id: uuid::Uuid, place_name: &str) -> String {
        format!("tns.{}.signals.{}.{}", self.prefix, workflow_id, place_name)
    }

    /// Get the subject pattern for watching all signals to a place.
    ///
    /// Format: `tns.{prefix}.signals.*.{place_name}`
    pub fn signal_watch_subject(&self, place_name: &str) -> String {
        format!("tns.{}.signals.*.{}", self.prefix, place_name)
    }

    /// Create a consumer for the signals stream.
    ///
    /// The filter_subject can be used to filter by workflow_id and/or place_name.
    /// Use `None` to consume all signal events.
    pub async fn create_signals_consumer(
        &self,
        name: &str,
        filter_subject: Option<&str>,
    ) -> Result<
        async_nats::jetstream::consumer::PullConsumer,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let stream = self.jetstream.get_stream(&self.signals_stream).await?;
        let filter = filter_subject
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.signals_subject.clone());
        let consumer = stream
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(format!("{}_{}", self.prefix, name)),
                filter_subject: filter,
                ..Default::default()
            })
            .await?;
        Ok(consumer)
    }

    /// Get message count in signals stream.
    pub async fn signals_message_count(
        &self,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let mut stream = self.jetstream.get_stream(&self.signals_stream).await?;
        let info = stream.info().await?;
        Ok(info.state.messages)
    }
}

/// Check if NATS is available at a specific URL.
pub async fn nats_available_at(url: &str) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(2), async_nats::connect(url)).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_context_creates_isolated_streams() {
        let url = shared_nats_url().await;
        let ctx = NatsTestContext::with_url(url)
            .await
            .expect("Failed to create context");

        // Verify streams exist
        let info = ctx
            .events_stream_info()
            .await
            .expect("Failed to get stream info");
        assert_eq!(info.config.name, ctx.events_stream);

        // Cleanup
        ctx.cleanup().await.expect("Failed to cleanup");
    }

    #[tokio::test]
    async fn test_nats_available_check() {
        let url = shared_nats_url().await;
        let available = nats_available_at(url).await;
        assert!(available, "Shared NATS testcontainer should be available");
    }
}
