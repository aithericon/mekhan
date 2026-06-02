# 20 · Loki Query

Demo of the **Loki/LogQL executor backend** (`backendType: loki`).

```
Start{app, since} → Loki query_range → End
```

- **query_logs**: runs the LogQL query `{job="varlogs", app="{{ start.app }}"}`
  over the `{{ start.since }}` lookback window (e.g. `1h`), `limit: 1000`,
  `direction: backward`. The whole-placeholder `{{ start.app }}` is bound from
  the Start producer and spliced into the double-quoted matcher string through a
  **LogQL-escaping render** — backslash and double-quote are escaped, so an
  upstream value cannot break out of the matcher (the LogQL analog of a Postgres
  `$1` bind).

The `End` node maps `entries`, `entry_count`, and `result_type` onto the
instance result.

## Connection

The step binds the workspace `loki` resource **`demo_loki`** via
`ResourceChannel::ConfigOverlay`. The resolved binding (`base_url`, optional
bearer `token`, optional `X-Scope-OrgID` `org_id`) is overlaid into the resolved
config; the executor-loki backend issues an HTTP `GET` to
`{base_url}/loki/api/v1/query_range`.

The `demo_loki` resource (`demos/resources/demo_loki.json`) points at a **local,
unauthenticated Loki**:

| field    | value                  |
|----------|------------------------|
| base_url | http://localhost:3100  |
| token    | _(absent — unauth)_    |
| org_id   | _(absent — single-tenant)_ |

## Pushing test logs first

This demo **queries** Loki; it does not write. Before running it live you must
push log lines into Loki under the matched labels, otherwise the query returns
zero entries and the `entry_count gte 1` assertion fails. For example, to make
the `finds-pushed-lines` test pass (`app = "varlogs-demo"`):

```bash
NS=$(date +%s)000000000
curl -s -H 'Content-Type: application/json' \
  http://localhost:3100/loki/api/v1/push \
  -d '{"streams":[{"stream":{"job":"varlogs","app":"varlogs-demo"},
       "values":[["'"$NS"'","hello from demo 20"]]}]}'
```

## Tests

`tests/*.json` drive `mekhan test <templateId>` (template id
`00000000-0000-0000-0000-000000000200`). Push at least one line under
`{job="varlogs", app="varlogs-demo"}` first (see above).

- `finds-pushed-lines.json` — `app="varlogs-demo"`, `since="1h"`; asserts
  `entry_count >= 1`, `entries` exists, and `result_type == "streams"`.
