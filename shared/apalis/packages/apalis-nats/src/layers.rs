use std::time::Duration;

use apalis_core::request::Request;
use tower::{Layer, Service};

use crate::NatsContext;

/// A layer that creates OpenTelemetry CONSUMER spans for job processing.
///
/// This layer automatically:
/// 1. Extracts trace context from NATS message headers (set by the producer's `job.push` span)
/// 2. Creates a `job.process` span with `SpanKind::Consumer`
/// 3. Links the consumer span to the producer span for proper distributed tracing
///
/// This enables service graph generation in observability tools like Grafana Tempo,
/// showing the connection between job producers and consumers.
///
/// # Example
/// ```rust,no_run
/// use apalis::prelude::*;
/// use apalis_nats::{NatsStorage, TracingLayer};
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Debug, Clone, Deserialize, Serialize)]
/// struct MyJob { data: String }
///
/// async fn process_job(job: MyJob) -> Result<(), Error> {
///     Ok(())
/// }
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = apalis_nats::connect("nats://localhost:4222").await?;
/// let storage = NatsStorage::new(client).await?;
///
/// let worker = WorkerBuilder::new("traced-worker")
///     .layer(TracingLayer::new())
///     .backend(storage)
///     .build_fn(process_job);
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "otel")]
#[derive(Clone, Debug)]
pub struct TracingLayer {
    service_name: Option<String>,
}

#[cfg(feature = "otel")]
impl TracingLayer {
    /// Create a new tracing layer with default settings.
    pub fn new() -> Self {
        Self { service_name: None }
    }

    /// Create a new tracing layer with a custom service name for the span.
    pub fn with_service_name(service_name: impl Into<String>) -> Self {
        Self {
            service_name: Some(service_name.into()),
        }
    }
}

#[cfg(feature = "otel")]
impl Default for TracingLayer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "otel")]
impl<S> Layer<S> for TracingLayer {
    type Service = TracingService<S>;

    fn layer(&self, service: S) -> Self::Service {
        TracingService {
            service,
            service_name: self.service_name.clone(),
        }
    }
}

#[cfg(feature = "otel")]
#[derive(Clone, Debug)]
pub struct TracingService<S> {
    service: S,
    service_name: Option<String>,
}

#[cfg(feature = "otel")]
impl<S, Req> Service<Request<Req, NatsContext>> for TracingService<S>
where
    S: Service<Request<Req, NatsContext>> + Send + Clone + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
    S::Response: Send + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = futures::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: Request<Req, NatsContext>) -> Self::Future {
        use tracing::Instrument;
        use tracing_opentelemetry::OpenTelemetrySpanExt;

        let mut inner = self.service.clone();
        let service_name = self.service_name.clone();

        let fut = async move {
            // Get the trace context from the NATS message headers (set by job.push producer)
            let parent_context = request.parts.context.trace_context().cloned();

            // Build span name based on service name
            let span_name = service_name
                .as_deref()
                .map(|name| format!("{}.process", name))
                .unwrap_or_else(|| "job.process".to_string());

            // Create a tracing span - tracing-opentelemetry will convert this to an OTel span
            // Use tracing::info_span! with dynamic name via record
            let tracing_span = tracing::info_span!(
                "job.process",
                otel.name = %span_name,
                otel.kind = "consumer",
                messaging.system = "nats",
                messaging.operation = "process"
            );

            // Set the parent context from NATS headers (links to job.push producer span)
            if let Some(parent_ctx) = parent_context {
                let _ = tracing_span.set_parent(parent_ctx);
            }

            // Execute the inner service within the span
            inner.call(request).instrument(tracing_span).await
        };

        Box::pin(fut)
    }
}

/// A layer that automatically sends periodic Progress acknowledgements to extend `ack_wait`
/// while a job is running. The heartbeat stops when the handler returns or panics.
#[derive(Clone, Debug)]
pub struct ProgressHeartbeatLayer {
    interval: Duration,
}

impl ProgressHeartbeatLayer {
    /// Create a new heartbeat layer with the given interval. The interval must be less
    /// than the consumer `ack_wait`.
    pub fn new(interval: Duration) -> Self {
        Self { interval }
    }
}

impl<S> Layer<S> for ProgressHeartbeatLayer {
    type Service = ProgressHeartbeatService<S>;

    fn layer(&self, service: S) -> Self::Service {
        ProgressHeartbeatService {
            service,
            interval: self.interval,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProgressHeartbeatService<S> {
    service: S,
    interval: Duration,
}

impl<S, Req> Service<Request<Req, NatsContext>> for ProgressHeartbeatService<S>
where
    S: Service<Request<Req, NatsContext>> + Send + Clone + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
    S::Response: Send + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = futures::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: Request<Req, NatsContext>) -> Self::Future {
        let mut inner = self.service.clone();
        let interval = self.interval;

        let fut = async move {
            // Start heartbeat (if this request carries a real NATS message)
            let _guard = request.parts.context.start_progress_heartbeat(interval);
            inner.call(request).await
        };

        Box::pin(fut)
    }
}
