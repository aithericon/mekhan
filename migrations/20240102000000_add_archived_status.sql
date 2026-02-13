-- Add 'archived' to the status enum for cleanup lifecycle
ALTER TABLE workflow_instances
    DROP CONSTRAINT IF EXISTS workflow_instances_status_check,
    ADD CONSTRAINT workflow_instances_status_check
        CHECK (status IN ('created', 'running', 'completed', 'failed', 'cancelled', 'archived'));
