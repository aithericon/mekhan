# 19 · Postgres Node

Demo of the **Postgres executor backend** (`backendType: postgres`).

```
Start{min_id, new_name} → Postgres READ → Postgres WRITE → End
```

- **READ** (`read_widgets`): `SELECT id, name FROM demo_pg.widgets WHERE id >= $1 ORDER BY id`
  in a read-only transaction. `$1` is bound from the whole-placeholder param
  `{{ start.min_id }}` (typed as a number). `projection: [id, name]` is required
  on a read and gives the output `rows` field a column schema.
- **WRITE** (`add_widget`): `INSERT INTO demo_pg.widgets (name) VALUES ($1) RETURNING id, name`
  read-write. `$1` is bound from `{{ start.new_name }}`. `rows_affected` comes
  from the command tag; the `RETURNING` row(s) land in `rows`.

The `End` node maps the read rows + count and the write's `rows_affected` +
`RETURNING` rows onto the instance result.

## Connection

Both steps bind the workspace `postgres` resource **`demo_pg`** via
`ResourceChannel::ConfigOverlay`. The resolved connection
(host/port/database/username/password/sslmode) is overlaid into the resolved
config; the executor-postgres backend builds/caches a sqlx `PgPool` keyed by
connection identity.

The `demo_pg` resource (`demos/resources/demo_pg.json`) points at the **dev
Postgres** the platform already runs:

| field    | value     |
|----------|-----------|
| host     | localhost |
| port     | 15439     | (slot-0 / main checkout — see note below) |
| database | mekhan    |
| username | mekhan    |
| password | mekhan    |
| sslmode  | disable   |

> **Per-worktree port note.** The resource hardcodes `15439`, the historical
> slot-0 dev Postgres port (same convention as `demos/resources/mail.json`
> hardcoding `localhost:1025`). A worktree on a non-zero slot exposes Postgres
> on `20000 + slot*100 + 10`; edit the resource (or the seeded row) to match
> if you run the live demo from a slotted worktree.

## Seeding the fixture table

This demo **never touches mekhan's own application tables.** It reads/writes a
dedicated `demo_pg.widgets` fixture table. Create + seed it once before
publishing/running:

```bash
just dev pg-demo-seed
# = psql postgres://mekhan:mekhan@localhost:15439/mekhan \
#       -f demos/19-postgres-node/seed.sql
```

`seed.sql` is idempotent: it `CREATE SCHEMA IF NOT EXISTS demo_pg`, creates
`demo_pg.widgets(id bigint identity, name text)`, then `TRUNCATE ... RESTART
IDENTITY` and re-inserts three baseline rows (`alpha`, `bravo`, `charlie` with
ids 1–3). Re-run it to reset to the known baseline.

## Tests

`tests/*.json` drive `mekhan test <templateId>` (template id
`00000000-0000-0000-0000-000000000170`). Re-seed first (`just dev pg-demo-seed`)
so the baseline is fresh — note that **each test run performs the WRITE**, so
the table grows across runs; the read assertions use `gte` to stay re-run-safe.

- `reads-and-writes.json` — `min_id=2` reads at least the seeded `bravo`/`charlie`
  rows, inserts `delta`, asserts `rows_affected == 1` and the `RETURNING` row exists.
- `reads-all-seeded.json` — `min_id=1` reads all three seeded rows (`gte 3`),
  inserts `echo`, asserts the write affected one row.
