-- ============================================================================
--  Dead-Zone (Restricted Area) tables
-- ============================================================================

CREATE TABLE IF NOT EXISTS dead_zones (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL,
    lat         REAL    NOT NULL,
    lng         REAL    NOT NULL,
    radius_m    REAL    NOT NULL,
    active      INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS zone_visits (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    vehicle_id   TEXT    NOT NULL,
    zone_id      INTEGER NOT NULL REFERENCES dead_zones(id),
    entry_ts     TEXT    NOT NULL,            -- ISO-8601 UTC
    exit_ts      TEXT    NOT NULL,            -- ISO-8601 UTC
    dwell_secs   INTEGER NOT NULL,
    point_count  INTEGER NOT NULL,
    alerted_at   TEXT,                         -- NULL until WhatsApp sent
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (vehicle_id, zone_id, entry_ts)
);

CREATE INDEX IF NOT EXISTS idx_zone_visits_unalerted
    ON zone_visits(alerted_at) WHERE alerted_at IS NULL;

CREATE TABLE IF NOT EXISTS zone_scan_runs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    finished_at      TEXT,
    status           TEXT    NOT NULL DEFAULT 'running',   -- running|success|error
    days_scanned     INTEGER NOT NULL DEFAULT 0,
    vehicles_scanned INTEGER NOT NULL DEFAULT 0,
    violations_found INTEGER NOT NULL DEFAULT 0,
    alerts_sent      INTEGER NOT NULL DEFAULT 0,
    error            TEXT
);
