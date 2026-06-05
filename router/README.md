# inference-router

The OpenAI-compatible **inference router** — the data plane of the self-hosted
model pool. See `docs/11-inference-router.md` (spec) and
`docs/29-model-pool-impl-plan.md` (Router-MVP).

A standalone deployable (umbrella workspace member, peer to `mekhan-service`):
it stays off mekhan's session-cookie middleware, scales independently, and
proxies inference over conventional OpenAI HTTP — **inference never crosses the
engine Petri net**.

## What the MVP does

- `POST /v1/chat/completions` — OpenAI-compatible, **buffered** (`stream:false`)
  and **SSE passthrough** (`stream:true`, modeled on mekhan's `/petri` proxy).
- Routes to a live replica that serves the requested `model`. The optional
  `X-Residency-Zone` request header is a **hard placement filter that fails
  closed** (GDPR): an unsatisfiable zone returns `422` and never crosses to
  another zone or to an external provider. There is **no automatic external
  offload** (doc 28 §7/§11 supersedes doc 11 §2.7/§5.10).
- Per-replica **admission**: a semaphore sized to each replica's concurrency
  `C` (vLLM `--max-num-seqs`). Saturation → `429` + `Retry-After`. The permit is
  held for the whole response (full SSE stream) and released on completion,
  client disconnect, or cancellation. The router is the **only** concurrency
  authority for inference — vLLM's continuous batcher does the real scheduling.
- **Cancellation**: `inference.cancel.{request_id}` (core NATS) or HTTP
  disconnect; publishes `inference.cancelled.{request_id}`. The request id is
  echoed in the `X-Inference-Request-Id` response header.
- **Metering**: one record per terminal on `inference.metering.{request_id}`
  (the doc-11-canonical subject the P5 Postgres projector subscribes to).
- `GET /v1/models`, `GET /healthz`, `GET /metrics` (Prometheus, the autoscale
  signal source), `GET /openapi.json` (the router's own contract — **not** part
  of mekhan's `ci::openapi-drift` gate).

### MVP cuts (deferred)

- **Static replica inventory** from config / `ROUTER_REPLICAS`. The live poll of
  mekhan's `GET /api/v1/capacities` + fleet snapshot + runner interface catalog
  is the soft-dep upgrade deferred to doc 11 P2 (seam in `inventory.rs`).
- **dev-noop auth** (fixed tenant). Real Bearer/JWT verification is a deferred
  seam isolated in `auth.rs`.
- **Metering is an ephemeral NATS event.** Durable persistence
  (`inference_request_log`) + executor-side `X-Instance-Id`/`X-Step-Id`
  injection (attribution) are P5.
- `/v1/embeddings` is a fast-follow on the **same** routing/admission/metering
  machinery (not a new protocol) — not yet implemented.

## Config

Defaults → optional TOML file (`ROUTER_CONFIG`) → `ROUTER_*` env. Replicas come
from `ROUTER_REPLICAS` (a JSON array) when set.

| Env | Meaning | Default |
|---|---|---|
| `ROUTER_BIND_ADDR` | bind `host:port` | `0.0.0.0:13200` |
| `ROUTER_AUTH_MODE` | `dev_noop` \| `bearer` | `dev_noop` |
| `ROUTER_DEFAULT_TENANT` | tenant attributed to requests | `dev` |
| `ROUTER_NATS_URL` | NATS for cancel + metering (optional) | — |
| `ROUTER_MEKHAN_URL` | mekhan base url for the deferred inventory poll | — |
| `ROUTER_REPLICAS` | JSON array of replicas (see below) | `[]` |

Replica shape: `{ base_url, model_ids: [..], residency_zone?, concurrency_c?, api_key? }`.

```bash
ROUTER_REPLICAS='[{"base_url":"http://localhost:11434","model_ids":["llama3.2"],"residency_zone":"eu-west","concurrency_c":2}]' \
  cargo run -p inference-router
```

## Run / test

```bash
cargo run -p inference-router          # serve (from the umbrella root → ./target/)
cargo test -p inference-router         # unit tests (routing/admission/auth/metering/usage/metrics)
```
