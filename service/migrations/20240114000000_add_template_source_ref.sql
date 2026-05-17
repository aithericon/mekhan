-- GitOps provenance: the git ref that produced a published template version.
--
-- `mekhan apply` authors a workflow in git and publishes it atomically; this
-- column records { remote, sha, dirty, ref? } so a published AIR is traceable
-- back to a reviewable commit. NULL for every UI-published / new_version row —
-- `apply` is the only writer, so its presence also distinguishes a
-- git-managed version from a web-authored one.

ALTER TABLE workflow_templates ADD COLUMN source_ref JSONB;
