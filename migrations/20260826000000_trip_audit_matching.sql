-- ============================================================================
--  Map-matched distances + open-until-return auditing
-- ============================================================================

-- How the leg's actual_km was computed: 'chord' (haversine point chain) or
-- 'matched' (OSRM map-matched road distance).
ALTER TABLE trip_match_legs ADD COLUMN distance_method TEXT NOT NULL DEFAULT 'chord';

-- Fraction (0..1) of the matched route that lies off the optimal route's
-- corridor. Replaces max-deviation as the primary deviation signal when the
-- leg was map-matched.
ALTER TABLE trip_match_legs ADD COLUMN off_route_pct REAL;

-- Nightly cross-check of our computed day distance against ETIT's own
-- day-mileage figure, per vehicle-day. A persistent gap exposes systematic
-- error in either source.
CREATE TABLE IF NOT EXISTS mileage_checks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    vehicle_id   TEXT NOT NULL,
    day_local    TEXT NOT NULL,
    etit_km      REAL,
    computed_km  REAL,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (vehicle_id, day_local)
);
