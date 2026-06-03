# 25 · Prometheus Query

Demo of the **Prometheus/PromQL executor backend** (`backendType: prometheus`).

```
Start{} → Prometheus query → End
```

- **query_up**: runs the instant PromQL query `up` against the `demo_prometheus`
  resource at the current evaluation time (instant query — the latest sample per
  series, no time window). `up` is the per-target scrape-health metric Prometheus
  exposes for every scrape job: `1` when the last scrape of a target succeeded,
  `0` when it failed. It needs **no inputs**, so the Start node carries no fields
  and the initial token is `{}`.

The `End` node maps `result_type`, `sample_count`, and `samples` onto the
instance result.

## Connection

The step binds the workspace `prometheus` resource **`demo_prometheus`** via
`ResourceChannel::ConfigOverlay`. The resolved binding (`base_url`, optional
bearer `token`, optional `X-Scope-OrgID` `org_id`) is overlaid into the resolved
config; the executor-prometheus backend issues an HTTP `GET` to
`{base_url}/api/v1/query`.

The `demo_prometheus` resource (`demos/resources/demo_prometheus.json`) points at
a **local, unauthenticated Prometheus**:

| field    | value                  |
|----------|------------------------|
| base_url | http://localhost:9090  |
| token    | _(absent — unauth)_    |
| org_id   | _(absent — single-tenant)_ |

## No push step required

Unlike the Loki demo (which must push log lines before it can query), this demo
needs **no write step**. Stock `prom/prometheus` **self-scrapes** — its default
config includes a `prometheus` scrape job pointed at itself — so the `up` metric
has at least one series (`up{job="prometheus", instance="localhost:9090"} 1`)
the moment Prometheus is running. The `up` query therefore returns a non-empty
`vector` result immediately.

## Range-query secondary path

This demo runs an **instant** query (`operation: query`). The range-query path
would instead set `operation: query_range` with a lookback window and step, e.g.
`since: 5m`, `step: 15s` — Prometheus then evaluates the query at each step over
the window and the envelope's `result_type` is `matrix` (a time series of
samples) rather than `vector` (one sample per series at the eval time).

## Tests

`tests/*.json` drive `mekhan test <templateId>` (template id
`00000000-0000-0000-0000-000000000250`). No setup is needed beyond a running
local Prometheus that self-scrapes (the default).

- `finds-up-metric.json` — empty start token `{}`; asserts
  `result_type == "vector"`, `sample_count >= 1`, and `samples` exists.
