ALTER TABLE tenants
    ADD COLUMN message_retention_days integer
        CHECK (message_retention_days BETWEEN 1 AND 3650),
    ADD COLUMN run_retention_days integer
        CHECK (run_retention_days BETWEEN 1 AND 3650);

CREATE INDEX runs_retention_idx ON runs(finished_at)
    WHERE status IN ('succeeded', 'failed', 'cancelled');
CREATE INDEX messages_retention_idx ON messages(created_at);
