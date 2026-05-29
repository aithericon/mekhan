-- Instance hierarchy for SubWorkflow drill-in.
--
-- A SubWorkflow node spawns its child as a SEPARATE engine net (random
-- `child_net_id`). Until now those child nets had no `workflow_instances`
-- row, so mekhan's step-execution projection dropped every child event
-- (`load_instance_context` keys on `net_id` and found nothing) and the
-- instance UI had no handle to drill into. These columns let the causality
-- ingest register a first-class child instance row (net_id = child_net_id)
-- the moment it sees the parent's `spawn_net` EffectCompleted, so the
-- existing step-execution projection + instance views light up for children
-- for free.
--
--   parent_instance_id — the instance that ran the SubWorkflow node.
--   parent_node_id     — the SubWorkflow `WorkflowNode.id` in the parent graph
--                        (the spawn transition is `t_{parent_node_id}_spawn`).
--   root_instance_id   — the top-of-tree instance (parent's root, or parent
--                        itself), so a whole tree is reachable in one query.
--   spawn_seq          — the parent net's spawn EffectCompleted event sequence;
--                        orders the children when a Loop/Map spawns the same
--                        node multiple times (one child per iteration).
ALTER TABLE workflow_instances
    ADD COLUMN parent_instance_id UUID NULL REFERENCES workflow_instances(id) ON DELETE SET NULL,
    ADD COLUMN parent_node_id     TEXT NULL,
    ADD COLUMN root_instance_id   UUID NULL REFERENCES workflow_instances(id) ON DELETE SET NULL,
    ADD COLUMN spawn_seq          BIGINT NULL;

CREATE INDEX idx_wi_parent ON workflow_instances(parent_instance_id) WHERE parent_instance_id IS NOT NULL;
CREATE INDEX idx_wi_root   ON workflow_instances(root_instance_id)   WHERE root_instance_id   IS NOT NULL;
