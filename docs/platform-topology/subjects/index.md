# Subjects

Subject naming conventions, grouped by domain. Canonical builders live in
`engine/core-engine/crates/api-types/src/subjects.rs` (engine) and
`service/src/nats/subjects.rs` (service-side filters).

# Domains

* [Petri subjects](petri.md) - events, commands, signals, bridges, and DLQ under `petri.` / `petri-dlq.`.
* [Human-task subjects](human.md) - the `human.{ws}.…` request/result protocol.
* [Executor subjects](executor.md) - `executor.{status,events,datastream}.…`, `inventory.fold.…`, `inference.metering.…`.
* [Fleet & presence subjects](fleet-presence.md) - `runner.` / `worker.` heartbeats, pool claims, and runner/worker JWT scopes.
* [Catalogue subjects](catalogue.md) - `catalogue.{query,subscribe,commands}.…` request/reply protocol.
