-- Workflow templates (top-level container)
CREATE TABLE workflow_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Identity
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',

    -- Version chain
    base_template_id UUID REFERENCES workflow_templates(id),
    parent_id UUID REFERENCES workflow_templates(id),
    version INTEGER NOT NULL DEFAULT 1,
    is_latest BOOLEAN NOT NULL DEFAULT TRUE,

    -- Publishing
    published BOOLEAN NOT NULL DEFAULT FALSE,
    published_at TIMESTAMPTZ,
    published_by UUID,

    -- Graph data (the visual workflow)
    graph JSONB NOT NULL,

    -- Compiled AIR (populated on publish)
    air_json JSONB,

    -- Metadata
    author_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for version chain queries
CREATE INDEX idx_wt_base_template ON workflow_templates(base_template_id);
CREATE INDEX idx_wt_is_latest ON workflow_templates(is_latest) WHERE is_latest = TRUE;
CREATE INDEX idx_wt_published ON workflow_templates(published) WHERE published = TRUE;

-- Workflow instances (running executions)
CREATE TABLE workflow_instances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Template reference (immutable after creation)
    template_id UUID NOT NULL REFERENCES workflow_templates(id),
    template_version INTEGER NOT NULL,

    -- petri-lab mapping
    net_id TEXT NOT NULL UNIQUE,

    -- State (derived from petri-lab, cached for queries)
    status TEXT NOT NULL DEFAULT 'created'
        CHECK (status IN ('created', 'running', 'completed', 'failed', 'cancelled')),

    -- Context
    created_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,

    -- Runtime data
    current_step TEXT,
    metadata JSONB DEFAULT '{}'
);

CREATE INDEX idx_wi_template ON workflow_instances(template_id);
CREATE INDEX idx_wi_status ON workflow_instances(status);
CREATE INDEX idx_wi_net_id ON workflow_instances(net_id);
