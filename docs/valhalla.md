# Routing: Valhalla

All road routing in Apex goes through etit-proxy, and etit-proxy routes with
**Valhalla**, planned for our tanker. That covers:
- the trip audit;
- `/admin/routing/route` and `/admin/routing/optimize`, used by FalconGo's fee
  mappings, FalconGo's trip summaries and the advisor;
- `/service/*`, used by the fee lookup.

Valhalla replaced OSRM on 2026-11-27. Madar's car OSRM on `:5000` is a
separate product and untouched.

## What it does better than the OSRM setup

| | OSRM (before) | Valhalla (now) |
|---|---|---|
| Truck rules | hand-written Lua profile, rebuild to change | `truck` costing per request: 4.0 m × 2.6 m × 16.5 m, 40 t, 10 t/axle, 5 axles, **hazmat**, 90 km/h cap; `TRUCK_*` env, no rebuild |
| Closed roads | an `exclude` class the app toggled from a night-window setting | in the data: `hgv=no` for a road closed at every hour, `hgv:conditional` for one closed at some hours (every request carries the real departure time, so Valhalla applies those itself) |
| Which roads a ban covers | any name *containing* "26 يوليو", incl. side streets and other towns | name **and** road class **and** any required tags (`tunnel=yes`) **and** a box (`valhalla/time-bans.json`); the exact way list ships with each build |
| Driving a banned road | invisible | the audit matches each leg to OSM ways with times and raises `banned_road` |
| Best drop order | up to 20 route calls, ≤ 4 drops, returns approximated as symmetric | one distance matrix, ≤ 7 drops, real one-way returns |
| Trip optimiser (FalconGo summaries) | Madar's **car** OSRM `/trip` | `/admin/routing/optimize` (Valhalla `optimized_route`, truck) |
| Data size on the VPS | ~3.2 GB, loaded into RAM | ~830 MB, memory-mapped (≈50 MB RSS idle) |

## Which route: shortest unless much slower

`RoutingClient::route` asks Valhalla for two routes at once, both with the
tanker's costing and the closed roads:
- its time-based route;
- its shortest route (`shortest: true`).

It uses the shortest one when that one's driving time is at most **1.15 ×**
the time-based route's; otherwise the time-based one (owner rule,
2026-11-27, `SHORTEST_MAX_TIME_RATIO`).

Why: on long trips Valhalla's time-based search can miss a road that is both
shorter and quicker. Somed → Qena came out 695 km inland, via the Regional
Ring Road and the Cairo–Assiut desert road, while our trucks drive the Red
Sea coast at about 541 km. Access, tags and penalties were all ruled out:
the long-route search simply missed the coast road, and `shortest: true`
found it (542.9 km, quicker too). On short city trips the shortest route can
be a slow crawl through town; the 15% cap keeps the faster road there (e.g.
Mostorod → 6th of October stays on the Ring Road).

This covers everything that calls `route`: the trip audit's per-leg optimal
routes, `/admin/routing/route` (FalconGo fee mappings and trip summaries,
the advisor) and the fee lookup's truck route.

**Not changed:** the distance matrix (`sources_to_targets`, the drop-order
check) and `optimized_route` (`/admin/routing/optimize`) still use Valhalla's
time-based costing alone. Valhalla has no "prefer shortest within a time
budget" for those, and asking twice would double a matrix's cost.

Stored figures computed before the change keep the old route until
recomputed: rescan the trip audit, and re-run FalconGo's fee-mapping enrich.

## Changing a rule

- **Truck size, weight or speed cap:** set `TRUCK_*` in etit-proxy's
  `.env` and restart it. No rebuild.
- **A closed road** (add one, change hours, widen a corridor):
  1. Edit `valhalla/time-bans.json`: `"always": true` for closed at every
     hour, or `banned_from`/`banned_to` (Cairo local) for closed at some.
  2. Build and deploy. The build stops if a rule catches no ways.
  3. Read the build's printed list, or `apex-time-bans.json`, to check what
     the rule caught.

The rules today (owner, 2026-11-27), all closed at every hour:

| Ban | Caught |
|---|---|
| `26th-of-july` | 84 ways, 92 km: the corridor (59.7 km), its service road الجانبى (23.1 km), downtown شارع 26 يوليو (9.2 km) |
| `ahmed-hamdi-tunnel` | 3 ways, 7 km: both directions of the original tunnel and the northern tunnel (Ahmed Hamdi 2); the approach roads stay open |

The Cairo Ring Road is open at all hours. With the tunnels closed, trucks for
South Sinai cross further north (Suez → Ras Sudr goes from 84 km to the
detour `test.sh` prints).

## English road names

Valhalla serves each road under its default-language name, which in Egypt is
the Arabic `name`, and has no request option for `name:en` (3.9.3, tested
2026-11-27). The fee lookup's road lists want the English name in the English
UI, and OpenStreetMap has one on most major roads. So the build exports them:

