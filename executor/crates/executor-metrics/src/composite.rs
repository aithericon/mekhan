use std::sync::Arc;

use aithericon_executor_domain::MetricPoint;
use tracing::warn;

use crate::traits::{MetricError, MetricSink};

/// Composite sink that fans out to multiple underlying sinks.
///
/// Records and flushes are forwarded to all child sinks. If any sink
/// fails, the error is logged but does not prevent other sinks from
/// receiving the data. The first error encountered is returned.
pub struct CompositeMetricSink {
    sinks: Vec<Arc<dyn MetricSink>>,
}

impl CompositeMetricSink {
    pub fn new(sinks: Vec<Arc<dyn MetricSink>>) -> Self {
        Self { sinks }
    }
}

#[async_trait::async_trait]
impl MetricSink for CompositeMetricSink {
    async fn record(&self, execution_id: &str, points: &[MetricPoint]) -> Result<(), MetricError> {
        let mut first_err: Option<MetricError> = None;

        for sink in &self.sinks {
            if let Err(e) = sink.record(execution_id, points).await {
                warn!(sink = sink.name(), error = %e, "metric sink record failed");
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn flush(&self, execution_id: &str) -> Result<(), MetricError> {
        let mut first_err: Option<MetricError> = None;

        for sink in &self.sinks {
            if let Err(e) = sink.flush(execution_id).await {
                warn!(sink = sink.name(), error = %e, "metric sink flush failed");
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn name(&self) -> &'static str {
        "composite"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::InMemoryMetricSink;
    use chrono::Utc;

    fn make_point(name: &str, value: f64) -> MetricPoint {
        MetricPoint {
            name: name.into(),
            value,
            step: None,
            timestamp: Utc::now(),
            metric_type: Default::default(),
            labels: Default::default(),
        }
    }

    #[tokio::test]
    async fn forwards_to_all_sinks() {
        let sink_a = Arc::new(InMemoryMetricSink::new(1000));
        let sink_b = Arc::new(InMemoryMetricSink::new(1000));

        let composite = CompositeMetricSink::new(vec![
            sink_a.clone() as Arc<dyn MetricSink>,
            sink_b.clone() as Arc<dyn MetricSink>,
        ]);

        composite
            .record("exec-1", &[make_point("loss", 0.5)])
            .await
            .unwrap();

        assert_eq!(sink_a.get("exec-1").await.len(), 1);
        assert_eq!(sink_b.get("exec-1").await.len(), 1);
    }

    #[tokio::test]
    async fn continues_on_error() {
        // sink_a has a limit of 1, sink_b has plenty of room
        let sink_a = Arc::new(InMemoryMetricSink::new(1));
        let sink_b = Arc::new(InMemoryMetricSink::new(1000));

        let composite = CompositeMetricSink::new(vec![
            sink_a.clone() as Arc<dyn MetricSink>,
            sink_b.clone() as Arc<dyn MetricSink>,
        ]);

        // First record succeeds on both
        composite
            .record("exec-1", &[make_point("loss", 0.5)])
            .await
            .unwrap();

        // Second record fails on sink_a (buffer full) but succeeds on sink_b
        let result = composite.record("exec-1", &[make_point("loss", 0.3)]).await;
        assert!(result.is_err());

        // sink_b still got the data
        assert_eq!(sink_b.get("exec-1").await.len(), 2);
    }
}
