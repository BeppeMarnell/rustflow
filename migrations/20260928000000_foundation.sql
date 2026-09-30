CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE tenants (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE api_keys (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id),
    public_id text NOT NULL UNIQUE,
    secret_digest bytea NOT NULL,
    scopes text[] NOT NULL DEFAULT '{}',
    allowed_flow_ids uuid[],
    allowed_model_aliases text[],
    expires_at timestamptz,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX api_keys_tenant_idx ON api_keys(tenant_id);

CREATE TABLE provider_credentials (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id),
    provider_alias text NOT NULL,
    secret_reference text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, provider_alias)
);

CREATE TABLE flows (
    id uuid PRIMARY KEY,
    tenant_id uuid NOT NULL REFERENCES tenants(id),
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id)
);

CREATE TABLE flow_versions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    flow_id uuid NOT NULL,
    version integer NOT NULL CHECK (version > 0),
    definition jsonb NOT NULL,
    published_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, flow_id) REFERENCES flows(tenant_id, id),
    UNIQUE (flow_id, version),
    UNIQUE (tenant_id, id)
);

CREATE FUNCTION reject_flow_version_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'published flow versions are immutable';
END;
$$;
CREATE TRIGGER flow_versions_immutable
    BEFORE UPDATE OR DELETE ON flow_versions
    FOR EACH ROW EXECUTE FUNCTION reject_flow_version_mutation();

CREATE TABLE conversations (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id)
);

CREATE TABLE messages (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    conversation_id uuid NOT NULL,
    role text NOT NULL CHECK (role IN ('user', 'assistant', 'system')),
    content text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, conversation_id) REFERENCES conversations(tenant_id, id)
);
CREATE INDEX messages_conversation_idx ON messages(tenant_id, conversation_id, created_at);

CREATE TABLE runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    conversation_id uuid NOT NULL,
    flow_version_id uuid NOT NULL,
    status text NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    input jsonb NOT NULL,
    output jsonb,
    error jsonb,
    idempotency_key text,
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz,
    finished_at timestamptz,
    FOREIGN KEY (tenant_id, conversation_id) REFERENCES conversations(tenant_id, id),
    FOREIGN KEY (tenant_id, flow_version_id) REFERENCES flow_versions(tenant_id, id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, idempotency_key)
);
CREATE INDEX runs_tenant_created_idx ON runs(tenant_id, created_at DESC);

CREATE TABLE node_runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    run_id uuid NOT NULL,
    node_id text NOT NULL,
    status text NOT NULL CHECK (status IN ('running', 'succeeded', 'failed', 'skipped')),
    attempt integer NOT NULL DEFAULT 1 CHECK (attempt > 0),
    output jsonb,
    error jsonb,
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    FOREIGN KEY (tenant_id, run_id) REFERENCES runs(tenant_id, id),
    UNIQUE (run_id, node_id, attempt)
);

CREATE TABLE run_events (
    run_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence > 0),
    tenant_id uuid NOT NULL,
    node_id text,
    event_type text NOT NULL,
    status text,
    metadata jsonb NOT NULL DEFAULT '{}',
    occurred_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, sequence),
    FOREIGN KEY (tenant_id, run_id) REFERENCES runs(tenant_id, id)
);

CREATE TABLE jobs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    run_id uuid NOT NULL UNIQUE,
    queue text NOT NULL DEFAULT 'interactive',
    status text NOT NULL CHECK (status IN ('queued', 'leased', 'done', 'failed')),
    available_at timestamptz NOT NULL DEFAULT now(),
    lease_owner text,
    lease_expires_at timestamptz,
    heartbeat_at timestamptz,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts integer NOT NULL DEFAULT 3 CHECK (max_attempts > 0),
    idempotency_key text,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, run_id) REFERENCES runs(tenant_id, id),
    UNIQUE (tenant_id, idempotency_key)
);
CREATE INDEX jobs_claim_idx ON jobs(queue, available_at, created_at)
    WHERE status = 'queued';
CREATE INDEX jobs_expired_lease_idx ON jobs(lease_expires_at)
    WHERE status = 'leased';
