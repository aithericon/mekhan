# Directory Update Log

## 2026-06-18
* **Initialization**: Created the NATS topology bundle from a full code survey
  (`service/`, `engine/`, `executor/`, `shared/apalis`) cross-checked against a
  live slot-0 dev cluster (`nats stream ls` / `nats consumer ls` / `nats kv ls`).
* **Database & Entities**: Added the `database/` domain covering all 68
  application tables, the `object_kind` enum, three reconcile views, and the
  foreign-key graph. Sourced from `service/migrations/*.sql` and verified live
  against slot-0 (`information_schema` + `pg_*`). Broadened the bundle scope from
  NATS-only to "NATS Topology & Data Model".
