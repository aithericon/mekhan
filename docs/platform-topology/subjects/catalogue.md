---
type: NATS Subject Family
title: Catalogue Subjects
description: The catalogue request/reply protocol for querying, subscribing, and issuing commands against the data catalogue.
tags: [nats, subjects, catalogue, request-reply, subscriptions]
timestamp: 2026-06-18T00:00:00Z
---

# Catalogue Subjects

The data catalogue uses NATS **request/reply**, not persistent streams.
Implemented in `service/src/catalogue/subscriptions.rs` and
`service/src/catalogue/responder.rs`.

# Schema

| Subject | Meaning |
|---------|---------|
| `catalogue.query.*` | query the catalogue (reply subject carries results) |
| `catalogue.subscribe` | create a subscription (reply with `subscription_id`) |
| `catalogue.unsubscribe` | remove a subscription |
| `catalogue.commands.*` | command dispatcher (e.g. register) |

Subscription dedup IDs: `cat-sub:{subscription_id}:{execution_id}:{artifact_id}`.
Subscriptions are persisted in the
[CATALOGUE_SUBSCRIPTIONS KV bucket](/platform-topology/kv/service-kv.md).

# Citations

[1] `service/src/catalogue/subscriptions.rs`, `service/src/catalogue/responder.rs`.
[2] `docs/06-triggers.md`.
