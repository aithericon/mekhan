# Key-Value Buckets

JetStream KV buckets. Engine buckets are isolated per workspace; service buckets
are global. Live-verified against slot-0 (2026-06-18).

# By owner

* [Engine KV](engine-kv.md) - per-workspace net metadata / activity / timers / idempotency, plus the watcher bucket.
* [Service KV](service-kv.md) - catalogue subscriptions and trigger state.
