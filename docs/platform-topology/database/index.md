# Database & Entities

The Postgres data model behind `mekhan-service` — every base table grouped by
domain, the well-known sentinel IDs, and the foreign-key graph that ties the
domains together. `mekhan-service` is the sole writer of this database; the
engine and executor never touch Postgres (they communicate over
[NATS](/platform-topology/overview.md)).

Schema is owned by `service/migrations/*.sql` (sqlx, embedded at compile time)
and was verified against the live slot-0 dev cluster on 2026-06-18: **68
application tables** (plus sqlx's `_sqlx_migrations`), the `object_kind` enum,
and three reconcile views.

# Domains

* [Well-known IDs & scopes](well-known-ids.md) - sentinel UUIDs, the scope axis, and the `object_kind` enum.
* [Identity & tenancy](identity-and-tenancy.md) - workspaces, members, users, auth sessions, sharing & invites.
* [Templates & authoring](templates-and-authoring.md) - workflow templates, folders, tags, library packs, staging, pages, webhooks, tests.
* [Instances & execution](instances-and-execution.md) - workflow instances and per-step execution records.
* [Causality & projections](causality-and-projections.md) - the event-sourced read models fed from NATS (causality, HPI, rollups).
* [Catalogue & files](catalogue-and-files.md) - data catalogue, file inventory, file servers, snapshots, reconcile views.
* [Assets](assets.md) - typed structured-data assets and their records.
* [Resources & secrets](resources-and-secrets.md) - resource definitions, versioned Vault pointers, ACLs, audit.
* [Fleet & compute](fleet-and-compute.md) - runners, workers, job templates, allocations, models, inference metering.
* [Collaboration](collaboration.md) - Yjs CRDT document and snapshot stores.

# Relationships

* [Entity relationships](relationships.md) - the foreign-key graph and which tables are deliberately FK-free projections.
