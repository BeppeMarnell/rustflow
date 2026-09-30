ALTER TABLE runs ADD COLUMN next_event_sequence bigint NOT NULL DEFAULT 1;
CREATE INDEX run_events_tenant_run_idx ON run_events(tenant_id, run_id, sequence);
