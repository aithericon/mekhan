-- Yjs CRDT document persistence for real-time collaborative editing.
-- Stores incremental updates and periodic snapshots per template.

CREATE TABLE yjs_documents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL REFERENCES workflow_templates(id) ON DELETE CASCADE,
    seq BIGSERIAL NOT NULL,
    update_data BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_yjs_documents_template ON yjs_documents(template_id);
CREATE INDEX idx_yjs_documents_template_seq ON yjs_documents(template_id, seq);

CREATE TABLE yjs_snapshots (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL UNIQUE REFERENCES workflow_templates(id) ON DELETE CASCADE,
    snapshot_data BYTEA NOT NULL,
    snapshot_seq BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_yjs_snapshots_template ON yjs_snapshots(template_id);
