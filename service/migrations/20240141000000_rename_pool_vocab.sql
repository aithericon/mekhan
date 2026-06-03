-- Vocabulary de-collision (docs/23): the word "pool" was overloaded across three
-- distinct concepts. This migration renames the DATA layer to the disambiguated
-- vocabulary:
--   resource_type 'token_pool'    -> 'concurrency_limit'  (bounded-admission limit)
--   resource_type 'presence_pool' -> 'runner_group'       (a group of enrolled runners)
--   allocations.kind 'token_pool_grant' -> 'concurrency_limit_grant'
--   runners.pool / runner_registration_tokens.pool -> runner_group
--       (which runner_group a runner joins)
--
-- The engine net-id primitive (`pool-<resource_id>`) is intentionally UNCHANGED:
-- it is an internal Petri admission-net (a "pool" of unit tokens in the CS sense),
-- invisible to users and shared by BOTH kinds. Only the user-/API-facing layer is
-- renamed. See docs/23 and the capacity-naming refactor.

-- 1. Resource kinds. `resources.resource_type` is free text (no CHECK constraint),
--    so a plain data update suffices.
UPDATE resources SET resource_type = 'concurrency_limit' WHERE resource_type = 'token_pool';
UPDATE resources SET resource_type = 'runner_group'      WHERE resource_type = 'presence_pool';

-- 2. Allocation grant kind. The `kind` column has an inline CHECK constraint
--    (Postgres auto-names it `allocations_kind_check`). Drop it, migrate the data,
--    then re-add the constraint with the renamed kind.
ALTER TABLE allocations DROP CONSTRAINT IF EXISTS allocations_kind_check;
UPDATE allocations SET kind = 'concurrency_limit_grant' WHERE kind = 'token_pool_grant';
ALTER TABLE allocations
    ADD CONSTRAINT allocations_kind_check
    CHECK (kind IN ('datacenter_lease','concurrency_limit_grant'));

-- 3. Runner group membership. The column is renamed to `runner_group` (NOT bare
--    `group` — `GROUP` is a SQL reserved word and would force quoting everywhere).
--    The API/DTO field is `group`; Rust row structs map it via #[sqlx(rename = ...)]
--    or an explicit `runner_group` SELECT.
ALTER TABLE runners                    RENAME COLUMN pool TO runner_group;
ALTER TABLE runner_registration_tokens RENAME COLUMN pool TO runner_group;
