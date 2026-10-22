CREATE TABLE daemon_credentials (
    daemon_id TEXT PRIMARY KEY CHECK (daemon_id ~ '^[A-Za-z0-9._-]{1,128}$'),
    token_sha256 TEXT NOT NULL CHECK (token_sha256 ~ '^[0-9a-f]{64}$'),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    rotated_at BIGINT CHECK (rotated_at >= 0),
    revoked_at BIGINT CHECK (revoked_at >= 0)
);
