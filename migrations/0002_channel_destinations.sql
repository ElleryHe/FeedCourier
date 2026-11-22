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
