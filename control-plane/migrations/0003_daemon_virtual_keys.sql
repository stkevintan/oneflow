ALTER TABLE virtual_keys
    ADD COLUMN daemon_id TEXT REFERENCES daemon_credentials (daemon_id);

CREATE INDEX virtual_keys_daemon_created_id_desc
    ON virtual_keys (daemon_id, created_at DESC, id DESC)
    WHERE revoked_at IS NULL;
