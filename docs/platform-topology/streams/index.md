# Streams

All JetStream streams, grouped by owner. Subject bindings and retention are
live-verified (slot-0, 2026-06-18) except where marked lazily-created.

# Engine-owned

* [PETRI_GLOBAL](petri-global.md) - canonical engine event stream, binds `petri.>`.
* [PETRI_DLQ](petri-dlq.md) - dead-letter queue for unprocessable petri messages.
* [Human-task streams](human-task-streams.md) - `HUMAN_REQUESTS` / `_CANCEL` / `_COMPLETED` / `_CANCELLED` / `_FAILED`.

# Service-owned (mekhan)

* [Executor streams](executor-streams.md) - `EXECUTOR_STATUS` / `_EVENTS` / `_DATASTREAM`.
* [INVENTORY_FOLD](inventory-fold.md) - file-crawl batch registration.
* [INFERENCE_METERING](inference-metering.md) - model-pool inference audit ledger.
* [MEKHAN_SILENT_DROPS](mekhan-silent-drops.md) - mekhan's own dead-letter stream.

# Job dispatch (apalis-nats)

* [Apalis job queues](apalis-job-queues.md) - `runner-jobs_{high,medium,low,dlq}`.
