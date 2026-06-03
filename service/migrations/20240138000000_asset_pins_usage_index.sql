-- Reverse-lineage support (docs/20 §9): "which runs used asset X" filters
-- workflow_instances by the asset_id embedded in the asset_pins JSONB map
-- (`{alias -> {asset_id, version}}`). The /api/v1/assets/{id}/usage endpoint
-- queries with the `@?` jsonpath operator, which a GIN index over
-- `jsonb_path_ops` accelerates.
CREATE INDEX IF NOT EXISTS idx_workflow_instances_asset_pins
    ON workflow_instances USING gin (asset_pins jsonb_path_ops);
