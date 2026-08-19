# Trip Audit — coordinated deployment runbook

The trip-audit feature spans four pieces that ship together as one release:
FalconGo (locations schema), etit-proxy-rust (scanner + API), apex-react
(Locations page + Trip Audit page), and the truck OSRM dataset. Nothing here
is deployed until every part is built and the local dry-run has passed.

## Order of operations (VPS: 187.124.33.153)

1. **Truck OSRM dataset**
   - `rsync` the locally-built `egypt-truck.osrm*` files (see `osrm/build.sh`)
     to `/opt/osrm/`.
   - Add a second service to `/opt/madar-rust/docker-compose.yml`:
     ```yaml
     osrm-truck:
       image: osrm/osrm-backend:latest
       restart: unless-stopped
       ports:
         - "127.0.0.1:5001:5000"
       volumes:
         - /opt/osrm:/data
       command: osrm-routed --algorithm mld /data/egypt-truck.osrm
       networks:
         - madar-net
     ```
     (Car dataset on :5000 keeps serving madar unchanged. RAM check
     2026-08-19: car instance uses ~320 MiB RSS, ~2.4 GiB available.)
   - `docker compose up -d osrm-truck`, then run `osrm/test.sh` with
     `OSRM_URL=http://127.0.0.1:5001` on the VPS.

2. **FalconGo** (first, because its migration creates the tables the proxy
   reads)
   - Deploy the new binary the usual way; on startup AutoMigrate creates
     `drop_off_points`, `terminal_aliases`, `terminals.radius_m` and the
     backfill seeds 277 drop-off names (≈250 pinned from fee_mappings).
   - Verify: `SELECT count(*) FROM drop_off_points;` and hit
     `GET /api/locations/inbox`.

3. **etit-proxy-rust**
   - Append to `/opt/etit-proxy-rust/.env`:
     ```
     APEX_DATABASE_URL=postgresql://apex:<password>@localhost:5432/apex
     OSRM_URL=http://127.0.0.1:5001
     OSRM_EXCLUDE_CLASS=daytimebanned
     TRIP_SCAN_TIME=04:45
     TRIP_SCAN_LOOKBACK_DAYS=1
     # tanker permit window (defaults shown)
     TANKER_NIGHT_START=20:00
     TANKER_NIGHT_END=07:00
     ```
     Consider a dedicated read-only PG role instead of the apex user:
     ```sql
     CREATE ROLE etit_ro LOGIN PASSWORD '...';
     GRANT CONNECT ON DATABASE apex TO etit_ro;
     GRANT USAGE ON SCHEMA public TO etit_ro;
     GRANT SELECT ON ALL TABLES IN SCHEMA public TO etit_ro;
     ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO etit_ro;
     ```
   - Deploy binary + restart. SQLite migration `20260819000000_trip_audit`
     runs automatically.
   - **Acceptance test** (the "one day before deployment" requirement):
     trigger a scan for yesterday via the API
     (`POST /api/v1/trip-audit/scan {"dates": ["<yesterday>"]}`) and review
     the matches on the dashboard. `trips_matched + trips_unmatched` must
     equal the parent-trip count for that day; spot-check 2–3 matched trips
     (distances plausible vs fee_mappings, geometries render).

4. **apex-react** — deploy the frontend last (its new pages call the APIs
   above).

## Rollback

Each piece is independent: the proxy scanner is disabled by removing
`APEX_DATABASE_URL`; FalconGo's new tables are additive (legacy lat/lng
columns still maintained by the pin-sync); the truck OSRM container can be
stopped without touching the car instance; frontend redeploys are atomic.

## Notes

- The scanner treats legs departing inside the tanker permit window
  (20:00–07:00, permits held by the fleet) as unrestricted; other legs are
  routed with `exclude=daytimebanned` (Cairo Ring Road per decree
  1721/2018; Regional Ring Road never excluded).
- While the car dataset is the only one available (or the truck dataset is
  missing the class), set `OSRM_EXCLUDE_CLASS=` (empty) — the scanner then
  never sends `exclude` and everything still works, just without time-gated
  optimal routing.
