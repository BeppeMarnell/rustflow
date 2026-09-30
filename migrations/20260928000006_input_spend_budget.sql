ALTER TABLE tenants
    ADD COLUMN max_input_tokens_per_day bigint NOT NULL DEFAULT 200000000
        CHECK (max_input_tokens_per_day > 0),
    ADD COLUMN max_spend_microusd_per_day bigint NOT NULL DEFAULT 100000000
        CHECK (max_spend_microusd_per_day > 0);

ALTER TABLE api_keys
    ADD COLUMN max_input_tokens_per_day bigint NOT NULL DEFAULT 20000000
        CHECK (max_input_tokens_per_day > 0),
    ADD COLUMN max_spend_microusd_per_day bigint NOT NULL DEFAULT 10000000
        CHECK (max_spend_microusd_per_day > 0);

ALTER TABLE runs
    ADD COLUMN reserved_input_tokens bigint NOT NULL DEFAULT 0
        CHECK (reserved_input_tokens >= 0),
    ADD COLUMN reserved_spend_microusd bigint NOT NULL DEFAULT 0
        CHECK (reserved_spend_microusd >= 0);
