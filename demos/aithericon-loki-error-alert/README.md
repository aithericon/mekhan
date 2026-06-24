# Aithericon Loki Error-Log Alert

Scheduled error-log alert for the Aithericon cluster. Composes the **cron
trigger**, the **Loki/LogQL backend**, an **Agent**, and the **SMTP backend**
into one end-to-end monitoring workflow.

```
Cron(12h) → Start{fire_time} → Loki query_range → Agent(summarize) → Decision(errors?)
                                                                        ├─ yes → SMTP send → End
                                                                        └─ no  → End (no email)
```

1. **cron** — a `cron` Trigger fires every 12 hours (`0 0 */12 * * *`, at
   00:00 and 12:00 UTC). The cron source exposes `fire_time` / `scheduled_time`;
   `payloadMapping` forwards `fire_time` onto the Start so the email can report
   when the check ran. `concurrency: skip` means a still-running scan is never
   double-fired.
2. **query_logs** — Loki `query_range` against the `aithericon_loki` resource.
   Runs `{nomad_task=~".+"} |~ "(?i)(error|fatal|panic|exception)"` over a
   `15m` lookback window, `limit: 40`, newest-first. Returns the fixed Loki
   envelope (`entries`, `entry_count`, `series`, `result_type`, `stats`). The
   `limit` is deliberately small — the whole entry envelope is fed to the Agent,
   and a few hundred entries overflow a typical 32K-token model context.
3. **summarize** — a single-shot Agent (`maxTurns: 1`, no tools) reads
   `{{ query_logs.entries }}` / `{{ query_logs.entry_count }}` and writes a
   concise incident summary into its `response` output. Bound to **Hugging Face
   Inference** (`provider: openai`, `resourceAlias: hf_inference`, model
   `Qwen/Qwen2.5-7B-Instruct`) — the same agent binding as the suessco sibling.
   Usage is billed to the **`Aithericon` HF org** via the `X-HF-Bill-To` header
   (emitted from the resource's `organization` field), so it draws the org's
   credits instead of the token owner's depletable personal allowance.
4. **has_errors** — a Decision node that gates the email on whether the Loki
   query actually returned error lines. The query already filters for
   error/fatal/panic/exception, so the guard `query_logs.entry_count > 0` means
   real errors were found: `> 0` routes to **send_alert**; otherwise it routes
   to a separate **end_noalert** (no email). `query_logs.entry_count` is read
   via a synthesized read-arc even though the Decision sits after the Agent.
5. **send_alert** — SMTP backend. Subject + text + HTML are Tera-rendered
   against `{{ query_logs.entry_count }}`, `{{ start.fire_time }}`, and the
   agent's `{{ summarize.response }}`, then sent through the `mail` resource.
   `retryPolicy.maxRetries` is **0** on purpose: the engine wraps the SMTP
   secret in a single-use Vault token, so a net-level retry would re-use a spent
   token and re-fire the send — an email storm. One attempt = one email.
6. **end** — maps `entry_count`, the send `outcome`, and the `subject`.
   **end_noalert** (the no-errors path) maps just `entry_count` — `send_alert`
   never runs there, so its outputs aren't available to map.

> **Cron format**: the engine's cron parser wants a **6-field** expression
> (leading seconds field), e.g. `0 0 */12 * * *` — *not* the 5-field crontab
> form `0 */12 * * *` (which fails compile with `TriggerCronInvalid`).

## Connections

| Step | Backend | Resource | Where it points |
|------|---------|----------|-----------------|
| query_logs | `loki` | `aithericon_loki` | `http://loki.service.consul:3100` (cluster Loki, Consul-resolved) |
| summarize | `openai` | `hf_inference` | `https://router.huggingface.co` — HF Inference, billed to org `Aithericon` |
| send_alert | `smtp` | `mail` | dev SMTP relay (mailhog `localhost:1025`) — **replace for prod** |

All three resources auto-seed at startup:

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
- **`hf_inference`** (`demos/resources/hf_inference.json`) — the HF Inference
  router. `api_key` is `${HF_API_KEY}` (provide a real token via env at seed
  time — never commit it), and `organization: "Aithericon"` is what makes the
  executor send `X-HF-Bill-To: Aithericon`.
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
mekhan apply demos/aithericon-loki-error-alert/
```

It is also seeded automatically at service startup (`MEKHAN__DEMOS__SEED=true`),
idempotent by templateId `00000000-0000-0000-0000-0000000000a1`.

## Customizing

- **Recipient** — edit `send_alert.config.to` (hard-coded to
  `oncall@aithericon.local`), or promote it to a Start field if you want it
  passed per-run.
- **Schedule / window** — change the cron `schedule` and keep the Loki
  `since` in step (both default to 15 min).
- **Agent model** — bound to HF Inference (`hf_inference`, model
  `Qwen/Qwen2.5-7B-Instruct`). Change `summarize.data.model.model` to any model
  the HF router serves. Billing follows the resource's `organization`
  (`X-HF-Bill-To`).
  To pick a different model, change `summarize.data.model.model` to another
  approved+loaded id.
