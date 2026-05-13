use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use aithericon_executor_domain::MetricPoint;

use crate::traits::{MetricError, MetricSink};

/// In-memory metric sink that stores points per execution.
///
/// Useful for testing and single-node deployments. Points are stored
/// in insertion order and can be queried after execution completes.
pub struct InMemoryMetricSink {
    max_per_execution: usize,
    data: Arc<RwLock<HashMap<String, Vec<MetricPoint>>>>,
}

impl InMemoryMetricSink {
    pub fn new(max_per_execution: usize) -> Self {
        Self {
            max_per_execution,
            data: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Retrieve all stored points for an execution.
    pub async fn get(&self, execution_id: &str) -> Vec<MetricPoint> {
        self.data
            .read()
            .await
            .get(execution_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Remove stored points for an execution (cleanup).
    pub async fn remove(&self, execution_id: &str) -> Vec<MetricPoint> {
        self.data
            .write()
            .await
            .remove(execution_id)
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl MetricSink for InMemoryMetricSink {
    async fn record(&self, execution_id: &str, points: &[MetricPoint]) -> Result<(), MetricError> {
        let mut data = self.data.write().await;
        let entry = data.entry(execution_id.to_string()).or_default();

        if entry.len() + points.len() > self.max_per_execution {
            return Err(MetricError::BufferFull {
                execution_id: execution_id.to_string(),
                count: entry.len() + points.len(),
                max: self.max_per_execution,
            });
        }

        entry.extend(points.iter().cloned());
        Ok(())
    }

    async fn flush(&self, _execution_id: &str) -> Result<(), MetricError> {
        // In-memory sink has nothing to flush.
        Ok(())
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_point(name: &str, value: f64, step: Option<u64>) -> MetricPoint {
        MetricPoint {
            name: name.into(),
            value,
            step,
            timestamp: Utc::now(),
            metric_type: Default::default(),
            labels: Default::default(),
        }
    }

    #[tokio::test]
    async fn record_and_get() {
        let sink = InMemoryMetricSink::new(1000);
        let points = vec![
            make_point("loss", 0.5, Some(1)),
            make_point("accuracy", 0.8, Some(1)),
        ];

        sink.record("exec-1", &points).await.unwrap();
        sink.record("exec-1", &[make_point("loss", 0.3, Some(2))])
            .await
            .unwrap();

        let stored = sink.get("exec-1").await;
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[0].name, "loss");
        assert_eq!(stored[2].value, 0.3);
    }

    #[tokio::test]
    async fn buffer_limit_enforced() {
        let sink = InMemoryMetricSink::new(2);
        let points = vec![make_point("a", 1.0, None), make_point("b", 2.0, None)];
        sink.record("exec-1", &points).await.unwrap();

        // This should fail — already at capacity
        let result = sink.record("exec-1", &[make_point("c", 3.0, None)]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remove_clears_data() {
        let sink = InMemoryMetricSink::new(1000);
        sink.record("exec-1", &[make_point("loss", 0.5, None)])
            .await
            .unwrap();

        let removed = sink.remove("exec-1").await;
        assert_eq!(removed.len(), 1);

        let after = sink.get("exec-1").await;
        assert!(after.is_empty());
    }

    #[tokio::test]
    async fn separate_executions() {
        let sink = InMemoryMetricSink::new(1000);
        sink.record("exec-1", &[make_point("loss", 0.5, None)])
            .await
            .unwrap();
        sink.record("exec-2", &[make_point("loss", 0.3, None)])
            .await
            .unwrap();

        assert_eq!(sink.get("exec-1").await.len(), 1);
        assert_eq!(sink.get("exec-2").await.len(), 1);
    }

    #[tokio::test]
    async fn flush_is_noop() {
        let sink = InMemoryMetricSink::new(1000);
        sink.flush("exec-1").await.unwrap();
    }
}
