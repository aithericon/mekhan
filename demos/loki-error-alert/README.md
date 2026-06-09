# Loki Error-Log Alert

Scheduled error-log alert for the Aithericon cluster. Composes the **cron
trigger**, the **Loki/LogQL backend**, an **Agent**, and the **SMTP backend**
into one end-to-end monitoring workflow.

```
Cron(*/15m) → Start{fire_time} → Loki query_range → Agent(summarize) → SMTP send → End
```

1. **cron** — a `cron` Trigger fires every 15 minutes (`0 */15 * * * *`, UTC).
   The cron source exposes `fire_time` / `scheduled_time`; `payloadMapping`
   forwards `fire_time` onto the Start so the email can report when the check
   ran. `concurrency: skip` means a still-running scan is never double-fired.
2. **query_logs** — Loki `query_range` against the `aithericon_loki` resource.
   Runs `{nomad_task=~".+"} |~ "(?i)(error|fatal|panic|exception)"` over a
   `15m` lookback window, `limit: 40`, newest-first. Returns the fixed Loki
   envelope (`entries`, `entry_count`, `series`, `result_type`, `stats`). The
   `limit` is deliberately small — the whole entry envelope is fed to the Agent,
   and a few hundred entries overflow a typical 32K-token model context.
3. **summarize** — a single-shot Agent (`maxTurns: 1`, no tools) reads
   `{{ query_logs.entries }}` / `{{ query_logs.entry_count }}` and writes a
   concise incident summary into its `response` output. Bound to the in-cluster
   **inference router** (`provider: internal`, `resourceAlias:
   internal_pool_router`, model `qwen3.5:9b`) — the GDPR-safe internal-LLM
   pattern from demo 37; inference never leaves the router and the compiler
   emits the OpenAI-compatible wire shape.
4. **send_alert** — SMTP backend. Subject + text + HTML are Tera-rendered
   against `{{ query_logs.entry_count }}`, `{{ start.fire_time }}`, and the
   agent's `{{ summarize.response }}`, then sent through the `mail` resource.
5. **end** — maps `entry_count`, the send `outcome`, and the `subject`.

> **Cron format**: the engine's cron parser wants a **6-field** expression
> (leading seconds field), e.g. `0 */15 * * * *` — *not* the 5-field crontab
> form `*/15 * * * *` (which fails compile with `TriggerCronInvalid`).

## Connections

| Step | Backend | Resource | Where it points |
|------|---------|----------|-----------------|
| query_logs | `loki` | `aithericon_loki` | `http://loki.service.consul:3100` (cluster Loki, Consul-resolved) |
| summarize | `internal_llm` | `internal_pool_router` | the in-cluster inference router (`${MEKHAN_ROUTER_URL:-http://127.0.0.1:13200}`) |
| send_alert | `smtp` | `mail` | dev SMTP relay (mailhog `localhost:1025`) — **replace for prod** |

Both resources auto-seed at startup:

- **`aithericon_loki`** (`demos/resources/aithericon_loki.json`) — the cluster
  Loki. The `base_url` is `${AITHERICON_LOKI_URL:-http://loki.service.consul:3100}`,
  so it resolves to the in-cluster Consul address by default and can be
  repointed for local testing:

  ```bash
  AITHERICON_LOKI_URL=http://localhost:3100 just dev   # or set it in your env
  ```

  The Consul address only resolves **from inside the cluster** (an executor on a
  Nomad node). On a laptop, either override `AITHERICON_LOKI_URL` or run the
  demo against a local Loki.
- **`mail`** (`demos/resources/mail.json`) — the demo SMTP relay. Pair with
  `just dev mailhog-up` to capture the alert at http://localhost:8025 without a
  real mail server.

The cluster's Alloy log pipeline labels Nomad alloc logs with `nomad_alloc_id`,
`nomad_task`, `stream`, `node`, `node_class` — that's why the matcher keys on
`nomad_task` and the line filter (rather than a label) selects error lines.

## Apply from the CLI

This template has **no HumanTask sidecars**, so it applies directly (unlike the
sidecar-carrying demos):

```bash
mekhan apply demos/loki-error-alert/
```

It is also seeded automatically at service startup (`MEKHAN__DEMOS__SEED=true`),
idempotent by templateId `00000000-0000-0000-0000-0000000000a1`.

## Customizing

- **Recipient** — edit `send_alert.config.to` (hard-coded to
  `oncall@aithericon.local`), or promote it to a Start field if you want it
  passed per-run.
- **Schedule / window** — change the cron `schedule` and keep the Loki
  `since` in step (both default to 15 min).
- **Agent model** — bound to the internal inference router (`internal_pool_router`,
  model `qwen3.5:9b`). The router endpoint comes from `MEKHAN_ROUTER_URL` on the
  service/executor env; the model must be in `internal_pool_registry` and loaded.
  To pick a different model, change `summarize.data.model.model` to another
  approved+loaded id.
