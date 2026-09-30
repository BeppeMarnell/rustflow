ALTER TABLE tenants
    ADD COLUMN max_concurrent_runs integer NOT NULL DEFAULT 100 CHECK (max_concurrent_runs > 0),
    ADD COLUMN max_runs_per_minute integer NOT NULL DEFAULT 1000 CHECK (max_runs_per_minute > 0);
ALTER TABLE api_keys
    ADD COLUMN max_concurrent_runs integer NOT NULL DEFAULT 20 CHECK (max_concurrent_runs > 0),
    ADD COLUMN max_runs_per_minute integer NOT NULL DEFAULT 60 CHECK (max_runs_per_minute > 0);
ALTER TABLE runs ADD COLUMN api_key_id uuid REFERENCES api_keys(id);
CREATE INDEX runs_key_created_idx ON runs(api_key_id, created_at DESC);
CREATE INDEX runs_key_active_idx ON runs(api_key_id) WHERE status IN ('queued', 'running');
CREATE INDEX runs_tenant_active_idx ON runs(tenant_id) WHERE status IN ('queued', 'running');
