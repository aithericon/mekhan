---
okf_version: "0.1"
---

# NATS Topology & Data Model

The complete messaging and persistence topology for the Aithericon platform: the
NATS / JetStream side (every stream, subject family, durable consumer, and KV
bucket across `mekhan-service`, `core-engine`, the `executor`, and the vendored
`apalis-nats` fork) **and** the Postgres data model behind `mekhan-service`
(every table, grouped by domain, with its foreign-key graph). Verified against a
live slot-0 dev cluster on 2026-06-18.

* [Overview](overview.md) - architecture, design principles, and the three subject roots.

# Streams

* [Streams index](streams/) - all JetStream streams and their subject bindings.

# Subjects

* [Subjects index](subjects/) - subject naming conventions, grouped by domain.

# Consumers

* [Consumers index](consumers/) - durable consumers and their filter subjects.

# Key-Value

* [KV buckets index](kv/) - JetStream KV buckets (per-workspace and global).

# Database & Entities

* [Database index](database/) - the Postgres data model: 68 application tables grouped by domain, well-known IDs, and the foreign-key graph.
