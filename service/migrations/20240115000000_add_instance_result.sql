-- Rich return values: the structured result envelope a workflow declares on
-- its End (success) / Failure (error) path. Populated by the lifecycle
-- consumer from `NetCompleted.exit_code` (or synthesized for cancellation).
-- Nullable with no default so existing rows and bare-terminal instances
-- (no result binding) stay NULL — fully backward compatible.
ALTER TABLE workflow_instances ADD COLUMN result JSONB NULL;
