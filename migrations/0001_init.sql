-- FeedCourier initial schema (SQLite WAL)
-- All timestamps are UTC Unix epoch seconds (INTEGER).

CREATE TABLE IF NOT EXISTS feeds (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    url TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL DEFAULT '',
    site_url TEXT,
    favicon_url TEXT,
    tags_json TEXT NOT NULL DEFAULT '[]',
    poll_interval_secs INTEGER NOT NULL DEFAULT 1800 CHECK (poll_interval_secs > 0),
    fetch_full_content INTEGER NOT NULL DEFAULT 0 CHECK (fetch_full_content IN (0, 1)),
    use_proxy INTEGER NOT NULL DEFAULT 0 CHECK (use_proxy IN (0, 1)),
    etag TEXT,
    last_modified TEXT,
    health_status TEXT NOT NULL DEFAULT 'active' CHECK (
        health_status IN ('active', 'backoff', 'circuit_open', 'paused')
    ),
    consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
    next_poll_at INTEGER,
    last_error TEXT,
    silent_init INTEGER NOT NULL DEFAULT 1 CHECK (silent_init IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_feeds_poll_schedule
    ON feeds (health_status, next_poll_at);

CREATE TABLE IF NOT EXISTS entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    feed_id INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
    fingerprint_sha256 TEXT NOT NULL CHECK (length(fingerprint_sha256) = 64),
    published_at INTEGER,
    discovered_at INTEGER NOT NULL DEFAULT (unixepoch()),
    payload_purged_at INTEGER,
    title TEXT,
    canonical_url TEXT,
    author TEXT,
    content_html TEXT,
    first_sentence TEXT,
    summary_text TEXT,
    media_json TEXT,
    UNIQUE (feed_id, fingerprint_sha256)
);

CREATE INDEX IF NOT EXISTS idx_entries_feed_discovered
    ON entries (feed_id, discovered_at DESC);

CREATE INDEX IF NOT EXISTS idx_entries_gc_candidates
    ON entries (discovered_at)
    WHERE payload_purged_at IS NULL;

CREATE TABLE IF NOT EXISTS channels (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('smtp', 'telegram', 'notion')),
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

CREATE INDEX IF NOT EXISTS idx_channels_credential_group
    ON channels (credential_group_key);

CREATE TABLE IF NOT EXISTS routes (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    name TEXT NOT NULL,
    feed_ids_json TEXT NOT NULL DEFAULT '[]',
    match_tags_json TEXT NOT NULL DEFAULT '[]',
    channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    include_keywords_json TEXT NOT NULL DEFAULT '[]',
    exclude_keywords_json TEXT NOT NULL DEFAULT '[]',
    include_regex TEXT,
    exclude_regex TEXT,
    options_json TEXT NOT NULL DEFAULT '{}',
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_routes_channel_id
    ON routes (channel_id);

CREATE INDEX IF NOT EXISTS idx_routes_enabled
    ON routes (enabled);

CREATE TABLE IF NOT EXISTS delivery_tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    route_id INTEGER NOT NULL REFERENCES routes(id) ON DELETE CASCADE,
    channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    entry_id INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'queued' CHECK (
        status IN ('queued', 'delivering', 'delivered', 'retrying', 'deferred', 'dead_letter')
    ),
    lease_until INTEGER,
    checkpoint_json TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_attempt_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_status_code INTEGER,
    diagnostic_log TEXT,
    completed_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (route_id, entry_id)
);

CREATE INDEX IF NOT EXISTS idx_delivery_tasks_status_next_attempt
    ON delivery_tasks (status, next_attempt_at);

CREATE INDEX IF NOT EXISTS idx_delivery_tasks_status_lease
    ON delivery_tasks (status, lease_until);

CREATE INDEX IF NOT EXISTS idx_delivery_tasks_entry_status
    ON delivery_tasks (entry_id, status);

CREATE INDEX IF NOT EXISTS idx_delivery_tasks_channel_status_completed
    ON delivery_tasks (channel_id, status, completed_at);

CREATE TABLE IF NOT EXISTS system_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS mcp_tokens (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    token_prefix TEXT NOT NULL,
    scope TEXT NOT NULL CHECK (scope IN ('readonly', 'admin')),
    last_used_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS sessions (
    token_hash TEXT PRIMARY KEY NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_sessions_expires_at
    ON sessions (expires_at);
