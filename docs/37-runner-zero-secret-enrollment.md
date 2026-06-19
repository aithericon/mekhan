# 37 — Zero-secret runner enrollment (single-origin broker)

An enrolled lab runner (`executor` daemon) should need **only** an enrollment
token and a URL to come fully online. It should hold no NATS credentials, no
S3/object-store keys, and no Vault token. mekhan brokers each of those three
data planes so the runner's **only** network peer is mekhan (same single-origin
posture the SPA already has).

The runner persists what it learns at enrollment into `identity.json` and a
`runner.token` file, so a restarted bare daemon needs nothing on the command
line beyond what it was first enrolled with.

## The three brokered channels

| Plane | Without broker | Brokered through mekhan |
|-------|----------------|-------------------------|
| **Control / messaging (NATS)** | runner needs a `.creds` file + `EXECUTOR_NATS_URL` | mekhan signs a scoped NATS user JWT from the public key the runner sent at enroll, and returns the public WS connect URL (`nats_url`). The runner assembles its own creds. NATS is never internet-facing. |
| **Storage (artifact bytes)** | runner needs S3 endpoint + access/secret keys | runner GET/PUTs bytes through `{base}/api/storage/blob`, authed only with its own runner bearer. mekhan proxies to the object store. |
| **Secrets (resource secrets)** | runner needs `VAULT_ADDR` (+ unwrap reachability) | the engine still hands the runner a single-use Vault **response-wrapping token** per job; the runner POSTs it to `{base}/api/v1/runners/{id}/secrets/unwrap` and mekhan unwraps it server-side. Vault is never runner-facing. |

`{base}` for the storage + secret channels is `identity.json` `storage_url ??
mekhan_url` — the runner uses the dedicated storage-broker URL when mekhan
advertised one at enroll, else the mekhan base it enrolled against.

## Status

| Phase | Channel | Status |
|-------|---------|--------|
| Phase 1 | NATS-native WebSocket transport + scoped JWT broker | **DONE** (see commit `143accfa`) |
| **Phase 2** | **Storage broker + secret-unwrap broker** | **DONE** (this change) |

### Phase 2 — what shipped

**Storage proxy (mekhan, `service/src/handlers/storage.rs`).** Two unmodeled
binary routes mounted INSIDE `require_auth_middleware` (same category as
`/api/yjs`, `/api/cloud-layer`, `/petri/*` — deliberately NOT OpenAPI-modeled,
because the consumer is the Rust executor's `BrokeredArtifactStore`, not the
generated TS client):

```text
GET  /api/storage/blob?key=<url-encoded key>   -> 200 octet-stream | 404 | 502
PUT  /api/storage/blob?key=<url-encoded key>   (body = octet-stream) -> 204
```

Every key is **workspace-authorized before it touches S3** — a runner may only
GET/PUT artifacts owned by its own tenant. The artifact keyspace is not
uniformly workspace-prefixed, so `authorize_runner_key` is shape-aware and
**fails closed** on any unrecognised shape:

- `ws/{workspace_id}/…`         → embedded ws must equal the runner's.
- `templates/{template_id}/…`   → `workflow_templates.workspace_id` match.
- `instances/{instance_id}/…`   → owning template's `workspace_id` match.
- `artifacts/mekhan-{ws}-…/…`   → ws parsed off the head of the execution_id
  (`mekhan-{workspace_uuid}-{instance_uuid}-{node_suffix}`), no DB hit.

**Secret-unwrap proxy (mekhan, `service/src/handlers/runners.rs`).** A
runner-token authed, self-only route (`subject == runner:{id}`, the same
boundary as heartbeat / nats-creds):

```text
POST /api/v1/runners/{id}/secrets/unwrap
  req  { "wrapping_token": "<hvs.*|s.*>" }
  resp { "secrets": { "KEY": "value", … } }   (HashMap<String,String>)
  401 wrong/foreign token · 502 unwrap failed · 503 no VAULT_ADDR on mekhan
```

mekhan calls `aithericon_secrets::vault_unwrap_secrets(VAULT_ADDR, token)`. The
wrapping token itself authenticates the unwrap, so mekhan needs no Vault
service token for this path — and the runner never learns `VAULT_ADDR`.

**Enrollment hint (mekhan).** `EnrolledRunner` gained `storage_url:
Option<String>` (omitted from JSON when null), populated from
`AppConfig.runner_storage_broker_url` (env `MEKHAN__RUNNER_STORAGE_BROKER_URL`),
mirroring the existing `nats_url` brokering.

### Phase 2 — executor side

- `BrokeredArtifactStore` (`executor-storage/src/brokered.rs`) implements
  `ArtifactStore` over the blob proxy. Key derivation is **byte-identical** to
  the Local/OpenDal stores (`artifacts/{execution_id}/{artifact_id}/{filename}`)
  so a brokered upload is addressable by an in-cluster worker reading the same
  bucket. `delete` is a logged no-op and `list` returns empty (the proxy is
  GET/PUT only — documented, intentional).
- **Daemon store selection** (`executor-service/src/main.rs`,
  `build_artifact_store`), additive precedence:
  1. `config.storage` Some → OpenDal / Local. **Static storage always wins**;
     brokered never overrides it.
  2. else if `runner_broker_base()` + a readable runner token resolve →
     `BrokeredArtifactStore`.
  3. else → `LocalArtifactStore`.
  `runner_broker_base()` = `runner_storage_url ?? mekhan_url`.
- **Secret-unwrap reroute** (`executor-worker/src/staging.rs`,
  `PlanSecretsHook`), per job:
  - `(wrapped_secrets, VAULT_ADDR)` → direct Vault (in-cluster worker; **Vault
    wins** when both present, no mekhan round-trip).
  - `(wrapped_secrets, no VAULT_ADDR, broker)` → brokered unwrap over plain HTTP
    (no `vault` feature needed).
  - else → the configured store.
- **Identity persistence** (`executor-service/src/register.rs` writes,
  `config.rs` reads): `identity.json` gained `mekhan_url` (always the trimmed
  enroll `--url`) and `storage_url` (`Option`, from `EnrolledRunner.storage_url`,
  written only when present). All fields are `#[serde(default)]` /
  `skip_serializing_if`.

A daemon with static `EXECUTOR_STORAGE` / `VAULT_ADDR` behaves exactly as before
— the whole change is additive.

## Why proxy storage and secret-unwrap through mekhan?

The decisive reason is the same as the NATS WebSocket rework: **keep the
runner's blast radius to a single origin and keep the backing services off the
public internet.** A bare runner that held S3 keys or a `VAULT_ADDR` would (a)
need those services reachable from wherever the runner runs, and (b) widen what
a compromised runner leaks. Proxying means:

- The object store and Vault are never runner-facing. Only mekhan reaches them.
- The runner's blob access is **workspace-scoped at the proxy**, not trusted to
  bucket policy.
- Secret unwrap stays single-use (Vault invalidates the wrapping token on the
  first unwrap) and the runner never holds a long-lived Vault credential.
- One front door (`:443` on the mekhan host) carries all three planes — zero new
  load-balancer or ingress wiring per channel.
