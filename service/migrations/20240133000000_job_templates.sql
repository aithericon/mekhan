-- Phase 3 (B-model) — job-template entity + versioning + staging join.
--
-- A "job template" is a reusable, flavor-tagged (slurm | nomad) cluster job
-- spec authored once in the control plane and staged onto N datacenter
-- resources. It mirrors the `resources` / `resource_versions` versioning +
-- soft-delete + workspace-scope pattern (see
-- service/migrations/20240120000000_create_resources.sql) but carries NO
-- vault coupling — a job template is a spec, not a secret.
--
--   job_templates           One row per logical template. Identified by
--                           (workspace_id, slug) among live rows.
--                           `latest_version` bumps when the spec / escape
--                           hatch / declared parameters change; metadata-only
--                           edits (display_name / visibility / consumer_locked)
--                           do not bump.
--   job_template_versions   Immutable per-version snapshot. `common_spec` is the
--                           typed flavor-neutral core; `escape_hatch` is the
--                           flavor-specific raw passthrough; `parameters` is the
--                           declared parameter list. Older versions are retained
--                           so a staging pinned at an older version keeps
--                           resolving.
--   template_stagings       M:N join between a template VERSION and a datacenter
--                           resource, tracking the per-cluster staging lifecycle
--                           (staging → staged | failed | stale).
--
-- `workspace_id` and `datacenter_resource_id` are UUIDs without FK constraints
-- in v1 (no `workspaces` table; `resources` lives in the same DB but the
-- staging row is intentionally decoupled so a soft-deleted datacenter doesn't
-- cascade-drop staging history). Forward-compatible: adding FKs later is a
-- single ALTER.

CREATE TABLE job_templates (
    id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    workspace_id        UUID         NOT NULL,

    -- Rhai/identifier-safe key, unique within a workspace among live rows.
    slug                TEXT         NOT NULL,

    display_name        TEXT         NOT NULL,

    -- Scheduler family this template targets. Drives which escape-hatch slot
    -- is meaningful (slurm → sbatch_directives, nomad → hcl_stanza).
    flavor              TEXT         NOT NULL CHECK (flavor IN ('slurm', 'nomad')),

    visibility          TEXT         NOT NULL DEFAULT 'private'
                                     CHECK (visibility IN ('public', 'private')),

    -- When true, consumers may stage/run this template but not edit its spec.
    consumer_locked     BOOLEAN      NOT NULL DEFAULT FALSE,

    -- Cursor of the most recent `job_template_versions.version`. 0 only in the
    -- transient window between the `job_templates` insert and the first
    -- `job_template_versions` insert; create() lands it at 1.
    latest_version      INTEGER      NOT NULL DEFAULT 0,

    created_by          TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    -- Soft-delete tombstone. NULL = live.
    deleted_at          TIMESTAMPTZ
);

-- Slug uniqueness applies only to live rows so a soft-deleted template frees
-- its slug for re-creation.
CREATE UNIQUE INDEX job_templates_workspace_slug_key
    ON job_templates (workspace_id, slug)
    WHERE deleted_at IS NULL;

CREATE INDEX job_templates_workspace_flavor_idx
    ON job_templates (workspace_id, flavor)
    WHERE deleted_at IS NULL;


CREATE TABLE job_template_versions (
    template_id         UUID         NOT NULL REFERENCES job_templates(id) ON DELETE CASCADE,
    version             INTEGER      NOT NULL,

    -- Typed flavor-neutral core (CommonSpec): cpus / gpus / gpu_type / mem_mb /
    -- time_limit / partition / image / entrypoint / env.
    common_spec         JSONB        NOT NULL,

    -- Flavor-specific raw passthrough (EscapeHatch): slurm fills
    -- `sbatch_directives`, nomad fills `hcl_stanza`. NULL when unused.
    escape_hatch        JSONB,

    -- Declared parameters (Vec<TemplateParameter>): name / kind / required /
    -- default / description.
    parameters          JSONB        NOT NULL DEFAULT '[]'::jsonb,

    created_by          TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    PRIMARY KEY (template_id, version)
);


CREATE TABLE template_stagings (
    id                      UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    template_id             UUID         NOT NULL REFERENCES job_templates(id) ON DELETE CASCADE,
    template_version        INTEGER      NOT NULL,

    -- Datacenter resource this version is staged onto. No FK: a soft-deleted
    -- datacenter must not cascade-drop the staging history.
    datacenter_resource_id  UUID         NOT NULL,

    status                  TEXT         NOT NULL
                                         CHECK (status IN ('staging', 'staged', 'failed', 'stale')),

    -- Remote handle the cluster returned for the staged artifact (e.g. a Nomad
    -- parameterized job id, or a remote template path on Slurm). NULL until staged.
    remote_ref              TEXT,

    staged_at               TIMESTAMPTZ,
    last_error              TEXT,

    created_at              TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at              TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    UNIQUE (template_id, template_version, datacenter_resource_id)
);

CREATE INDEX template_stagings_datacenter_idx
    ON template_stagings (datacenter_resource_id);