- `valhalla/export_road_names.py` writes `apex-road-names.json`: `name:en` by
  way id, for every motorway–tertiary way (and link) that has one (about
  26,000 ways, 1 MB). It reads the same extract the tiles come from, so the way
  ids match the edges Valhalla returns. `build.sh` runs it; `deploy.sh` ships
  and checksums it; `test.sh` checks it reads and covers a real route.
- etit-proxy reads it from `VALHALLA_ROAD_NAMES`
  (`/opt/valhalla/current/apex-road-names.json`), looks each road-list edge's
  way id up, and adds `name_en` to the stretch. It re-reads the file when it
  changes, so a Valhalla deploy needs no etit-proxy restart. Missing file:
  Arabic names only, nothing fails.

To refresh the names alone (same extract, tiles unchanged):

```sh
~/work/valhalla-venv/bin/python valhalla/export_road_names.py \
  ~/work/valhalla-build/egypt-latest.osm.pbf ~/work/valhalla-build/out/apex-road-names.json
```

then `deploy.sh` as below (unchanged files are hard-linked on the VPS).

## Build (on a workstation, never the VPS)

```sh
python3 -m venv ~/work/valhalla-venv && ~/work/valhalla-venv/bin/pip install osmium
cd valhalla/
DOCKER="sudo docker" PYTHON=~/work/valhalla-venv/bin/python \
  BUILD_DIR=~/work/valhalla-build ./build.sh          # FRESH=1 re-downloads the extract
```

The build takes about 5 minutes. Everything it produces lands in
`$BUILD_DIR/out/`. `BUILD_INFO` pins the image digest that built the tiles,
and `deploy.sh` runs exactly that digest on the VPS.

**Local check:**

```sh
sudo docker run -d --rm --name apex-valhalla-local -p 127.0.0.1:18002:8002 \
  -v ~/work/valhalla-build/out:/data <digest from BUILD_INFO> valhalla_service /data/valhalla.json 2
VALHALLA_URL=http://127.0.0.1:18002 ./test.sh ~/work/valhalla-build/out/apex-time-bans.json \
  ~/work/valhalla-build/out/apex-road-names.json
VALHALLA_URL=http://127.0.0.1:18002 VALHALLA_TIME_BANS=~/work/valhalla-build/out/apex-time-bans.json \
  VALHALLA_ROAD_NAMES=~/work/valhalla-build/out/apex-road-names.json \
  cargo test --test valhalla_live -- --nocapture
```

## Deploy

```sh
VPS=root@apextransport.ddns.net SSH_KEY=~/.ssh/apex_vps_osrm \
  BUILD_DIR=~/work/valhalla-build valhalla/deploy.sh
```

See the header of `valhalla/deploy.sh` for the exact steps. In short, it
touches nothing live until the new release has passed `test.sh` in a
throwaway container. It then flips `/opt/valhalla/current` and restarts
`apex-valhalla` on `127.0.0.1:8002`. If the live test fails, it flips back.

`valhalla/deploy.sh --rollback` points `current` at the previous release.

## Cutover from OSRM (once)

1. **Deploy Valhalla** with `deploy.sh`. The first run creates the
   container.
2. **etit-proxy `.env`:**
   - remove `OSRM_URL` and `OSRM_EXCLUDE_CLASS`;
   - add:
     ```
     VALHALLA_URL=http://127.0.0.1:8002
     VALHALLA_TIME_BANS=/opt/valhalla/current/apex-time-bans.json
     VALHALLA_ROAD_NAMES=/opt/valhalla/current/apex-road-names.json
     ```
   - then deploy etit-proxy.
3. **FalconGo:** deploy the release whose trip summaries call
   `/admin/routing/optimize`. It needs the same `ETIT_PROXY_ADMIN_URL` and
   token it already has.
4. **Recompute old numbers.** Stored figures keep the OSRM numbers they
   were computed with until recomputed:
   - **Trip audit:** `POST /admin/trip-audit/scan` with the `dates` to
     rescan (e.g. the last 60 days).
   - **Fee mappings:** `POST /api/mappings/enrich-osrm` in FalconGo
     (permission 3) re-routes every mapping through Valhalla.
5. **Remove the old truck OSRM.** Once the above has run cleanly for a
   while, remove the `osrm-truck` service from
   `/opt/madar-rust/docker-compose.yml` and delete `/opt/osrm/egypt-truck.osrm*`.
   That frees about 3 GB. The car instance on `:5000` stays: it is
   Madar's.

## Legacy names

Stored columns and API fields keep their pre-switch names, so no repo needs
a lock-step deploy:
- `trip_matches.osrm_km` / `osrm_secs`, `trip_match_legs.osrm_km` /
  `osrm_secs` / `osrm_geometry`;
- FalconGo `fee_mappings.osrm_distance` / `osrm_duration`;
- `POST /api/mappings/enrich-osrm`.

They hold Valhalla truck-route figures now. The UIs label them "truck route".

## Known gaps

- OpenStreetMap does not tag the Ahmed Hamdi tunnels `hazmat=no`, so
  `hazmat` alone would not keep tankers out of them; the `ahmed-hamdi-tunnel`
  rule does.
- Valhalla knows no other Egyptian truck bans than those in OpenStreetMap
  and `time-bans.json`.
