-- Registered data types (Cut B of the catalogue query interface): a user
-- promotes a schema-fingerprint digest (fmeta FNV-1a 64, hex16 — already
-- extracted as the `meta.schema` virtual field and expression-indexed by
-- `idx_cat_fmeta_schema` from 20240168) to a named, described data type.
--
-- NOTE: `columns` stores the HUMANIZED display projection
-- (`[{name, data_type, nullable}]` with e.g. `timestamp<UTC>`), NOT the
-- fingerprint-canonical serde form — the fingerprint is verified against a
-- typed exemplar entry at promote/attach time and can never be recomputed
-- from this column.
CREATE TABLE catalogue_data_types (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT        NOT NULL UNIQUE,
    description TEXT,
    columns     JSONB       NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Authorship convention per 20240170: UUID = AuthUser::subject_as_uuid(),
    -- resolvable via user_profiles.
    created_by  UUID,
    updated_by  UUID
);

-- Digest membership: global PK = a digest is owned by AT MOST ONE data type
-- (attaching an already-owned digest is a 409 at the API).
CREATE TABLE catalogue_data_type_digests (
    digest       TEXT        PRIMARY KEY,
    data_type_id UUID        NOT NULL REFERENCES catalogue_data_types(id) ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by   UUID
);

CREATE INDEX idx_cat_dtype_digests_type ON catalogue_data_type_digests (data_type_id);
