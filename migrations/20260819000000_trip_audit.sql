-- ============================================================================
--  Trip Audit tables
--
--  Populated by the nightly trip scanner: parent trips from the apex Postgres
--  DB are matched against ETIT GPS history, each leg is compared to the OSRM
--  optimal route, and inefficiencies are flagged for dashboard review.
-- ============================================================================

CREATE TABLE IF NOT EXISTS trip_scan_runs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    finished_at      TEXT,
    status           TEXT    NOT NULL DEFAULT 'running',   -- running|success|error
    days_scanned     INTEGER NOT NULL DEFAULT 0,
    trips_scanned    INTEGER NOT NULL DEFAULT 0,
    trips_matched    INTEGER NOT NULL DEFAULT 0,
    trips_unmatched  INTEGER NOT NULL DEFAULT 0,
    flags_raised     INTEGER NOT NULL DEFAULT 0,
    suggestions_made INTEGER NOT NULL DEFAULT 0,
    error            TEXT
);

-- One row per apex parent_trip examined. Re-scans replace the previous match
-- (and its legs/flags) so the latest scan wins.
CREATE TABLE IF NOT EXISTS trip_matches (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    scan_run_id      INTEGER NOT NULL REFERENCES trip_scan_runs(id),
    parent_trip_id   INTEGER NOT NULL,
    day_local        TEXT    NOT NULL,            -- apex trips.date (YYYY-MM-DD)
    company          TEXT    NOT NULL,
    terminal_name    TEXT    NOT NULL,
    car_no_plate     TEXT    NOT NULL,
    vehicle_id       TEXT,                        -- ETIT GUID (cars.etit_car_id)
    driver_name      TEXT,
    status           TEXT    NOT NULL,            -- matched|partial|unmatched
    unmatched_reason TEXT,                        -- no_etit_car_id|no_terminal_coords|
                                                  -- no_dropoff_coords|no_gps_data|
                                                  -- no_terminal_departure|no_dropoff_visits
    start_ts         TEXT,                        -- terminal departure, ISO-8601 UTC
    end_ts           TEXT,                        -- last geofence event, ISO-8601 UTC
    returned_to_terminal INTEGER NOT NULL DEFAULT 0,
    deliveries_expected  INTEGER NOT NULL DEFAULT 0,
    deliveries_visited   INTEGER NOT NULL DEFAULT 0,
    actual_km        REAL,
    osrm_km          REAL,
    expected_km      REAL,                        -- contracted fee_mappings distance sum
    actual_secs      INTEGER,
    osrm_secs        INTEGER,
    distance_ratio   REAL,                        -- actual_km / osrm_km
    duration_ratio   REAL,
    reviewed_at      TEXT,
    review_note      TEXT,
    created_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (parent_trip_id)
);

CREATE INDEX IF NOT EXISTS idx_trip_matches_day    ON trip_matches(day_local);
CREATE INDEX IF NOT EXISTS idx_trip_matches_status ON trip_matches(status);

CREATE TABLE IF NOT EXISTS trip_match_legs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    match_id         INTEGER NOT NULL REFERENCES trip_matches(id) ON DELETE CASCADE,
    seq              INTEGER NOT NULL,
    leg_type         TEXT    NOT NULL,            -- outbound|between|return
    from_name        TEXT    NOT NULL,
    to_name          TEXT    NOT NULL,
    depart_ts        TEXT    NOT NULL,
    arrive_ts        TEXT    NOT NULL,
    night_window     INTEGER NOT NULL DEFAULT 0,  -- 1 if departed inside permit window
    actual_km        REAL    NOT NULL,
    osrm_km          REAL,
    actual_secs      INTEGER NOT NULL,
    osrm_secs        INTEGER,
    distance_ratio   REAL,
    max_deviation_m  REAL,
    actual_geometry  TEXT    NOT NULL DEFAULT '', -- polyline5 of the actual GPS track
    osrm_geometry    TEXT    NOT NULL DEFAULT ''  -- polyline5 of the OSRM optimal route
);

CREATE INDEX IF NOT EXISTS idx_trip_match_legs_match ON trip_match_legs(match_id, seq);

CREATE TABLE IF NOT EXISTS trip_flags (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    match_id     INTEGER NOT NULL REFERENCES trip_matches(id) ON DELETE CASCADE,
    leg_id       INTEGER REFERENCES trip_match_legs(id) ON DELETE CASCADE,
    flag_type    TEXT    NOT NULL,  -- excess_distance|excess_duration|unplanned_stop|
                                    -- route_deviation|skipped_delivery|no_return_data
    severity     TEXT    NOT NULL DEFAULT 'warning',   -- info|warning|critical
    details_json TEXT    NOT NULL DEFAULT '{}',
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX IF NOT EXISTS idx_trip_flags_match ON trip_flags(match_id);

-- GPS-derived pin suggestions for the dashboard locations inbox. Recomputed
-- by the calibration pass; identity is (kind, name) so re-runs update in
-- place and preserve dismissals until the suggestion itself changes.
CREATE TABLE IF NOT EXISTS location_suggestions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    kind          TEXT    NOT NULL,               -- terminal|dropoff
    name          TEXT    NOT NULL,
    suggested_lat REAL    NOT NULL,
    suggested_lng REAL    NOT NULL,
    current_lat   REAL,
    current_lng   REAL,
    offset_m      REAL,                           -- NULL when no current pin
    stop_count    INTEGER NOT NULL,
    scan_run_id   INTEGER NOT NULL REFERENCES trip_scan_runs(id),
    status        TEXT    NOT NULL DEFAULT 'pending',  -- pending|accepted|dismissed
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (kind, name)
);
