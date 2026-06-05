-- Model-pool P5 (docs/29 §7') — the durable, idempotent GDPR processing record
-- for self-hosted inference.
--
-- The router publishes one complete `InferenceRequestLog` record per
-- completed/cancelled/errored request on `inference.metering.{request_id}`
-- (captured by the `INFERENCE_METERING` JetStream stream). The mekhan projector
-- (`service/src/projections/inference_metering.rs`) upserts it here keyed by
-- `request_id` (PRIMARY KEY) so redeliveries are idempotent.
--
-- Field→column mapping note: the record's `tenant` → `tenant_id`, `model` →
-- `model_id`; every other field maps by name.
CREATE TABLE inference_request_log (
    request_id        TEXT PRIMARY KEY,
    tenant_id         TEXT NOT NULL,
    instance_id       TEXT,
    step_id           TEXT,
    model_id          TEXT NOT NULL,
    replica_id        TEXT NOT NULL,
    replica_base_url  TEXT NOT NULL,
    residency_zone    TEXT,
    slo_tier          TEXT,
    status            TEXT NOT NULL,
    prompt_tokens     BIGINT NOT NULL DEFAULT 0,
    completion_tokens BIGINT NOT NULL DEFAULT 0,
    total_tokens      BIGINT NOT NULL DEFAULT 0,
    started_at        TIMESTAMPTZ NOT NULL,
    finished_at       TIMESTAMPTZ NOT NULL,
    recorded_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_inference_request_log_instance_id ON inference_request_log (instance_id);
CREATE INDEX idx_inference_request_log_model_id ON inference_request_log (model_id);
CREATE INDEX idx_inference_request_log_started_at ON inference_request_log (started_at DESC);
