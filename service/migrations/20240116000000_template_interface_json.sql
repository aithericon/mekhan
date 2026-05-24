-- Per-node compiler sub-graph interface registry, persisted alongside
-- `air_json`. Sidecar metadata: NOT embedded in AIR; a parent compile that
-- embeds this template via a `SubWorkflow` node reads this column to find
-- the child's entry place + workflow-exit terminals without re-deriving them
-- from naming conventions or `place_type` filtering.
--
-- See `service/src/compiler/interface.rs` for the serialized shape and
-- `service/src/process/publish.rs::resolve_subworkflow_air` for the consumer.
-- NULL on existing rows; the resolver falls back to the legacy filter when
-- the column is absent.
ALTER TABLE workflow_templates
    ADD COLUMN interface_json JSONB;
