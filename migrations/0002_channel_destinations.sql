-- FeedCourier v0.2.0: Two-layer delivery channel model (Channel Account + Destinations)
-- Channels store encrypted account credentials, proxy, shared pacing/jitter, hourly/daily quotas,
-- and shared cooldown_until.
-- Each Channel can own multiple Destinations (Telegram chat/topic, SMTP recipients/reply_to,
-- Notion data_source_id/column_mapping), each with its own encrypted target config and enabled flag.

CREATE TABLE IF NOT EXISTS channel_destinations (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    encrypted_config BLOB NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_channel_destinations_channel_id
    ON channel_destinations (channel_id);

-- Nullable during SQL migration; populated atomically by startup Rust migration hook
-- (`Database::migrate_channels_to_v2`) using `MasterKey` before handling traffic.
ALTER TABLE routes
    ADD COLUMN destination_id INTEGER REFERENCES channel_destinations(id) ON DELETE CASCADE;

CREATE INDEX IF NOT EXISTS idx_routes_destination_id
    ON routes (destination_id);

ALTER TABLE delivery_tasks
    ADD COLUMN destination_id INTEGER REFERENCES channel_destinations(id) ON DELETE CASCADE;

CREATE INDEX IF NOT EXISTS idx_delivery_tasks_destination_id
    ON delivery_tasks (destination_id);

-- Independent quota & pacing ledger decoupled from business-entity ON DELETE CASCADE
-- so deleting feeds, routes, destinations, or clearing feed cache never resets rolling
-- 1h / 24h channel quotas or min_interval pacing timestamps.
CREATE TABLE IF NOT EXISTS delivery_quota_ledger (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    credential_group_key TEXT NOT NULL,
    channel_id INTEGER NOT NULL,
    completed_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_quota_ledger_group_completed
    ON delivery_quota_ledger (credential_group_key, completed_at DESC);

CREATE INDEX IF NOT EXISTS idx_quota_ledger_channel_completed
    ON delivery_quota_ledger (channel_id, completed_at DESC);

-- Backfill the last 24 hours of delivered tasks for installations upgrading from v0.1.x.
INSERT INTO delivery_quota_ledger (credential_group_key, channel_id, completed_at)
SELECT c.credential_group_key, dt.channel_id, dt.completed_at
FROM delivery_tasks dt
JOIN channels c ON c.id = dt.channel_id
WHERE dt.status = 'delivered'
  AND dt.completed_at IS NOT NULL
  AND dt.completed_at >= unixepoch() - 86400;
