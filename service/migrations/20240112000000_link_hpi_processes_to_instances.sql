-- Link each HPI process back to the workflow instance / petri net that
-- produced it.
--
-- Until now there was no join key from a process to its instance/template:
-- hpi_processes.process_id is the seed token id, and the instance id only
-- lived inside causality_event_tokens.token_data._instance_id (injected when
-- the start token is parameterised — see service/src/petri/instance.rs).
-- Both columns are nullable: petri-lab scenarios created outside a mekhan
-- instance legitimately have no instance, and pre-existing rows that never
-- carried _instance_id stay unlinked.

ALTER TABLE hpi_processes ADD COLUMN instance_id UUID;
ALTER TABLE hpi_processes ADD COLUMN net_id      TEXT;

CREATE INDEX idx_hpi_proc_instance_id ON hpi_processes (instance_id) WHERE instance_id IS NOT NULL;
CREATE INDEX idx_hpi_proc_net_id      ON hpi_processes (net_id)      WHERE net_id IS NOT NULL;

-- Backfill existing processes. The process_id IS the seed token id, so the
-- matching produced token row in causality_event_tokens holds _instance_id
-- in its token_data JSON. net_id is the engine convention "mekhan-{uuid}".
UPDATE hpi_processes hp
SET instance_id = (cet.token_data ->> '_instance_id')::uuid,
    net_id      = 'mekhan-' || (cet.token_data ->> '_instance_id')
FROM causality_event_tokens cet
WHERE cet.token_id = hp.process_id
  AND cet.role = 'produced'
  AND cet.token_data ? '_instance_id'
  AND hp.instance_id IS NULL;
