ALTER TABLE api_keys ADD COLUMN primary_token_revoked_at timestamptz;

CREATE TABLE api_key_tokens (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    key_id uuid NOT NULL REFERENCES api_keys(id),
    public_id text NOT NULL UNIQUE,
    secret_digest bytea NOT NULL,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX api_key_tokens_key_idx ON api_key_tokens(key_id, created_at);
