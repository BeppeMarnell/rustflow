CREATE TABLE ragflow_datasets (
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    dataset_id text NOT NULL CHECK (dataset_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, dataset_id)
);
