-- ============================================================================
--  vehicles: live columns are now kept current by Tier B
-- ============================================================================
--  Until now only Tier A (GetTransportersPage, daily) wrote this table, so
--  online / speed / status / status_label / last_location_at were a snapshot
--  as old as the last Tier A refresh — while the API and the table presented
--  them as current. Tier B (GetTransportersSpeedStatus, every minute) now
--  updates those columns for every vehicle on each poll, and Tier A keeps the
--  metadata (codename, plate, speed_limit, extra_json).
--
--  refreshed_at: when the row was last written by either tier.
--  lat / lng:    last known position, from Tier B (NULL until the first poll).
-- ============================================================================

ALTER TABLE vehicles ADD COLUMN lat REAL;
ALTER TABLE vehicles ADD COLUMN lng REAL;
