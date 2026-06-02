-- Container-staging (docs/22): tracks the materialization of a `container_image`
-- resource into an Apptainer `.sif` on a specific datacenter cluster. Mirrors
-- `template_stagings` (20240133000000) — a one-shot `materialize-<row_id>` Petri
-- net fires the `materialize_image` engine effect, whose terminal event the
-- `image_materializations` projection folds into the row.
--
-- Keyed by (container_resource_id, container_version, datacenter_resource_id):
-- an image is pulled onto each cluster where it's used; content-addressing by
-- `digest` dedups identical pulls within a cluster's shared FS.
CREATE TABLE image_materializations (
    id                      UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    -- The `container_image` resource + the version that was materialized.
    container_resource_id   UUID         NOT NULL,
    container_version       INTEGER      NOT NULL,

    -- The datacenter cluster the image was pulled onto (its login node).
    datacenter_resource_id  UUID         NOT NULL,

    -- materializing → ready | failed (mirrors template_stagings.status).
    status                  TEXT         NOT NULL DEFAULT 'materializing'
                                         CHECK (status IN ('materializing', 'ready', 'failed', 'stale')),

    -- Content address of the produced .sif (sha256 of the file) + its absolute
    -- path on the cluster's shared FS. NULL until the pull completes.
    digest                  TEXT,
    sif_path                TEXT,
    size_bytes              BIGINT,

    last_error              TEXT,
    created_at              TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at              TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

-- One row per (image version × datacenter); re-materializing upserts it.
CREATE UNIQUE INDEX image_materializations_unique
    ON image_materializations (container_resource_id, container_version, datacenter_resource_id);

CREATE INDEX image_materializations_container_idx
    ON image_materializations (container_resource_id);
CREATE INDEX image_materializations_datacenter_idx
    ON image_materializations (datacenter_resource_id);
