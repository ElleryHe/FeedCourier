-- no-transaction
-- FeedCourier v0.3.1: In-process JS Plugins (`boa_engine`), Channel Plugins (`channels.kind = 'plugin'`),
-- and persistent at-least-once `on_delivered` plugin event outbox.

PRAGMA foreign_keys = OFF;

BEGIN IMMEDIATE;

-- 1. Extend `plugins` table with JS plugin source, kind, script persistence, SHA-256, and compiled manifest.
ALTER TABLE plugins
    ADD COLUMN source TEXT NOT NULL DEFAULT 'builtin' CHECK (source IN ('builtin', 'market', 'custom'));
ALTER TABLE plugins
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'processor' CHECK (kind IN ('processor', 'channel'));
ALTER TABLE plugins
    ADD COLUMN script_js TEXT;
ALTER TABLE plugins
    ADD COLUMN script_sha256 TEXT;
ALTER TABLE plugins
    ADD COLUMN manifest_json TEXT;
ALTER TABLE plugins
    ADD COLUMN installed_at INTEGER;
ALTER TABLE plugins
    ADD COLUMN disabled_reason TEXT;

-- 2. Rebuild `channels` table so `kind` accepts 'plugin' alongside 'smtp', 'telegram', 'notion'.
CREATE TABLE channels_v4 (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('smtp', 'telegram', 'notion', 'plugin')),
    encrypted_config BLOB NOT NULL,
    credential_group_key TEXT NOT NULL,
    capabilities_json TEXT NOT NULL DEFAULT '{}',
    min_interval_secs INTEGER NOT NULL DEFAULT 15 CHECK (min_interval_secs >= 0),
    jitter_max_secs INTEGER NOT NULL DEFAULT 10 CHECK (jitter_max_secs >= 0),
    hourly_quota INTEGER CHECK (hourly_quota IS NULL OR hourly_quota > 0),
    daily_quota INTEGER CHECK (daily_quota IS NULL OR daily_quota > 0),
    use_proxy INTEGER NOT NULL DEFAULT 0 CHECK (use_proxy IN (0, 1)),
    cooldown_until INTEGER,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

INSERT INTO channels_v4 (
    id, name, kind, encrypted_config, credential_group_key, capabilities_json,
    min_interval_secs, jitter_max_secs, hourly_quota, daily_quota, use_proxy,
    cooldown_until, enabled, created_at, updated_at
)
SELECT
    id, name, kind, encrypted_config, credential_group_key, capabilities_json,
    min_interval_secs, jitter_max_secs, hourly_quota, daily_quota, use_proxy,
    cooldown_until, enabled, created_at, updated_at
FROM channels;

-- Snapshot previous `channels` autoincrement watermark so deleted tail channel IDs are never
-- reused after table rebuild (preserving isolation of `delivery_quota_ledger` and `logs.db`).
CREATE TEMP TABLE _fc_channels_seq_bak (seq INTEGER);
INSERT INTO _fc_channels_seq_bak (seq)
SELECT seq FROM sqlite_sequence WHERE name = 'channels';

DROP TABLE channels;
ALTER TABLE channels_v4 RENAME TO channels;

DELETE FROM sqlite_sequence WHERE name = 'channels';
INSERT INTO sqlite_sequence (name, seq)
SELECT 'channels', max_seq
FROM (
    SELECT MAX(val) AS max_seq
    FROM (
        SELECT COALESCE(MAX(seq), 0) AS val FROM _fc_channels_seq_bak
        UNION ALL
        SELECT COALESCE(MAX(id), 0) AS val FROM channels
    )
)
WHERE max_seq > 0;
DROP TABLE _fc_channels_seq_bak;

CREATE INDEX IF NOT EXISTS idx_channels_credential_group
    ON channels (credential_group_key);

-- 3. Persistent at-least-once event outbox for `on_delivered` plugin hooks.
CREATE TABLE IF NOT EXISTS plugin_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    plugin_id TEXT NOT NULL REFERENCES plugins(id) ON DELETE CASCADE,
    event TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (plugin_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_plugin_events_due
    ON plugin_events (next_attempt_at ASC, id ASC);

COMMIT;

PRAGMA foreign_keys = ON;
