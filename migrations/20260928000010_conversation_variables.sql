-- Existing conversations can be claimed only when their creator key is known.
-- Unclaimed legacy rows remain inaccessible through the user-facing API.
ALTER TABLE api_keys ADD CONSTRAINT api_keys_tenant_id_id_unique UNIQUE (tenant_id, id);
ALTER TABLE conversations ADD COLUMN owner_api_key_id uuid;
UPDATE conversations c SET owner_api_key_id = (
    SELECT r.api_key_id FROM runs r
    WHERE r.tenant_id = c.tenant_id AND r.conversation_id = c.id AND r.api_key_id IS NOT NULL
    ORDER BY r.created_at, r.id LIMIT 1
);
ALTER TABLE conversations ADD CONSTRAINT conversations_owner_key_fk
    FOREIGN KEY (tenant_id, owner_api_key_id) REFERENCES api_keys(tenant_id, id);
CREATE INDEX conversations_owner_idx ON conversations(tenant_id, owner_api_key_id, id);

CREATE TABLE conversation_variables (
    tenant_id uuid NOT NULL,
    conversation_id uuid NOT NULL,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 64),
    value jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, conversation_id, name),
    FOREIGN KEY (tenant_id, conversation_id) REFERENCES conversations(tenant_id, id) ON DELETE CASCADE
);
