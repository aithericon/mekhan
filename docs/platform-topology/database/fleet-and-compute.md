---
type: Database Domain
title: Fleet & Compute
description: The compute fleet — runners, workers, their registration tokens, job templates, allocations, model state/replicas, inference metering, image builds, and the human roster.
tags: [database, fleet, runners, workers, allocations, models, inference]
timestamp: 2026-06-18T00:00:00Z
---

# Fleet & Compute

The machines and capacity that execute work. Runners/workers enroll via
[zero-secret enrollment](/platform-topology/subjects/fleet-presence.md) (mekhan
brokers a scoped NATS identity), report presence over NATS, and pull jobs from
the [apalis job queues](/platform-topology/streams/apalis-job-queues.md). Default
worker/runner/model groups live in the [platform scope](well-known-ids.md).

# Schema — runners & workers

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `runners` | `id`, `workspace_id` → (logical), `name`, `runner_group`, `token_hash`, `nats_public_key`, `capabilities` (jsonb), `status`, `last_seen_at` | An enrolled runner (ROS / device bridge). `nats_public_key` is its brokered NATS identity. |
| `runner_interfaces` | `runner_id` → `runners.id`, `workspace_id`, `catalog` (jsonb), `catalog_version` | Per-runner self-reported ROS topics/services/actions catalog (upserted on discovery). |
| `runner_registration_tokens` | `id`, `workspace_id`, `runner_group`, `token_hash`, `reusable`, `uses`/`max_uses`, `expires_at`, `revoked_at` | Enrollment tokens for runners. |
| `workers` | `id`, `workspace_id`, `name`, `worker_group`, `token_hash`, `nats_public_key`, `backends` (jsonb), `status`, `last_seen_at`, `routing_partition` | An enrolled executor worker; `backends` lists the step backends it serves. |
| `worker_registration_tokens` | `id`, `workspace_id`, `worker_group`, `token_hash`, `reusable`, `uses`/`max_uses`, `expires_at` | Enrollment tokens for workers. |
| `capability_types` | `id`, `workspace_id`, `name`, `fields` (jsonb), `revoked_at` | User-defined capability schemas advertised by fleet members. |
| `roster_members` | `id`, `workspace_id`, `capacity_id`, `member_user_id`, `caps` (jsonb), `concurrency`, `availability` (jsonb), `available` | **Human** capacity: people enrolled against a worker-capacity resource to take human tasks. |

# Schema — jobs & allocations

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `job_templates` | `id`, `workspace_id`, `slug`, `display_name`, `flavor`, `visibility`, `consumer_locked`, `latest_version`, `container_resource_id`, `deleted_at` | A parameterized job spec (the scheduler-facing template, distinct from a [workflow template](templates-and-authoring.md)). |
| `job_template_versions` | (`template_id` → `job_templates.id`, `version`), `common_spec` (jsonb), `escape_hatch` (jsonb), `parameters` (jsonb) | One immutable version of a job template. |
| `allocations` | `id`, `kind`, `net_id`, `instance_id`, `grant_id`, `cluster_resource_id`, `scheduler_flavor`, `alloc_id`, `node`, `status`, `requested_tres`/`allocated_tres` (jsonb), `queue_wait_ms`, `cpu_seconds`/`gpu_seconds`, `peak_rss_bytes`, `last_sequence` | A scheduler allocation (lease) for a net/instance, with accounting (TRES, wait, usage). |

# Schema — models & inference

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `model_states` | (`workspace_id`, `model_id`), `registry_resource_id`, `state`, `base`, `replicas`, `autoscale_mode`, `desired_replicas`, `scale_up`/`scale_down_threshold`, `cooldown_secs`, `residency_zone`, `idle_evict` | Desired/declared state of an LLM model, incl. autoscale policy. |
| `model_replicas` | `id`, `workspace_id`, `model_id`, `desired_count`, `observed_count`, `status`, `residency_zone`, `load_started_at`/`load_finished_at`, `last_load_duration_ms` | Observed replica counts and load timing per model. |
| `inference_request_log` | `request_id`, `tenant_id`, `instance_id`, `step_id`, `model_id`, `replica_id`, `residency_zone`, `slo_tier`, `status`, `prompt`/`completion`/`total_tokens`, `started_at`/`finished_at` | Per-request inference metering (fed from the [`inference.` stream](/platform-topology/streams/inference-metering.md)). |

# Schema — image builds

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `image_materializations` | `id`, `container_resource_id`, `container_version`, `datacenter_resource_id`, `status`, `digest`, `sif_path`, `size_bytes`, `last_error` | Tracks building/materializing a container image (e.g. to a Singularity `.sif`) at a datacenter. |

# Notes

- `runners` / `workers` `last_seen_at` is bridged from NATS presence, not an HTTP
  heartbeat; a server-computed `online` overlay derives from the live presence
  snapshot. See [fleet presence](/platform-topology/subjects/fleet-presence.md).
- Per-row platform membership is opt-in via `?platform=true` on the list
  endpoints (platform-scoped groups vs. workspace groups).

# Citations

[1] `service/migrations/20240132_allocations.sql`,
    `20240133_job_templates.sql`,
    `20240134_runners.sql`, `20240142_workers.sql`,
    `20240135_capability_types.sql`,
    `20240143_runner_interfaces.sql`,
    `20240144_default_worker_group.sql`,
    `20240156_roster_members.sql`,
    `20240139_job_template_container_resource.sql`,
    `20240140_image_materializations.sql`.
[2] `service/migrations/20240145_model_states.sql`,
    `20240146_model_replicas.sql`,
    `20240148_inference_request_log.sql`,
    `20240152_model_states_policy.sql`,
    `20240155_model_idle_evict.sql`,
    `20240159_model_replica_load_timing.sql`,
    `20240188_platform_shared_infra.sql`,
    `20240191_retire_legacy_per_workspace_groups.sql`.
[3] `service/src/{fleet,runners_nats,presence}/`, `service/src/projections/allocations/`.
