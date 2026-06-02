-- Container-staging (docs/22): a Slurm `Scheduled` job template may bind a
-- `container_image` resource. mekhan materializes that image to an Apptainer
-- `.sif` on the cluster and runs the drain executor inside it.
--
-- Forward-only column (NOT folded into 20240133000000_job_templates.sql, which
-- is already applied on live/dev DBs — editing it would break the sqlx
-- migration checksum). Nullable, no FK in v1: a dangling reference resolves to
-- "no container" at compile time rather than a hard constraint, matching how
-- cluster/resource aliases are resolved softly elsewhere.
ALTER TABLE job_templates
    ADD COLUMN container_resource_id UUID;
