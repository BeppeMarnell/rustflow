ALTER TABLE tenants ADD COLUMN max_output_tokens_per_day bigint NOT NULL DEFAULT 10000000
    CHECK (max_output_tokens_per_day > 0);
ALTER TABLE api_keys ADD COLUMN max_output_tokens_per_day bigint NOT NULL DEFAULT 1000000
    CHECK (max_output_tokens_per_day > 0);
ALTER TABLE runs ADD COLUMN reserved_output_tokens bigint NOT NULL DEFAULT 0
    CHECK (reserved_output_tokens >= 0);
