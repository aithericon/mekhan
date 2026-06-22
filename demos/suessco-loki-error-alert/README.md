# Suessco Loki Error-Log Alert

Scheduled error-log alert for the **Suessco** prod cluster, driven by a
**cross-cluster worker**. Same shape as the `loki-error-alert` demo, but the
Loki query runs on an executor that lives *inside the Suessco cluster* and
enrolls into this mekhan's `suessco` worker pool — so it reaches Suessco's
in-cluster Loki locally while the control plane stays on aithericon.

```
Cron(12h) → Start{fire_time} → Loki query_range → Agent(summarize) → Decision(errors?)
            [group: suessco ── runs on the Suessco worker]              ├─ yes → SMTP send → End
                                                                        └─ no  → End (no email)
```

1. **cron** — a `cron` Trigger fires every 12 hours (`0 0 */12 * * *`, at
   00:00 and 12:00 UTC). `payloadMapping` forwards `fire_time` onto the Start so
   the email can report when the check ran. `concurrency: skip` means a
   still-running scan is never double-fired.
2. **query_logs** — Loki `query_range` against the `suessco_loki` resource.
   Runs `{nomad_task=~".+"} | logfmt | level=~"(?i)error|fatal|critical|panic"`
   over a `15m` lookback, `limit: 8`, newest-first. This filters on the **parsed
   logfmt `level` field** — only genuine error-level lines — rather than a
   substring grep for the word "error" (which pulls in INFO lines that merely
   mention it, bloating the token count). **`deploymentModel.group: "suessco"`**
   pins this step to the executor enrolled in mekhan's `suessco` worker pool —
   the one running inside the Suessco cluster — so `loki.service.consul:3100`
   resolves to *Suessco's* Loki, not aithericon's. The `limit` is deliberately
   small — the whole entry envelope feeds the Agent, and too many (or too large)
   entries overflow a 32K-token model context.
3. **summarize** — a single-shot Agent (`maxTurns: 1`, no tools) reads
   `{{ query_logs.entries }}` / `{{ query_logs.entry_count }}` and writes a
   concise incident summary into its `response` output. Bound to **Hugging Face
   Inference** (`provider: openai`, `resourceAlias: hf_inference`, model
   `Qwen/Qwen2.5-7B-Instruct`). Usage is billed to the **`Aithericon` HF org**
   via the `X-HF-Bill-To` header — emitted from the resource's `organization`
   field — so it draws the org's credits instead of the token owner's depletable
   personal allowance. The agent step has no `group`, so it runs on the default
   (aithericon) executor.
4. **has_errors** — a Decision node that gates the email on whether the Loki
   query returned error lines. The guard `query_logs.entry_count > 0` means real
   errors were found: `> 0` routes to **send_alert**; otherwise it routes to a
   separate **end_noalert** (no email). `query_logs.entry_count` is read via a
   synthesized read-arc even though the Decision sits after the Agent.
5. **send_alert** — SMTP backend. Subject + text + HTML are Tera-rendered
   against `{{ query_logs.entry_count }}`, `{{ start.fire_time }}`, and the
   agent's `{{ summarize.response }}`, then sent through the `mail` resource to
   `sah@aithericon.eu`. Its `retryPolicy.maxRetries` is **0** on purpose: the
   engine wraps the SMTP secret in a single-use Vault token, so a net-level
   retry would re-use a spent token (`wrapping token already used`) and, while
   re-staging, fire the send again — an email storm. One attempt = one fresh
   token = one email. (NATS at-least-once can still cause a rare duplicate;
   exactly-once needs SMTP-side idempotency in the executor.)
6. **end** — maps `entry_count`, the send `outcome`, and the `subject`.
   **end_noalert** (the no-errors path) maps just `entry_count`.

> **Cron format**: the engine's cron parser wants a **6-field** expression
> (leading seconds field), e.g. `0 0 */12 * * *` — *not* the 5-field crontab
> form `0 */12 * * *` (which fails compile with `TriggerCronInvalid`).

## Connections

| Step | Backend | Resource | Where it points |
|------|---------|----------|-----------------|
| query_logs | `loki` | `suessco_loki` | `http://loki.service.consul:3100` resolved **inside the Suessco cluster** (via `group: suessco`) |
| summarize | `openai` | `hf_inference` | `https://router.huggingface.co` — HF Inference, billed to org `Aithericon` |
| send_alert | `smtp` | `mail` | SMTP relay — **replace for prod** |

All three resources auto-seed at startup:

- **`suessco_loki`** (`demos/resources/suessco_loki.json`) — Suessco's cluster
  Loki. `base_url` is `${SUESSCO_LOKI_URL:-http://loki.service.consul:3100}`; the
  Consul address only resolves **from inside the Suessco cluster** (the enrolled
  `suessco`-group worker). On a laptop, override `SUESSCO_LOKI_URL`.
- **`hf_inference`** (`demos/resources/hf_inference.json`) — the HF Inference
  router. `api_key` is `${HF_API_KEY}` (provide a real token via env at seed
  time — never commit it), and `organization: "Aithericon"` is what makes the
  executor send `X-HF-Bill-To: Aithericon`.
- **`mail`** (`demos/resources/mail.json`) — the demo SMTP relay; swap for a
  real relay in prod.

## Cross-cluster routing — the load-bearing piece

This demo only works end-to-end when an executor is enrolled into mekhan's
`suessco` worker pool from *inside* the Suessco cluster (see
`suessco/hetzner-cloud-cluster/layers/09_mekhan_executor`). That executor:
- dials OUT to aithericon for control (`mekhan.aithericon.eu`,
  `wss://nats.aithericon.eu`, S3), and
- reaches Suessco's services (Loki) locally.

Steps whose `deploymentModel.group = "suessco"` are dispatched only to it.
The agent step (no group) stays on the aithericon side.

## Apply from the CLI

This template has **no HumanTask sidecars**, so it applies directly:

```bash
mekhan apply demos/suessco-loki-error-alert/
```

It is also seeded automatically at service startup (`MEKHAN__DEMOS__SEED=true`),
idempotent by templateId `00000000-0000-0000-0000-0000000000a2`.

## Customizing

- **Recipient** — edit `send_alert.config.to` (hard-coded to
  `sah@aithericon.eu`), or promote it to a Start field for per-run targeting.
- **Schedule / window** — change the cron `schedule` and keep the Loki `since`
  in step (both default to ~15 min / 12 h).
- **Agent model** — bound to HF Inference (`hf_inference`, model
  `Qwen/Qwen2.5-7B-Instruct`). Change `summarize.data.model.model` to any model
  the HF router serves. Billing follows the resource's `organization`.
- **Which cluster** — point this at a different downstream cluster by swapping
  `suessco_loki` + the `group` value for that cluster's pool + Loki resource.
