//! NomadClient: `SchedulerClient` implementation using Nomad's HTTP API.
//!
//! Handles parameterized job dispatch, cancellation, and status queries.
//! Each client is constructed per-net with routing context (net_id, signal_place)
//! that gets stamped into Nomad job metadata for watcher routing.

use std::collections::HashMap;

use petri_domain::{JobStatus, SchedulerClient, SchedulerError, SubmitRequest, SubmitResult};

use crate::config::NomadConfig;
use petri_scheduler_bridge::RoutingMeta;

use crate::models::{DispatchJobRequest, DispatchJobResponse, Job, JobStopResponse};
use crate::status_mapping;

/// Nomad scheduler client for parameterized job dispatch.
///
/// Constructed per-net with routing context embedded in Nomad job metadata.
/// Supports per-status signal routing: each `JobStatus` variant can target
/// a different place via `signal_routes`, with `fallback_place` used for
/// backward-compatible `petri_place` stamping.
pub struct NomadClient {
    http_client: reqwest::Client,
    config: NomadConfig,
    /// Net ID for this client — stamped into Nomad job metadata.
    net_id: String,
    /// Per-status signal routes: maps a status name (e.g. "running", "completed")
    /// to the place that should receive the signal for that status.
    signal_routes: HashMap<String, String>,
    /// Fallback place stamped as `petri_place` for backward compatibility.
    fallback_place: String,
}

impl NomadClient {
    /// Create a new Nomad client with per-status signal routing.
    ///
    /// # Arguments
    /// * `config` - Nomad connection configuration
    /// * `net_id` - Petri net ID (stamped into job metadata)
    /// * `fallback_place` - Default place stamped as `petri_place` for backward compatibility
    /// * `signal_routes` - Per-status routing map (status name -> place name).
    ///   Each entry produces a `petri_signal_{status}` meta key in dispatched jobs.
    pub fn new(
        config: NomadConfig,
        net_id: impl Into<String>,
        fallback_place: impl Into<String>,
        signal_routes: HashMap<String, String>,
    ) -> Result<Self, SchedulerError> {
        let http_client = config.build_http_client().map_err(|e| {
            SchedulerError::NotConnected(format!("Failed to build HTTP client: {}", e))
        })?;

        Ok(Self {
            http_client,
            config,
            net_id: net_id.into(),
            signal_routes,
            fallback_place: fallback_place.into(),
        })
    }

    /// Convenience constructor that routes all statuses to a single place.
    ///
    /// `signal_place` becomes the `fallback_place` internally — it is stamped
    /// as the `petri_place` meta key on dispatched jobs and the watcher uses it
    /// as the default target for all status signals.
    ///
    /// Equivalent to `NomadClient::new(config, net_id, signal_place, HashMap::new())` —
    /// no per-status routes are configured, so every status signal goes to `signal_place`.
    pub fn new_single_place(
        config: NomadConfig,
        net_id: impl Into<String>,
        signal_place: impl Into<String>,
    ) -> Result<Self, SchedulerError> {
        Self::new(config, net_id, signal_place, HashMap::new())
    }

    /// Build a full URL for a Nomad API endpoint.
    fn url(&self, path: &str) -> String {
        format!(
            "{}/v1/{}?region={}",
            self.config.addr.trim_end_matches('/'),
            path.trim_start_matches('/'),
            self.config.region
        )
    }

    /// Add authentication header if a token is configured.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref token) = self.config.token {
            req.header("X-Nomad-Token", token)
        } else {
            req
        }
    }
}

