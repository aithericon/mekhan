-- Add a third `visibility` tier: `private`. A private template is a
-- sub-workflow owned by exactly one parent workflow family — hidden from the
-- catalogue, not runnable standalone, and embeddable only by its owner. It is
-- the workflow analogue of Rust's module-private `fn` (vs. `public` = `pub`,
-- `workspace` = `pub(crate)`).
--
-- `owner_template_id` is the owning parent's FAMILY base id
-- (COALESCE(base_template_id, id)). Kept as a plain UUID with no FK: an owner
-- family may be deleted, which harmlessly orphans the private child (it then
-- references nobody and remains non-runnable until re-homed or deleted).

-- Extend the enum. The original constraint was added as an inline column-level
-- CHECK in 20240124, so Postgres named it `<table>_<column>_check`. `IF EXISTS`
-- keeps this safe if the name ever differs.
ALTER TABLE workflow_templates DROP CONSTRAINT IF EXISTS workflow_templates_visibility_check;
ALTER TABLE workflow_templates ADD  CONSTRAINT workflow_templates_visibility_check
    CHECK (visibility IN ('workspace','public','private'));

ALTER TABLE workflow_templates ADD COLUMN owner_template_id UUID;

-- A private template MUST declare its owner; non-private rows MUST NOT.
ALTER TABLE workflow_templates ADD CONSTRAINT workflow_templates_private_owner_chk
    CHECK (
        (visibility = 'private' AND owner_template_id IS NOT NULL)
     OR (visibility <> 'private' AND owner_template_id IS NULL)
    );

-- Lookup path: the editor picker enumerates a parent's own private children
-- via `?owner_template_id=`. Partial — only private rows carry an owner.
CREATE INDEX idx_templates_owner ON workflow_templates(owner_template_id)
    WHERE owner_template_id IS NOT NULL;
