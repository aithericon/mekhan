-- Expose the executor `execution_id` on each step row so the UI can address a
-- step's data-plane channel bytes via the datastream tap
-- (`GET /api/v1/executions/{execution_id}/channels/{channel}/data`). Hoisted by
-- the step_executions projection off the AutomatedStep/Agent envelope before
-- `outputs` is unwrapped to its business fields. NULL for non-executor nodes
-- (Start/End/Decision/...). Additive + backfilled on the next event fold.
ALTER TABLE step_execution ADD COLUMN execution_id TEXT;
