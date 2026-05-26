# `deploy/` — mekhan-service deployment

Terraform/OpenTofu layers + Docker context that deploy mekhan-service to the [HetznerCluster](../../HetznerCluster) Nomad cluster.

## Layout

| Path | What |
|---|---|
| [`docker/Dockerfile.ci-build`](docker/Dockerfile.ci-build) | Fat CI builder image — everything the build pool needs. Pushed to `forge.aithericon.eu/milanender/mekhan-ci-builder:latest`. |
| [`docker/Dockerfile.service.prebuilt`](docker/Dockerfile.service.prebuilt) | Tiny Alpine runtime image. Takes the prebuilt static `mekhan-service` binary + the SvelteKit `app/build/` bundle. |
| [`docker/Dockerfile.executor`](docker/Dockerfile.executor) | Comprehensive Debian image for `aithericon-executor-service` (kreuzberg + tesseract + python). Built on demand; not deployed by these layers yet. |
| [`dev/`](dev/) | TF layer that deploys mekhan-service to the **dev** cluster (10.30.x subnet). |
| [`prod/`](prod/) | TF layer that deploys to the **prod** cluster (10.20.x subnet). Currently the only fully-populated environment. |
| [`zitadel/`](zitadel/) | Local-only bootstrap for the docker-compose Zitadel; orthogonal to Nomad deploy. |

## Architecture

```
                Woodpecker CI                            HetznerCluster (Nomad)
                ─────────────                            ──────────────────────
build-frontend         ┐                                  ┌─ Postgres (06b, Patroni HA)
build-backend-service  ┼─► docker buildx ─► forge.        │  postgres.service.consul:5432
                       │                    aithericon.eu/│
                       │                    MilanEnder/   ├─ NATS (04c)
publish-backend-service┘                    mekhan-       │  nats.service.consul:4222
                                            service:$SHA  │
                                              │           │
                  ┌───────────────────────────┘           ├─ Traefik (05_ingress)
deploy-dev ───►   │                                       │  *.aithericon.eu / *.dev.aithericon.eu
deploy-prod ──►   ▼ tofu apply -var=image_tag=$SHA        │
              ┌─────────────────────────────────┐         │
              │ nomad_job "mekhan-service"      │ ───────►│ mekhan-service (canary rolling update)
              │  - constraint: node.class=...   │         │  count=2 prod / 1 dev
              │  - provider="consul" + Traefik  │         │  /api/health → HTTP check
              │  - env vars from TF_VAR_*       │         │
              └─────────────────────────────────┘         └─ Zitadel (06d/e) for auth_mode=bff
                            ▲
                            │
                  state in Hetzner Object Storage
                  (tfstate-aithericon-{dev,prod} bucket,
                   AES-GCM encrypted client-side)
```

