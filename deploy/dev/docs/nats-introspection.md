# NATS introspection runbook (debugging runners/jobs on the shared broker)

The dev and prod **mekhan** accounts share one NATS cluster behind
`wss://nats.aithericon.eu` (decentralized JWT auth: operator → account → user).
Each env is a separate **account**, so each has its own JetStream + subject
namespace on the one broker. When a runner is "online but jobs don't run", the
questions you need answered live in the **system account's** monitoring
endpoints — not in app logs.

> TL;DR: from `deploy/dev` (so direnv has loaded `VAULT_*`):
> ```bash
> ./scripts/nats-debug.sh setup      # once: materialize creds + nats context
> ./scripts/nats-debug.sh connz dev  # connections + subs for the dev account
> ./scripts/nats-debug.sh jsz   dev  # streams + consumers (pending/waiting/ack)
> ```

## The identity you need

`$SYS.REQ.*` is only answerable by a **system-account** user. The provisioned,
read-only one is the **resolver user**, published to Vault by
`environments/04b_nats_config`:

| What | Vault path | Field |
|---|---|---|
| Resolver (system) user creds | `secret/nats/system/resolver` | `.data.data.creds` |
| — older fallback location | `secret/nats/cluster` | `.data.data.system_resolver_user.creds` |
| Per-env app account JWT | `secret/nats/apps/mekhan/<env>/account` | `.data.data.jwt` |

The **account public key** (needed to scope JSZ/CONNZ to one env) is the `sub`
claim of that account JWT — `nats-debug.sh` decodes it for you, so nothing is
hardcoded. `mekhan-dev` is account `AB5QAAWXZXQQKZONS3AYCHJLM3VWVELTWXVHXMZIX2Z7AN5HEJZFTMID`.

> ⚠️ The system account has **JetStream disabled for itself** (`nats stream ls`
> as the resolver user → error 10039). That's expected — it can still
> *introspect* every other account via `$SYS.REQ`. Don't "fix" it.
>
> ⚠️ Do **not** mint a new privileged user from the operator/account
> `signing_seed` to debug — it's unnecessary (the resolver user already
> answers `$SYS.REQ`) and the sandbox classifier blocks it. Use the
> pre-provisioned creds.

## The requests

Single-replica (R1) JetStream assets live on exactly **one** of the cluster's
servers, so a `PING` fans out and only the owning server returns detail —
broadcast to a few replies and merge (`--replies 3` for the 3-server cluster).

```bash
# JetStream detail for an account (streams, consumers, pending/waiting/ack):
nats --context aithericon-sys req '$SYS.REQ.SERVER.PING.JSZ' \
  '{"account":"<ACCOUNT_PUBKEY>","streams":true,"consumer":true,"config":true}' \
  --replies 3 --raw | jq '.data.account_details[].stream_detail[]?'

# Connections for an account (with per-connection subscription list):
nats --context aithericon-sys req '$SYS.REQ.SERVER.PING.CONNZ' \
  '{"acc":"<ACCOUNT_PUBKEY>","subscriptions":true}' \
  --replies 3 --raw | jq '.data.connections[]'
```

## Reading the answer (what the fields mean for runner debugging)

- **A runner identity shows up as a CONNZ connection with a `fileserve.<runner_id>.read`
  subscription** (and the partitioned-pool consumer pulls). One healthy runner =
  **one** connection with live `in_msgs`/`out_msgs`/`rtt`. A NATS connection with
  live traffic **must** have a live process behind it — Traefik can't keep a dead
  one breathing.
- **Two connections for the same `runner_id`** (or a consumer with `num_waiting`
  = 2) = a **duplicate/orphaned runner** sharing the durable pull consumer. That's
  the "jobs accepted but never run" failure: JetStream round-robins pulls, ~half
  land on the dead twin and get acked into the void (work-queue retention →
  msg deleted → live runner never sees them, stream `msgs=0`, no deserialize
  log). Fix = kill the orphan; CONNZ drops 2→1, `num_waiting` 2→1, jobs flow.
- **Stream `msgs` climbing with `num_pending` high and `num_waiting` 0** = nothing
  is pulling (no live consumer bound) — the opposite problem.

## Cross-checks

- **Logs:** Loki at `http://loki.service.consul.aithericon:3100`. Labels:
  `nomad_task` ∈ {`engine`, `executor`, `service`, `nats`, …}. e.g.
  `{nomad_task="engine"} |= "<instance_id>"` to see what the engine published.
- **Nomad:** `NOMAD_ADDR=https://10.20.0.10:4646`. Jobs: `mekhan-service-dev`,
  `executor-dev`; the engine runs as `nomad_task=engine`.
- Both are wired by `deploy/dev/.envrc` on the NetBird mesh.

## Open improvement (#3): a least-privilege `nats-debug` user

Today `nats-debug.sh` uses the **resolver** user, which is broader than
introspection needs. The cleaner identity is a dedicated system-account user
scoped to `$SYS.REQ.SERVER.PING.>` + `$SYS.REQ.ACCOUNT.>` only — no JetStream,
no app-subject publish — provisioned once and stored at
`secret/nats/system/debug`. Provisioning needs the operator key (run with the
operator/system material from Vault, e.g. via the `nsc` workspace that
`scripts/generate-lab-user.sh` already seeds):

```bash
# In an nsc workspace seeded with the operator + system account (see
# generate-lab-user.sh for the Vault material + `nsc add operator/import account`):
nsc add user --account <SYSTEM_ACCOUNT_NAME> --name nats-debug \
  --allow-sub '_INBOX.>' \
  --allow-pub '$SYS.REQ.SERVER.PING.>' \
  --allow-pub '$SYS.REQ.ACCOUNT.>'
nsc push -a <SYSTEM_ACCOUNT_NAME>                      # publish to the resolvers
nsc generate creds --account <SYSTEM_ACCOUNT_NAME> --name nats-debug \
  | vault kv put secret/nats/system/debug creds=-      # stored read-only identity
```

Then point the script at it: `RESOLVER_VAULT_PATH=secret/nats/system/debug
./scripts/nats-debug.sh setup` (or flip the script default once it's published).
