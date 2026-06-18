-- Per-test resource/pool bindings (`slot_key -> resource_id`), forwarded into
-- the launched `test_run` instance as the highest-precedence binding tier so a
-- test can pin which concrete resource each requirement slot resolves to. Same
-- JSON shape as `CreateInstanceRequest.bindings`. Defaults to an empty object
-- so existing rows + the `SELECT *`-backed `TemplateTest` row decode cleanly.
ALTER TABLE template_tests
    ADD COLUMN bindings JSONB NOT NULL DEFAULT '{}'::jsonb;
