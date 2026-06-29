# Resource GitOps — managing resources from CI/CD

Workflows bind **resources** at publish time (databases, LLM providers, SMTP
relays, object stores, runner pools, …). For a GitOps pipeline you want the same
treatment templates already get with `mekhan apply`: declare resources in the
repo, and on every push reconcile them against the server — creating what's
missing, updating what changed, and leaving everything else untouched.

`mekhan resource apply` is that reconcile step. It is **path-keyed** and
**hash-idempotent**: re-running an unchanged manifest writes nothing.

## The end-to-end CI flow

```sh
export MEKHAN_CLI_SERVER=https://mekhan.example.com
export MEKHAN_CLI_TOKEN=uat_…            # a mekhan-native PAT (uat_ token)

# 1. Pull secrets from your secret store into the environment.
export PG_PASSWORD=$(vault kv get -field=password secret/app/db)
export OPENAI_API_KEY=$(vault kv get -field=api_key secret/app/openai)

# 2. Reconcile resources (idempotent — safe on every run).
mekhan resource apply resources/

# 3. Apply the workflow templates that bind them.
mekhan apply workflows/crawl/
mekhan apply workflows/report/
```

Secrets **never live in the repo**. Manifests hold `${VAR}` placeholders; the
CLI interpolates them from its own environment immediately before sending. CI is
the only place the plaintext credential exists, and only in process memory.

## Manifest format

One JSON file per resource, in a directory you point `apply` at. The shape is
the resource create request — the exact same shape the server's demo seeder
reads (`demos/resources/*.json`):

```json
{
  "path": "app_pg",
  "resource_type": "postgres",
  "display_name": "Application Postgres",
  "config": {
    "host": "db.internal",
    "port": 5432,
    "database": "app",
    "username": "app_rw",
    "password": "${PG_PASSWORD}",
    "sslmode": "require"
  }
}
```

- `path` — the snake_case identifier workflows reference as `app_pg.host`. It is
  the upsert key (together with the scope).
- `resource_type` — one of the registered types (`mekhan resource` rejects
  unknown types; see `GET /api/v1/resources/types`). **Immutable**: applying a
  different type to an existing path is a `409` — delete and re-apply to change
  it.
- `config` — public + secret fields together; the server splits them by the
  type descriptor and writes the secret half to Vault.
- `${VAR}` / `${VAR:-default}` anywhere in the file is interpolated from the
  environment. An unset var with no default collapses to empty.
- Optional: `workspace_id`, `scope_kind` (`workspace` | `folder` | `template` |
  `platform`), `scope_id`, `restricted`.

## What `apply` does

For each manifest it resolves the existing resource by `(scope, path)` and
decides by a SHA-256 over the canonical config (public ∪ secret):

| State | Action | Result |
|-------|--------|--------|
| path absent | `created` | resource + v1 written |
| present, config hashes equal | `unchanged` | nothing written |
| present, config differs | `updated` | new version written |
| present, different type | — | `409` (delete to change type) |

A rotated **secret** counts as a change even though secrets never round-trip on
a read — the hash is computed over the submitted config, including the secret
half. Key order doesn't matter (the hash is canonical).

```
$ mekhan resource apply resources/
created    app_pg                   postgres       v1
unchanged  app_openai               openai         v3
updated    mail                     smtp           v2

3 applied — 1 created, 1 updated, 1 unchanged
```

## Ad-hoc / direct use

No file required — build a resource inline (secrets still come from the
environment, never the shell literal):

```sh
mekhan resource apply \
  --path app_pg --type postgres \
  --set host=db.internal --set port=5432 --set database=app \
  --set username=app_rw --set 'password=${PG_PASSWORD}' --set sslmode=require
```

`--set key=value` values are env-interpolated, then parsed as JSON (so `port=5432`
is a number) with a bare-string fallback. You can also pipe a manifest on stdin:
`… | mekhan resource apply -`.

## Inspecting & removing

```sh
mekhan resource list                 # all resources (optionally --type postgres)
mekhan resource get app_pg           # detail by path or UUID (secrets redacted)
mekhan resource delete app_pg        # soft-delete by path or UUID
```

## Notes

- Auth is the standard CLI PAT (`MEKHAN_CLI_TOKEN`, a `uat_` token). The apply
  needs `Editor` on the target scope (create) / the resource (update); a
  `platform`-scoped resource needs platform admin.
- `apply` reconciles resources only — it does not touch workflow templates. Run
  it before `mekhan apply` so a template that binds a resource can resolve it at
  publish time.
- Idempotency is server-side (the stored config hash), so it holds regardless of
  which client applies — CLI, CI, or a hand-rolled `POST /api/v1/resources/apply`.
