-- FeedCourier v0.3.0: Built-in Plugin Framework & SMTP Per-Recipient Compliance

CREATE TABLE IF NOT EXISTS plugins (
    id TEXT PRIMARY KEY NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    installed_version TEXT NOT NULL,
    settings_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS plugin_instances (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    plugin_id TEXT NOT NULL REFERENCES plugins(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    encrypted_config BLOB NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_plugin_instances_plugin_id
    ON plugin_instances (plugin_id);

CREATE TABLE IF NOT EXISTS plugin_link_cache (
    instance_id INTEGER NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
    long_url_sha256 TEXT NOT NULL,
    short_url TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_used_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (instance_id, long_url_sha256)
);

CREATE INDEX IF NOT EXISTS idx_plugin_link_cache_last_used
    ON plugin_link_cache (last_used_at);

CREATE TABLE IF NOT EXISTS smtp_recipients (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    destination_id INTEGER NOT NULL REFERENCES channel_destinations(id) ON DELETE CASCADE,
    address_hmac TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('unverified', 'pending', 'verified', 'unsubscribed', 'bounced')),
    code_sha256 TEXT,
    code_expires_at INTEGER,
    code_attempts INTEGER NOT NULL DEFAULT 0,
    last_code_sent_at INTEGER,
    codes_sent_window_start INTEGER,
    codes_sent_in_window INTEGER NOT NULL DEFAULT 0,
    consent_method TEXT,
    consented_at INTEGER,
    revoked_method TEXT,
    revoked_at INTEGER,
    bounce_diagnostic TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (destination_id, address_hmac)
);

CREATE INDEX IF NOT EXISTS idx_smtp_recipients_destination_status
    ON smtp_recipients (destination_id, status);

ALTER TABLE channel_destinations
    ADD COLUMN auto_paused INTEGER NOT NULL DEFAULT 0 CHECK (auto_paused IN (0, 1));
