-- Stored model configurations, oldest first, for review and rollback.
-- `config` is redacted (see config_secrets::redact): literal provider
-- `api_key` values are never kept here, so a removed key does not linger in
-- history. Rolling back reuses the keys currently stored for the same model
-- names. The control plane seeds the first entry and prunes old ones.
CREATE TABLE model_config_history (
    id BIGSERIAL PRIMARY KEY,
    version TEXT NOT NULL,
    config JSONB NOT NULL,
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    source TEXT NOT NULL CHECK (source IN ('initial', 'put', 'rollback')),
    rolled_back_from BIGINT
);
