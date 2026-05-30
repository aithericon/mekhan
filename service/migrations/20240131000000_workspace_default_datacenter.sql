-- Multi-cluster scheduling (docs/16 §6): the LAST rung of the cluster
-- selection chain. When a Scheduled/leased step names no cluster on the node
-- AND the template carries no `default_scheduler`, publish falls back to this
-- workspace-level default datacenter.
--
-- Stored as a `resource_id` (not an alias) because it IS the pin — it skips
-- the alias→resource lookup the node/template rungs need. `ON DELETE SET NULL`
-- so deleting the datacenter resource silently clears the default rather than
-- orphaning a dangling FK (a publish that then has no resolution hard-fails
-- with `SchedulerUnresolved`, which is the correct loud failure).
ALTER TABLE workspaces
    ADD COLUMN default_datacenter_resource_id UUID NULL
        REFERENCES resources(id) ON DELETE SET NULL;
