-- ============================================================================
--  etit-proxy v2 — initial schema
-- ============================================================================
--  Persists Tier A (vehicle metadata) and tiers C/D (history) across restarts.
--  Tier B (live status) is intentionally NOT persisted — it is fully
--  ephemeral and overwritten every minute by the poller.
-- ============================================================================

-- ---------------------------------------------------------------------------
-- Vehicles (Tier A snapshot, persisted so cold-start has data immediately)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS vehicles (
    id                TEXT    PRIMARY KEY,
    codename          TEXT    NOT NULL,
    plate             TEXT    NOT NULL DEFAULT '',
    speed_limit       INTEGER NOT NULL DEFAULT 0,
    online            INTEGER NOT NULL DEFAULT 0, -- boolean
    speed             INTEGER NOT NULL DEFAULT 0,
    status            INTEGER NOT NULL DEFAULT 0,
    status_label      TEXT    NOT NULL DEFAULT '',
    last_location_at  TEXT,                       -- ISO-8601 / null
    extra_json        TEXT    NOT NULL DEFAULT '{}',
    refreshed_at      TEXT    NOT NULL            -- ISO-8601
);

CREATE INDEX IF NOT EXISTS idx_vehicles_codename ON vehicles(codename);

-- ---------------------------------------------------------------------------
-- History days
-- ---------------------------------------------------------------------------
-- One row per (vehicle, calendar_day_local). points_json is the flat array
-- of HistoryPoint records the API serves to the frontend; geometry is the
-- pre-computed Google-compatible polyline for the same array.
--
-- `is_hot` lets the polling layer mark today/yesterday entries as
-- "rewriteable" — once the calendar rolls over, the row stays in this
-- table but is_hot flips to 0 and it becomes the "cold" cache.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS history_days (
    vehicle_id   TEXT    NOT NULL,
    day_local    TEXT    NOT NULL, -- 'YYYY-MM-DD' in configured timezone
    fetched_at   TEXT    NOT NULL, -- ISO-8601 UTC
    point_count  INTEGER NOT NULL,
    points_json  TEXT    NOT NULL,
    geometry     TEXT    NOT NULL,
    is_hot       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (vehicle_id, day_local)
);

CREATE INDEX IF NOT EXISTS idx_history_hot       ON history_days(is_hot)     WHERE is_hot = 1;
CREATE INDEX IF NOT EXISTS idx_history_fetched   ON history_days(fetched_at);

-- ---------------------------------------------------------------------------
-- Audit log: rolling record of upstream calls. Useful for ops dashboards
-- and post-mortems on session/redirect storms. Capped at ~10k rows by
-- application code (DELETE FROM ... WHERE id NOT IN ...).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS upstream_audit (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    at           TEXT    NOT NULL,
    kind         TEXT    NOT NULL,         -- 'login' | 'vehicles' | 'live' | 'history'
    ok           INTEGER NOT NULL,
    duration_ms  INTEGER NOT NULL,
    detail       TEXT
);

CREATE INDEX IF NOT EXISTS idx_audit_at ON upstream_audit(at);
