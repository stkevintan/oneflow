CREATE TABLE IF NOT EXISTS virtual_key_state (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    revision BIGINT NOT NULL CHECK (revision >= 0)
);

INSERT INTO virtual_key_state (singleton, revision)
VALUES (TRUE, 0)
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE IF NOT EXISTS virtual_model_config (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    config JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS virtual_keys (
    id TEXT PRIMARY KEY CHECK (id ~ '^[0-9a-f]{32}$'),
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    token_sha256 TEXT NOT NULL CHECK (token_sha256 ~ '^[0-9a-f]{64}$'),
    allowed_models JSONB NOT NULL CHECK (jsonb_typeof(allowed_models) = 'array'),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    expires_at BIGINT CHECK (expires_at >= 0),
    revoked_at BIGINT CHECK (revoked_at >= 0)
);

CREATE INDEX IF NOT EXISTS virtual_keys_created_id_desc
    ON virtual_keys (created_at DESC, id DESC);
