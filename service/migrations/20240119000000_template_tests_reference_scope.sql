-- Reference scope: a snapshot of the synthetic `{ result, steps.<slug>.output }`
-- object the assertion DSL evaluates against. Captured at promote-time from
-- the source instance, then refreshed after every successful run so authors
-- always have a recent-and-relevant reference visible while editing.
ALTER TABLE template_tests
    ADD COLUMN reference_scope JSONB;
