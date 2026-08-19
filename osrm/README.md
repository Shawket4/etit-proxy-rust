# OSRM truck profile (Egypt fuel-tanker fleet)

Custom OSRM profile and build/test tooling for routing 40 t fuel tankers
(4.0 m high, 2.6 m wide, 16.5 m long) across Egypt.

## Files

| File | Purpose |
|---|---|
| `truck.lua` | OSRM profile (api_version 4, based on car.lua). Truck dimensions, `hgv`/`truck` access handling, height/weight/width/length limits, 26th July corridor hard-blocked, Cairo Ring Road tagged with the excludable `daytimebanned` class. |
| `build.sh`  | Reproducible dataset build (docker, MLD pipeline). Produces `egypt-truck.osrm*`. |
| `test.sh`   | Curl test suite against a running instance. `OSRM_URL=http://host:port ./test.sh`; exits non-zero on failure. |

## The `daytimebanned` class

> Naming note: OSRM only accepts **alphanumeric** class names
> (`isValidClassName` rejects `_`), so the class is `daytimebanned`,
> not `daytime_banned`. The app must send `exclude=daytimebanned`.

Cairo Ring Road (الطريق الدائري, ref 516): heavy trucks >= 4 t are banned
06:00–24:00 (PM decree 1721/2018) and allowed 00:00–06:00. OSRM has no
time-dependent routing, so the profile tags every Cairo Ring Road way with an
excludable class instead, and the **application layer decides per request**:

- During the ban window (06:00–24:00): append `exclude=daytimebanned` to the
  `/route/v1/driving/...` query. Routes then divert (typically via the
  Regional Ring Road).
- 00:00–06:00, or for the petroleum-permit window logic (permits allow
  20:00–07:00 inside Cairo): query without the exclude. Combining the decree
  window with the permit window is application policy, not profile logic.

The Regional Ring Road (الطريق الدائري الإقليمي, ref 515) is the designated
24/7 truck bypass and is **never** tagged — the profile explicitly excludes
"الإقليمي"/"Regional" (and Middle Ring Road / other cities' ring roads) from
the match. Matching is by name/ref only: Lua profiles cannot see way geometry,
so there is no bounding-box filter; see the comment block in `truck.lua` for
the exact rules verified against the live Egypt extract.

## Rebuilding the dataset

Requires docker (same image as the VPS: `osrm/osrm-backend:latest`) and
roughly 6–8 GB of RAM available to the docker daemon.

```sh
cd osrm/
./build.sh                       # downloads egypt-latest.osm.pbf, builds into ./osrm-build/
BUILD_DIR=/opt/osrm ./build.sh   # build straight into a deployment dir
```

The pipeline is `osrm-extract -p truck.lua` → `osrm-partition` →
`osrm-customize` (MLD; the VPS runs `--algorithm mld`). Output files are named
`egypt-truck.osrm*` so they can sit next to the car dataset. Rebuild whenever
`truck.lua` changes or to pick up fresh OSM data (monthly is plenty).

## VPS deployment notes

1. Build (on the VPS or locally, then rsync) and place `egypt-truck.osrm*`
   in `/opt/osrm/`.
2. Serving — two options, **choice not yet decided**:
   - **Second service (recommended for rollout):** run a second `osrm-routed`
     container on another port alongside the existing car instance, e.g.
     ```sh
     docker run -d --name osrm-truck --restart unless-stopped \
       -p 5001:5000 -v /opt/osrm:/data osrm/osrm-backend:latest \
       osrm-routed --algorithm mld --max-table-size 8000 /data/egypt-truck.osrm
     ```
     The proxy then targets the car instance or the truck instance per
     vehicle type.
   - **Replace the car dataset:** point the existing service at
     `egypt-truck.osrm` instead. Simpler, but every query gets truck
     restrictions — only do this if the fleet is trucks-only.
3. Verify after deploy: `OSRM_URL=http://localhost:5001 ./test.sh`.

## Notes / limitations

- `exclude=daytimebanned` on a query that starts or ends *on* the Ring Road
  itself returns `NoRoute`/`NoSegment` for that leg — expected.
- An unknown exclude class makes OSRM return HTTP 400; `test.sh` checks both
  directions.
- The 26th July corridor is hard-blocked for trucks in the profile (not
  excludable) — tankers are never allowed there.
