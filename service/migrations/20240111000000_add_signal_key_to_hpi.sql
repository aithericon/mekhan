-- Tag metric/log rows with the originating signal_key so the provenance
-- event-detail sheet can scope them to a single executor job.
--
-- Before this, both tables keyed only on `process_id`. In a BO campaign
-- every iteration shares one process_id, so the Submit Execution detail
-- view showed metrics from all 30 iterations jumbled together. With a
-- signal_key tag we can filter to exactly the job that this specific
-- Submit Execution dispatched.

ALTER TABLE hpi_metrics ADD COLUMN signal_key TEXT;
ALTER TABLE hpi_logs    ADD COLUMN signal_key TEXT;

CREATE INDEX idx_hpi_metrics_signal_key ON hpi_metrics (signal_key) WHERE signal_key IS NOT NULL;
CREATE INDEX idx_hpi_logs_signal_key    ON hpi_logs    (signal_key) WHERE signal_key IS NOT NULL;
