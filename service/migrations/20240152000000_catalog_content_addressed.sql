-- Legacy file migration — Phase 1 foundation (docs/32).
--
-- Content-addresses the catalogue, decouples its identity from the job-net
-- composite key, and introduces the file_inventory + legacy staging tables.
-- Everything downstream (crawl/reconcile/migrate) FKs/links to `content_hash`.

-- ---------------------------------------------------------------------------
-- Catalogue: surrogate PK + content_hash logical identity
-- ---------------------------------------------------------------------------

-- Surrogate primary key. gen_random_uuid() is pgcrypto (pg13+), the repo's
-- established convention for UUID defaults.
ALTER TABLE catalogue_entries ADD COLUMN entry_id UUID NOT NULL DEFAULT gen_random_uuid();

-- Logical identity. NULL for job-net artifacts (which the job path does not
-- hash); populated for legacy/by-reference rows.
ALTER TABLE catalogue_entries ADD COLUMN content_hash TEXT;

-- Swap the composite (execution_id, id) PK for the surrogate FIRST — Postgres
-- refuses to drop NOT NULL on a column while it is still part of a primary key.
ALTER TABLE catalogue_entries DROP CONSTRAINT catalogue_entries_pkey;
ALTER TABLE catalogue_entries ADD PRIMARY KEY (entry_id);

-- Legacy logical rows have no execution/job-net provenance. Relax the columns
-- that the job path always set so a content-addressed row can exist on its own.
ALTER TABLE catalogue_entries ALTER COLUMN execution_id DROP NOT NULL;
ALTER TABLE catalogue_entries ALTER COLUMN id DROP NOT NULL;
ALTER TABLE catalogue_entries ALTER COLUMN name DROP NOT NULL;
ALTER TABLE catalogue_entries ALTER COLUMN category DROP NOT NULL;
ALTER TABLE catalogue_entries ALTER COLUMN filename DROP NOT NULL;

-- content_hash is the logical identity. A UNIQUE CONSTRAINT (not a partial
-- index) so it can serve as an FK target. Nullable column ⇒ many NULLs are
-- allowed (job artifacts), but every non-null hash is unique.
ALTER TABLE catalogue_entries ADD CONSTRAINT uq_cat_content_hash UNIQUE (content_hash);

-- Preserve the existing artifact lookup (get_entry filters
-- WHERE execution_id = $1 AND id = $2). Partial-unique so legacy rows with
-- NULL/empty execution_id don't collide.
CREATE UNIQUE INDEX uq_cat_exec_id ON catalogue_entries (execution_id, id)
    WHERE execution_id IS NOT NULL AND execution_id <> '';

-- ---------------------------------------------------------------------------
-- file_inventory: one row per PHYSICAL copy of a file.
-- ---------------------------------------------------------------------------
-- content_hash is a LOGICAL link to catalogue_entries.content_hash (index
-- only, NO hard FK — avoids insert-ordering pain during crawl/reconcile when
-- a physical file is observed before its catalogue row exists).
CREATE TABLE file_inventory (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    content_hash    TEXT,
    file_server_id  TEXT        NOT NULL,
    path            TEXT        NOT NULL,
    status          TEXT        NOT NULL,
    is_canonical    BOOLEAN     NOT NULL DEFAULT false,
    copy_of         UUID        REFERENCES file_inventory(id),
    migration_target TEXT,
    provenance      JSONB       NOT NULL DEFAULT '{}',
    first_seen      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen       TIMESTAMPTZ,
    last_verified   TIMESTAMPTZ,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (file_server_id, path)
);
CREATE INDEX idx_inv_content_hash  ON file_inventory (content_hash);
CREATE INDEX idx_inv_status        ON file_inventory (status);
CREATE INDEX idx_inv_server_status ON file_inventory (file_server_id, status);

-- ---------------------------------------------------------------------------
-- legacy staging — raw, pristine, re-importable. Populated by the Phase-2
-- offline importer. NOT inventory: this is the legacy ArangoDB baseline.
-- ---------------------------------------------------------------------------
CREATE TABLE legacy_file_index (
    legacy_key      TEXT        PRIMARY KEY,
    file_server_id  TEXT,
    path            TEXT,
    hash            TEXT,
    size            BIGINT,
    node_id         TEXT,
    owner_id        TEXT,
    created         TIMESTAMPTZ,
    modified        TIMESTAMPTZ,
    raw             JSONB
);
CREATE INDEX idx_lfi_hash        ON legacy_file_index (hash);
CREATE INDEX idx_lfi_server_path ON legacy_file_index (file_server_id, path);

CREATE TABLE legacy_delete_queue (
    key             TEXT        PRIMARY KEY,
    hash            TEXT,
    size            BIGINT,
    modified        TIMESTAMPTZ
);
CREATE INDEX idx_ldq_hash ON legacy_delete_queue (hash);
