-- Re-shape the seeded `default` worker group's capacity row from the
-- legacy (dispatch + exclusivity) axes to the new (acceptance) axis.
--
-- Background: migration 20240144000000 originally seeded every workspace
-- with a default worker-group whose `public_config` carried
-- {dispatch:'pull', exclusivity:'hold'}. Commit 58dd5079 edited the
-- seed in place to {acceptance:'auto'} — but that's an applied
-- migration, so editing it broke the sqlx checksum on any DB that had
-- already applied 20240144 (i.e. every prod and long-lived dev DB).
--
-- This migration restores the contract by:
--   1. Reverting 20240144 to its as-originally-applied content (handled
--      out-of-band; this file does NOT touch 20240144 itself).
--   2. Updating any seeded row that still carries the legacy axes to
--      the new shape, in place.
--
-- Idempotent: matches only rows whose `public_config` has the old keys.
-- DBs that never had a 20240144 apply (and got the seed via a future
-- replay) are untouched. DBs whose default group has been HAND-modified
-- since the seed are also untouched — we don't blindly overwrite.

UPDATE resource_versions
SET public_config = (public_config
        - 'dispatch'
        - 'exclusivity')
    || jsonb_build_object('acceptance', 'auto')
WHERE
    -- Only the seeded default-group capacity row (vault_path shape
    -- + version 1 narrow the blast radius; the JSON key check
    -- guarantees we only touch rows still on the legacy axes).
    version = 1
    AND vault_path LIKE 'aithericon/resources/%/v1'
    AND public_config ? 'dispatch'
    AND public_config ? 'exclusivity'
    AND public_config ->> 'liveness' = 'competing_consumer'
    AND public_config ->> 'capacity_kind' = 'fixed';