The deploy code lives **in this repo** (per web-platform's convention of "the service owns its rollout") rather than in HetznerCluster's `layers/`. Nothing in this folder requires Terragrunt — it's plain OpenTofu pointed at the cluster's Nomad/Consul/Traefik infra.

## One-time setup

These have to happen **once, by hand**, before CI can deploy. They split between the cluster operator (mostly in [HetznerCluster](../../HetznerCluster)) and the mekhan repo owner.

### A. Cluster operator (in HetznerCluster)

1. **Database**. Nothing to do — mekhan's TF in `deploy/dev/postgres.tf` provisions its own role + database against the shared Patroni cluster (layer 06b). The operator just needs to confirm the Patroni superuser password is available in Vault at `secret/postgres/patroni` (already true on every standard cluster deploy) so mekhan's `.envrc` can fetch it once.
2. **Object Storage**. Create two Hetzner Object Storage buckets: `mekhan-artifacts-dev` and `mekhan-artifacts`. Issue an access-key pair scoped to each.
3. **Nomad ACL token**. Create two policies (`mekhan-dev-deploy`, `mekhan-prod-deploy`) with `submit-job` + `read-job` on namespace `default`. Mint tokens, hand them to the mekhan owner.
4. **Hetzner S3 backend bucket**. The repos already use `tfstate-aithericon-{dev,prod}` — just confirm mekhan's S3 key (`mekhan/{dev,prod}/terraform.tfstate`) doesn't collide with anything. Issue an access-key pair the CI runner can use, or reuse the existing operator key.
5. **Traefik route**. Nothing to pre-create — Traefik discovers via Consul Catalog, so mekhan registering itself with the right tags is enough. But DNS for `mekhan.aithericon.eu` / `mekhan.dev.aithericon.eu` must point at the Hetzner Load Balancer fronting the ingress nodes.
6. **NetBird mesh access**. The Woodpecker runner that picks up the `role: deploy` jobs must have NetBird (or Tailscale/WireGuard) connectivity to `10.20.0.10:4646` / `10.30.0.10:4646`. If your build runner is a separate machine from the deploy runner, label them differently.

### B. mekhan repo owner

1. **Woodpecker secrets** — only two:

   | Secret name | Required | Source / value |
   |---|---|---|
   | `vault_token` | **yes** | Vault token with read access to `secret/nomad/bootstrap`, `secret/postgres/patroni`, `secret/docker/registry`, and `secret/services/mekhan/*`. Easiest is the cluster's bootstrap token (`VAULT_TOKEN` in `HetznerCluster/environments/prod/.secrets`). Scope it down later via a `mekhan-deploy` policy. |
   | `mekhan_cli_token` | optional | Zitadel service-user PAT for `50-deploy-workflows.yml`. Only needed if you use the workflow-template GitOps path. |

   Every other secret (Forgejo creds, Nomad token, Patroni password, Hetzner S3 keys, state encryption passphrase) is fetched from Vault at runtime inline in the pipeline `commands:` — see the `export X=$(vault kv get -field=X path)` block in [40-deploy.yml](../.woodpecker/40-deploy.yml). Mirrors web-platform's `before_script` pattern. The CI builder image carries the `vault` CLI.

2. **Vault path `secret/services/mekhan/dev`** — one-time `vault kv put` for the mekhan-specific bits that aren't already in Vault. Run this once from your laptop with VPN up:

   ```bash
   set -a && source HetznerCluster/environments/prod/.secrets && set +a
   export VAULT_ADDR=http://10.20.0.20:8200

   vault kv put secret/services/mekhan/dev \
     hetzner_s3_access_key="$TF_VAR_hetzner_s3_access_key" \
     hetzner_s3_secret_key="$TF_VAR_hetzner_s3_secret_key" \
     state_encryption_passphrase="$TF_VAR_state_encryption_passphrase"
   ```

   That populates the three values mekhan needs that aren't shared cluster infra. Verify:

   ```bash
   vault kv get secret/services/mekhan/dev
   ```

3. **Bootstrap the NATS user** — once, before the first `tofu apply`. Mints the `mekhan-dev-worker` user on the shared NATS broker and publishes its .creds bundle to Vault at `secret/nats/apps/mekhan/dev/worker`, which mekhan-service's Nomad alloc reads at startup. Same VPN + Vault env as step 2:

   ```bash
   export VAULT_ADDR=http://10.20.0.20:8200
   export VAULT_TOKEN=hvs.…   # cluster bootstrap token from .secrets

   ./deploy/dev/scripts/generate-nats-user.sh
   ```

   Re-run any time you want to rotate the user's creds — the helper deletes and recreates the user idempotently. The matching read policy (`mekhan-nats-read`) + two JWT-Nomad roles (`mekhan-service`, `mekhan-executor`) are created by `tofu apply` from `deploy/dev/vault.tf`; the script and the TF are split this way (same as web-platform) so credential rotation never requires touching state.

   Requires `nsc`, `vault`, and `jq` on PATH.

4. **Vault as Resource secret store** — no bootstrap step required. As of `service/src/main.rs:207`, mekhan-service auto-detects Vault via `VAULT_ADDR` + `VAULT_TOKEN` (both injected by Nomad — `VAULT_TOKEN` via the workload-identity exchange, `VAULT_ADDR` from `var.vault_addr` in the jobspec env). On every resource create or new-version, `VaultResourceStore::put_kv` writes the version payload to:

   ```
   secret/data/aithericon/resources/{workspace_id}/{resource_id}/v{n}
   ```

   The engine reads that same path when wrapping `{{secret:...}}` refs into a single-use cubbyhole token before NATS dispatch; the executor calls `vault_unwrap_secrets()` with that token as its own auth (no Vault token on the executor side). All three capabilities are wired by `deploy/dev/vault.tf`:

   | Policy | Granted to | Capability |
   |---|---|---|
   | `mekhan-nats-read` | `mekhan-service` + `mekhan-executor` roles | `read` on `secret/data/nats/apps/mekhan/dev/worker` |
   | `mekhan-resources-rw` | `mekhan-service` role only | `create, read, update, delete` on `secret/data/aithericon/resources/*` |
   | `mekhan-wrap` | `mekhan-service` role only | `update` on `sys/wrapping/wrap` |

   If `VAULT_ADDR` is unset (or `VAULT_TOKEN` injection fails), mekhan falls back to `InMemoryResourceStore` and logs a WARN on boot — see the `resource_store:` line. **Resource secrets WILL NOT SURVIVE A RESTART in that mode**, so the warn is load-bearing for prod.

5. **Local TF init files** — these are gitignored, copy them once per workstation:

   ```bash
   # dev — backend config is inlined in backend.tf, only tfvars needed
   cp deploy/dev/dev.auto.tfvars.example      deploy/dev/dev.auto.tfvars

   # prod still uses partial backend config (different bucket per env)
   cp deploy/prod/backend.hcl.example         deploy/prod/backend.hcl
   cp deploy/prod/prod.auto.tfvars.example    deploy/prod/prod.auto.tfvars
   ```

   The `.example` files are pre-filled with the right Hetzner endpoints — usually only `image_repository` and the resource sizes need tweaking.

6. **CI builder image** — must exist in the registry before `10-lint.yml` / `30-build.yml` / `40-deploy.yml` can pull it:

   ```bash
   docker login forge.aithericon.eu
   just ci::build-ci-builder forge.aithericon.eu/milanender
   ```

7. **Bootstrap TF state** — run once from each layer to create the encrypted state object:

   ```bash
   # On a machine that can reach Hetzner S3 (no VPN needed for this part).
   # Env vars come from .envrc (direnv) — see deploy/dev/.envrc.example.
   cd deploy/dev && tofu init
   cd ../prod   && tofu init -backend-config=backend.hcl
   ```

## Deploy workflow

| Trigger | What happens |
|---|---|
| Push to `dev` branch | `10-lint` → `30-build` (push image tagged `$CI_COMMIT_SHA`) → `40-deploy.deploy-dev` (tofu apply → Nomad canary rollout) → `verify-dev` (`nomad deployment status` until healthy or fail) |
| Push to `main` | `10-lint` → `30-build`. **Stops there.** No automatic deploy. |
| Click "Manual" on a `main` pipeline | Same as push to `main` plus `deploy-prod` + `verify-prod`. |
| Tag push | `50-deploy-workflows` runs — this is **workflow-template GitOps**, not service rollout. Orthogonal. |

## Local deploy (manual, for debugging)

The `just ci::deploy-*` recipes work locally too, so you can dry-run before letting CI do it:

```bash
cd /Users/sumitsah/all_project/aithericon/aithericon_clinic/mekhan

# Make sure your VPN / NetBird tunnel to the cluster is up.

export AWS_ACCESS_KEY_ID=…
export AWS_SECRET_ACCESS_KEY=…
export TF_VAR_state_encryption_passphrase=…
export TF_VAR_nomad_token=…
export TF_VAR_registry_user=…
export TF_VAR_registry_password=…
export TF_VAR_database_url=…
export TF_VAR_s3_access_key=…
export TF_VAR_s3_secret_key=…

just ci::deploy-prod "$(git rev-parse HEAD)"     # uses prod tfvars
just ci::verify-deploy prod
```

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `tofu apply` hangs and times out talking to Nomad | Runner not on the NetBird/WireGuard mesh — can't reach 10.20.0.10:4646. Tag the runner correctly or run from a machine that has the tunnel. |
| Nomad deployment fails health check | `/api/health` not responding — check `nomad alloc logs <alloc-id>`. Common cause: `MEKHAN__DATABASE_URL` wrong, or Postgres firewall doesn't allow the Nomad client subnet. |
| Image pull fails on Nomad client | Registry creds wrong, or the Nomad client host's docker daemon isn't authenticated to `forge.aithericon.eu`. The jobspec passes explicit `auth { username; password }` so this shouldn't happen unless the secret values are wrong. |
| Traefik returns 404 for `mekhan.aithericon.eu` | DNS doesn't point at the LB, **or** mekhan-service didn't register in Consul (check `nomad job status mekhan-service` for healthy allocs). |
| `tofu init` fails with "state decryption failed" | `TF_VAR_state_encryption_passphrase` differs from what the state object was originally written with. Stable passphrase is critical. |

## What's deliberately NOT in this scaffold

- **Full Vault integration for runtime config**. Vault now backs two things: (a) the NATS creds bundle (`secret/data/nats/apps/mekhan/dev/worker`, bootstrap script-driven) and (b) the Resource secret store (`secret/data/aithericon/resources/*`, mekhan-service writes inline on resource CRUD). Policies + JWT roles live in `deploy/dev/vault.tf`. The runtime-config bits that *still* pass through TF variables sourced from Woodpecker secrets — `MEKHAN__DATABASE_URL`, `MEKHAN__S3__*`, registry creds, the introspection client secret — are next. To finish the migration: add a `secret/services/mekhan/runtime` KV + sibling policy in `vault.tf`, seed that path, add `template {}` stanzas to the jobspec, and drop the corresponding `env {}` lines. The CI builder image already has the `vault` CLI baked in.
- **engine + executor Nomad jobs**. The `Dockerfile.executor` is built and ready, but no TF layer deploys it. When you're ready, mirror the `dev/` / `prod/` structure with new layers next to it.
- **Smoke tests post-deploy**. web-platform runs a `ci-smoke-test.yaml` after dev deploys; we don't have one yet.
