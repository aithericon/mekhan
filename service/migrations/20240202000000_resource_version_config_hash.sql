-- Change-detection hash for the GitOps resource `apply` (upsert) path.
--
-- `POST /api/v1/resources/apply` is path-keyed and idempotent: re-applying an
-- unchanged resource must be a no-op rather than minting a useless new version.
-- To decide "changed vs unchanged" without ever reading the secret half back
-- (secrets live in Vault and are never re-emitted on the wire), every version
-- write records a SHA-256 over the canonical JSON of the FULL submitted config
-- (public ∪ secret, after capacity-preset expansion). The apply handler
-- compares the incoming hash against the latest version's stored hash.
--
-- Nullable: pre-existing `resource_versions` rows carry NULL. The apply path
-- treats a NULL stored hash as "changed", so the first apply of a legacy
-- resource bumps exactly once (re-stamping the hash) and every apply after that
-- is a clean no-op. The interactive CRUD paths (create/update/rotate) always
-- write a non-NULL hash going forward.
ALTER TABLE resource_versions
    ADD COLUMN config_hash TEXT;
