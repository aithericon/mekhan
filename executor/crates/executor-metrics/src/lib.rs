pub mod composite;
pub mod config;
#[cfg(feature = "loki")]
pub mod loki;
pub mod memory;
#[cfg(feature = "nats")]
pub mod nats;
pub mod traits;

pub use composite::CompositeMetricSink;
pub use config::{MetricSinkConfig, MetricsConfig};
pub use memory::InMemoryMetricSink;
pub use traits::{MetricError, MetricSink};

#[cfg(feature = "loki")]
pub use loki::LokiMetricSink;
#[cfg(feature = "nats")]
pub use nats::NatsMetricSink;