#[async_trait::async_trait]
impl SchedulerClient for NomadClient {
    async fn submit(&self, request: SubmitRequest) -> Result<SubmitResult, SchedulerError> {
        // Routing metadata goes in Meta (declared in ParameterizedJob.MetaOptional)
        let routing = RoutingMeta {
            net_id: self.net_id.clone(),
            fallback_place: self.fallback_place.clone(),
            signal_routes: self.signal_routes.clone(),
            event_routes: HashMap::new(),
            signal_key: request.signal_key.clone(),
        };
        let meta = routing.to_meta_tags();

        // Token data is NOT sent in the Nomad dispatch payload.
        // The executor pulls the full ExecutionJob from the NATS job queue,
        // so including token_data here would be redundant and hits Nomad's
        // hardcoded 16KB dispatch payload limit for large specs.
        // Only routing metadata (net_id, signal_key, etc.) goes in meta tags.
        let dispatch_req = DispatchJobRequest { payload: None, meta };

        let url = self.url(&format!("job/{}/dispatch", request.job_template_id));

        let resp = self
            .auth(self.http_client.post(&url))
            .json(&dispatch_req)
            .send()
            .await
            .map_err(|e| SchedulerError::SubmissionFailed(format!("HTTP error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(SchedulerError::SubmissionFailed(format!(
                "Nomad dispatch failed ({}): {}",
                status, body
            )));
        }

        let dispatch_resp: DispatchJobResponse = resp
            .json()
            .await
            .map_err(|e| SchedulerError::SubmissionFailed(format!("Invalid response: {}", e)))?;

        tracing::info!(
            dispatched_job_id = %dispatch_resp.dispatched_job_id,
            eval_id = %dispatch_resp.eval_id,
            template = %request.job_template_id,
            signal_key = %request.signal_key,
            execution_id = %request.execution_id,
            net_id = %self.net_id,
            "Nomad job dispatched"
        );

        Ok(SubmitResult {
            scheduler_job_id: dispatch_resp.dispatched_job_id,
        })
    }

    async fn cancel(&self, scheduler_job_id: &str) -> Result<(), SchedulerError> {
        let url = self.url(&format!("job/{}", scheduler_job_id));

        let resp = self
            .auth(self.http_client.delete(&url))
            .send()
            .await
            .map_err(|e| SchedulerError::CancellationFailed(format!("HTTP error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(SchedulerError::CancellationFailed(format!(
                "Nomad stop failed ({}): {}",
                status, body
            )));
        }

        let _stop_resp: JobStopResponse = resp
            .json()
            .await
            .map_err(|e| SchedulerError::CancellationFailed(format!("Invalid response: {}", e)))?;

        tracing::info!(
            scheduler_job_id = %scheduler_job_id,
            "Nomad job cancelled"
        );

        Ok(())
    }

    async fn status(&self, scheduler_job_id: &str) -> Result<JobStatus, SchedulerError> {
        let url = self.url(&format!("job/{}", scheduler_job_id));

        let resp = self
            .auth(self.http_client.get(&url))
            .send()
            .await
            .map_err(|e| SchedulerError::QueryFailed(format!("HTTP error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(SchedulerError::QueryFailed(format!(
                "Nomad job query failed ({}): {}",
                status, body
            )));
        }

        let job: Job = resp
            .json()
            .await
            .map_err(|e| SchedulerError::QueryFailed(format!("Invalid response: {}", e)))?;

        Ok(status_mapping::map_job_status(&job, &self.config.task_name))
    }

    fn name(&self) -> &str {
        "nomad"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_construction() {
        let config = NomadConfig {
            addr: "http://localhost:4646".to_string(),
            region: "global".to_string(),
            ..NomadConfig::default()
        };
        let client = NomadClient::new_single_place(config, "test-net", "inbox").unwrap();

        assert_eq!(
            client.url("job/my-job/dispatch"),
            "http://localhost:4646/v1/job/my-job/dispatch?region=global"
        );
    }

    #[test]
    fn test_url_construction_trailing_slash() {
        let config = NomadConfig {
            addr: "http://localhost:4646/".to_string(),
            region: "us-west-1".to_string(),
            ..NomadConfig::default()
        };
        let client = NomadClient::new_single_place(config, "test-net", "inbox").unwrap();

        assert_eq!(
            client.url("job/my-job"),
            "http://localhost:4646/v1/job/my-job?region=us-west-1"
        );
    }
}
