pub mod composite;
pub mod config;
pub mod file;
pub mod filter;
#[cfg(feature = "loki")]
pub mod loki;
#[cfg(feature = "nats")]
pub mod nats;
pub mod traits;

pub use composite::CompositeLogSink;
pub use config::{LogSinkConfig, LogsConfig};
pub use file::FileLogSink;
pub use filter::LevelFilterSink;
pub use traits::{LogError, LogSink};

#[cfg(feature = "loki")]
pub use loki::LokiLogSink;
#[cfg(feature = "nats")]
pub use nats::NatsLogSink;
