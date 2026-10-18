CREATE TABLE conversation_notes (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    conversation_id uuid NOT NULL,
    author_api_key_id uuid NOT NULL,
    body text NOT NULL CHECK (octet_length(body) BETWEEN 1 AND 10000),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, conversation_id) REFERENCES conversations(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, author_api_key_id) REFERENCES api_keys(tenant_id, id)
);
CREATE INDEX conversation_notes_conversation_idx
    ON conversation_notes(tenant_id, conversation_id, created_at, id);
