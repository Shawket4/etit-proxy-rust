# etit-proxy — Frontend API Reference

> **Audience.** This document is the contract between the etit-proxy-rust backend and any frontend client (React + Google Maps, mobile, dashboards, etc.).

*   **Base URL.** `http(s)://<host>:<port>` — Defaults to port `3000`.
*   **Authentication.** All endpoints except `/health` require a valid JWT with `user_type: "admin_user"`.
*   **API Version.** Stable endpoints live under `/api/v1/...`.
*   **Time Zone.** All timestamps are **UTC, ISO-8601** (e.g. `2026-04-27T05:01:09Z`).

---

## 1. Authentication

The proxy requires a JWT (issued by the main system).

### 1.1 Token Transport
The middleware looks for the token in:
1.  **Cookie**: `jwt`
2.  **Header**: `Authorization: Bearer <token>`

### 1.2 Access Control
- **Standard API**: Requires a valid JWT where `claims.user_type == "admin_user"`.
- **Admin API**: Requires the JWT **AND** an `X-Admin-Token` header matching the server's configuration.

---

## 2. Data Structures

### 2.1 `Vehicle`
Metadata about a tracked unit, plus its last known live state. The metadata
(`id`…`speedLimit`) comes from the daily ETIT vehicle list; the live fields
(`online`…`lng`) are rewritten from every live poll (default each minute), so
they are as current as `/vehicles/live` — and persisted, so they survive a
restart.
| Field | Type | Description |
| :--- | :--- | :--- |
| `id` | `string` | Unique identifier (UUID). |
| `codename` | `string` | Raw label (e.g., "OL-1/ABC1234"). |
| `plate` | `string` | Extracted plate number. |
| `speedLimit` | `number` | km/h limit for this vehicle. |
| `online` | `boolean` | False when the last live status was `12` (Offline). |
| `speed` | `number` | Last known speed in km/h. |
| `status` | `number` | Raw status integer. |
| `statusLabel` | `string` | Friendly label (e.g., "Moving"). |
| `lastLocationAt` | `string?` | ISO-8601 UTC timestamp of last fix. |
| `lat` / `lng` | `number?` | Last known position; absent until the first live poll. |

### 2.2 `LiveVehicleStatus`
Real-time tracking snapshot.
| Field | Type | Description |
| :--- | :--- | :--- |
| `id` | `string` | Vehicle identifier. |
| `plate` | `string?` | Plate number. |
| `lat` / `lng` | `number` | Decimal degrees. |
| `speed` | `number` | Current km/h. |
| `status` | `number` | Raw status code. |
| `statusLabel` | `string` | Friendly label. |
| `timestamp` | `string?` | Time of the GPS fix. |
| `event` | `string?` | Last event (e.g., "Ignition On"). |
| `sensorData` | `string?` | Raw sensor strings from upstream. |

### 2.3 `HistoryPoint`
A single breadcrumb in a playback path.
| Field | Type | Description |
| :--- | :--- | :--- |
| `lat` / `lng` | `number` | Coordinates. |
| `timestamp` | `string?` | Time of this point. |
| `speed` | `number` | km/h at this point. |
| `speedLimit` | `number` | km/h limit at this point. |

### 2.4 `HistoryResponse`
| Field | Type | Description |
| :--- | :--- | :--- |
| `vehicleId` | `string` | Echoes the request. |
| `from` / `to` | `string` | Time window. |
| `points` | `HistoryPoint[]` | Chronological breadcrumbs. |
| `stops` | `Stop[]` | List of calculated stops. |
| `sensors` | `SensorEvent[]` | List of ignition/sensor events. |
| `geometry` | `string` | **Encoded Polyline** (Google precision 5). |
| `cached` | `boolean` | True if served from SQLite. |
| `fetchedAt` | `string` | When data was last pulled from ETIT. |

### 2.5 `Stop`
| Field | Type | Description |
| :--- | :--- | :--- |
| `lat` / `lng` | `number` | Stop location. |
| `from` / `to` | `string` | Start/End of stop. |
| `duration` | `string` | Friendly duration (HH:MM:SS). |
| `address` | `string` | Reverse-geocoded location. |

### 2.6 `TripSummary`
| Field | Type | Description |
| :--- | :--- | :--- |
| `totalMileage` | `string` | Distance (km). |
| `totalActiveTime` | `string` | Moving duration. |
| `totalIdleTime` | `string` | Engine on, speed 0 duration. |
| `driverName` | `string` | Name of driver. |
| `numberOfStops` | `string` | Count of stops. |
| `totalFuelConsumption` | `string` | Estimated fuel used. |
| `ignitionOnCount` | `string` | Count of engine starts. |

---

## 3. Endpoints

### 3.1 `GET /api/v1/vehicles`
List every vehicle the proxy is tracking.
- **Returns**: `Vehicle[]`

### 3.2 `GET /api/v1/vehicles/live`
Get a snapshot of the current status/position for all vehicles.
- **Returns**: `LiveVehicleStatus[]`

### 3.3 `GET /api/v1/vehicles/:id`
Get metadata for a single vehicle.
- **Returns**: `Vehicle` (or `404`)

### 3.4 `GET /api/v1/vehicles/:id/live`
Get the current status/position for a single vehicle.
- **Returns**: `LiveVehicleStatus` (or `404`)

### 3.5 `GET /api/v1/vehicles/:id/history`
Fetch playback path, stops, and events for a vehicle.
- **Query Params**: 
    - `date`: `YYYY-MM-DD` (Local day)
    - `from` / `to`: ISO timestamps (Window)
    - `refresh`: `boolean` (Force upstream reload)
- **Returns**: `HistoryResponse`

### 3.6 `GET /api/v1/vehicles/:id/history/summary`
Fetch high-level statistics for a vehicle's movement over a period.
- **Query Params**: Same as 3.5.
- **Returns**: `TripSummary`

### 3.7 `GET /api/v1/stream/live` (SSE)
Real-time stream of status updates.
- **Events**: `snapshot`, `update`, `lag`.
- **Payload**: `{"count": number, "vehicles": LiveVehicleStatus[]}`
- **Event `lag`**: `{"missed": number}` if client is too slow.

---

## 4. Admin API (`/admin`)

Requires `X-Admin-Token` header.

### 4.1 `POST /admin/cache/invalidate`
- **Body**: `{"scope": "all" | "vehicles" | "live" | "history"}`
- **Returns**: `{"scope": string, "ok": bool, "message": string}`

### 4.2 `POST /admin/auth/relogin`
- **Returns**: `{"ok": bool, "epoch": number}`

### 4.3 `POST /admin/users/unblock`
- Unblocks the hardcoded first-row user (`magda.adly`) by replaying the
  legacy `UserList.aspx` WebForms postback against the **login** host.
- **Body**: none.
- **Returns**:
  ```json
  {
    "ok": true,
    "target": "magda.adly",
    "httpStatus": 200,
    "isUsersGrid": true,
    "errorPage": false,
    "accessDenied": false,
    "upstreamMessage": "…",   // omitted when none
    "note": "users grid re-rendered with no rejection signals; event accepted…",
    "bodyExcerpt": "…"        // omitted on a clean grid render
  }
  ```
- **Interpreting the response**:
  - `ok: true` → server accepted the event with no rejection signals. The grid
    markup has **no blocked flag**, so this is "accepted", not hard proof the
    user is unblocked.
  - `ok: false` + `accessDenied: true` → the acting account lacks rights on
    UserList; grant it `Full Control` on the ETIT side.
  - `ok: false` + `errorPage: true` → upstream faulted; see `upstreamMessage` /
    `bodyExcerpt`.
- **Notes**: idempotent. Target row index (`ctl04`) is fixed in
  `upstream::users::UNBLOCK_TARGET`; update it if the grid's row order changes.



### 4.4 `POST /admin/routing/route`

The one place any service asks "how far is A from B by road" (FalconGo fee
mappings and trip summaries, the advisor). Routed by Valhalla for the
configured tanker (`TRUCK_*`), time bans included. Valhalla is asked for its
time-based and its shortest route; the shortest is returned when its time is
at most 1.15 × the time-based route's (docs/valhalla.md). `/optimize` below is
time-based only.

```jsonc
// request
{ "from_lat": 30.146, "from_lng": 31.286, "to_lat": 30.7865, "to_lng": 31.0004,
  "via": [[30.4, 31.1]],                     // optional, in order
  "depart_at": "2026-11-28T13:00:00+02:00",  // optional, RFC 3339
  "restricted": true }                       // used only without depart_at
// response
{ "distanceKm": 87.9, "durationMin": 74.2, "geometry": "<polyline5>",
  "restricted": true, "departure": "2026-11-28T13:00", "engine": "valhalla" }
```

Without `depart_at`, `restricted: true` (the default) plans a route that is
legal at any hour: it leaves inside the ban windows, so every time-banned road
is avoided. That is what a stored reference distance needs. `restricted:
false` means leaving now. `departure` is `"any_hour"` or the local time used.

### 4.5 `POST /admin/routing/optimize`

Visit several stops in the best order (Valhalla `optimized_route`, truck,
minimising time).

```jsonc
// request: end defaults to start (a round trip)
{ "start": {"lat": 30.146, "lng": 31.286},
  "stops": [{"lat": 30.7865, "lng": 31.0004}, {"lat": 30.466, "lng": 31.1858}],
  "end": null, "depart_at": null, "restricted": true }
// response
{ "order": [1, 0],                            // indices into stops
  "legs": [{"distanceKm": 43.1, "durationMin": 32.0, "geometry": "<polyline5>"}, …],
  "distanceKm": 227.2, "durationMin": 186.0, "geometry": "<polyline5, whole trip>",
  "departure": "any_hour", "engine": "valhalla" }
```

## 5. Trip Audit (`/api/v1/trip-audit/*`)

Populated by the nightly scanner (`TRIP_SCAN_TIME`). All endpoints require the
standard admin JWT like the rest of `/api/v1`.

### 5.1 `GET /trip-audit/matches?from=&to=&status=&company=&flagged=`
One row per audited parent trip. `status` ∈ `matched|partial|unmatched`;
`flagged=true` keeps only trips with at least one flag. Returns
`flag_count`, totals (`actual_km`, `osrm_km`, `expected_km`,
`distance_ratio`, `duration_ratio`), and review state.

### 5.2 `GET /trip-audit/matches/:id`
Match + `legs[]` (per-leg actual vs best truck route, `actual_geometry`/`osrm_geometry`
polyline5, `max_deviation_m`, `night_window`) + `flags[]`
(`excess_distance|excess_duration|unplanned_stop|route_deviation|`
`skipped_delivery|no_return_data|no_terminal_departure|banned_road|`
`suboptimal_order|ineffective_bundling`, `details_json`).

**Routing.** `osrm_*` fields are legacy names: since 2026-11-27 they hold
Valhalla truck-route figures (see `docs/valhalla.md`). Each leg is routed for
a truck leaving at that leg's real departure time, so time-of-day bans apply
as they did when it was driven.

**`banned_road`** (critical): the leg, map-matched to OSM ways, drove a road
closed to the fleet (`valhalla/time-bans.json`) while it was closed. One flag
per continuous stretch; `details_json`:
`{ leg_seq, ban_id, road, road_ar, always, banned_hours, from_ts, to_ts (epoch s), km, roads: [names] }`.
`always: true` (26th of July, the Ahmed Hamdi tunnels) means closed at every
hour and `banned_hours` is null; a timed ban has `always: false` and
`banned_hours: "07:00-20:00"` (Cairo local).

**`suboptimal_order`** / **`ineffective_bundling`** come from one truck
distance matrix over terminal + drops (2–7 drops), with real one-way returns.

### 5.3 `POST /trip-audit/matches/:id/review` — body `{ "note": "..."? }`
Marks the trip reviewed (sets `reviewed_at`).

### 5.4 `POST /trip-audit/scan` — body `{ "dates": ["YYYY-MM-DD"]? }`
Runs a scan now (defaults to the lookback window). Returns the summary.

### 5.5 `GET /trip-audit/runs`
Last 30 scan runs with counters.

### 5.6 `GET /trip-audit/suggestions?status=pending`
GPS-derived pin suggestions (`kind` ∈ `terminal|dropoff`): suggested point,
current pin (if any), `offset_m`, `stop_count`.

### 5.7 `POST /trip-audit/suggestions/:id/ack` — body `{ "status": "accepted"|"dismissed" }`
Records the inbox decision (the pin itself is written via the FalconGo
locations API).

## 6. Dead Zones (`/api/v1/zones`)

Circular geofences the fleet is scanned against. CRUD is
`GET /zones?active=`, `GET|PUT|DELETE /zones/:id`, `POST /zones`
(`{ name, lat, lng, radius_m, active? }`); `DELETE` is a soft delete
(`active = 0`).

### 6.1 `POST /zones/scan` — body `{ "from": "YYYY-MM-DD", "to": "YYYY-MM-DD" }?`

Two modes, and the body is what picks between them:

* **No body (or `{}`) — scheduled.** The nightly round: scans the configured
  lookback (`ZONE_LOOKBACK_DAYS`), records each visit in `zone_visits`, and
  **sends a WhatsApp alert** for every visit not alerted on yet.
* **`from` + `to` — manual.** Scans those inclusive local days and **returns a
  report**: it writes no visit rows and sends nothing to the WhatsApp group, so
  it is safe to re-run over the same range while investigating.

Both ends must be given together, `to` must not precede `from`, and the range
may span at most 92 days — anything else is a `400`, never a silent fallback to
the alerting round.

Response (`ScanSummary`):

```jsonc
{
  "run_id": 41,
  "status": "success",            // or "error"
  "mode": "manual",               // or "scheduled"
  "from": "2026-09-01",           // days actually covered, inclusive
  "to": "2026-09-07",
  "days_scanned": 7,
  "vehicles_scanned": 38,
  "violations_found": 3,
  "alerts_sent": 0,               // always 0 for "manual"
  "violations": [                 // populated for "manual" only
    {
      "vehicle_id": "12345",
      "codename": "TRK-14",
      "zone_id": 2,
      "zone_name": "Cairo depot",
      "zone_lat": 30.0444,
      "zone_lng": 31.2357,
      "entry_local": "2026-09-03 01:12:40",  // org timezone
      "exit_local": "2026-09-03 03:41:02",
      "dwell_secs": 8902,
      "point_count": 74
    }
  ],
  "error": null
}
```

## 7. Service API (`/service`)

Read-only lookups for other Apex services. Unlike `/admin/*` this surface
has a stable contract, because other services depend on it.

- **Auth:** an `X-Service-Token` header matching `SERVICE_READ_TOKEN`. No
  JWT. When `SERVICE_READ_TOKEN` is unset, every `/service/*` route answers
  `404`. A wrong or missing token gets `401`.
- **Separate key on purpose:** `SERVICE_READ_TOKEN` is not `ADMIN_TOKEN`, so
  a reader cannot rescan, clear caches or force an ETIT re-login.
- Nothing under `/service` writes anything.

### 7.1 `GET /service/trip-audit/driven-km?terminal=<name>&drop_off=<name>`

How far our trucks actually drove one terminal → drop-off route. The fee
lookup (falcon-fees) shows it next to the route's recorded km and truck-route km.

**Source:** the trip audit's stored legs. A leg counts when it is a trip's
**first** leg (`seq = 0`), is `outbound`, and ends at `drop_off`. That is one
uninterrupted drive from the terminal geofence exit to the drop-off
geofence entry. Excluded:

- `between` legs, which start at the previous drop, not at the terminal.
- `return` legs, which run the other way. One-way roads and time-of-day
  bans make the reverse a different route.
- Trips with a garage dwell on the way. The scan splits that outbound leg
  at the garage, so no single leg is the route.

**Matching names:** `terminal` matches the trip's own terminal text (the
spelling `fee_mappings` uses) or the canonical terminal name, so a trip that
names its terminal by an alias still counts. Names compare trimmed and
case-insensitively. The route is physical, so trips of every company count.

A leg starts and ends at geofence edges, so it reads a few hundred metres
shorter than a gate-to-gate distance. That only matters on very short
routes.

```jsonc
{
  "terminal": "Alexandria",
  "drop_off": "Tanta",
  "trips": 18,                 // usable legs; 0 when none
  "median_km": 124.3,          // the middle trip
  "p10_km": 118.0,             // nearest-rank 10th / 90th percentile:
  "p90_km": 131.2,             //   "most trips"; equals min / max with few trips
  "road_matched": 15,          // of `trips`, how many were map-matched to roads
                               //   (the rest are chord sums of GPS fixes)
  "first_day": "2026-08-20",   // local trip days, YYYY-MM-DD
  "latest_day": "2026-11-25"
}
```

Every field is always present. With `trips: 0` the km and day fields are
`null`. `400` if `terminal` or `drop_off` is missing or blank.

### 7.2 `GET /service/trip-audit/routes?terminal=&drop_off=&from_lat=&from_lng=&to_lat=&to_lng=`

Two drives of one route for the fee lookup's map, both on the road network:

- **`truck_route`**: Valhalla's best route for the configured tanker between
  the two pins: its shortest route unless that is over 1.15 × slower than its
  time-based one (as §4.4). It must be legal at any hour, so closed roads are
  avoided.
  It is `null` when no pins are sent or Valhalla cannot route.
- **`best_trip`**: the shortest **whole** drive our trucks made. The
  candidates are the route's road-matched outbound legs (the same legs as
  §7.1, minus chord sums), and the shape is Valhalla's map match of the
  truck's GPS. A reading is not whole when:
  - it is shorter than `min_share` (0.8) of the shorter of the truck route
    and the 75th-percentile trip; or
  - its shape has a straight segment over 5 km (a GPS gap bridged by a
    line); or
  - with pins sent, its shape starts or ends more than 10 km from them (the
    tracker came online part-way).

  Readings shorter than the best that fail any of these are counted in
  `set_aside`. `best_trip` is `null` when no whole drive is found.

The pins are optional, but send all four or none (`400` otherwise).

**`roads`** (on both routes): the major roads the route takes, in driving
order, for a turn-by-turn-like list (`src/routing/roads.rs`). Valhalla's
`trace_attributes` walks the route's own shape edge by edge (`map_snap` if
the walk breaks), and one code path summarises both routes:

- consecutive edges of one road join: the same name (Arabic spelling
  variants folded), or a shared ref where one side has no name. Differently
  named sections of one ref stay apart (the coast road 11 is four sections);
- ramps, turn channels, roundabouts and junctions, and any stretch shorter
  than 2% of the route (at least 3 km, at most 10 km), are folded into the
  road before them, or after them at the very start; their km go with them,
  so the list adds up to the route;
- local streets (residential, unclassified, service) form one stretch when
  long enough, with `name` and `ref` null.

Each stretch:

| field | |
|---|---|
| `name` | OSM name as mapped (Arabic in Egypt; this Valhalla build serves no `name:en`). `null` for an unnamed road and for local streets |
| `ref` | road number (`"11"`, `"244"`), or `null` |
| `road_class` | Valhalla's class for most of the stretch: `motorway`, `trunk`, `primary`, `secondary`, `tertiary`, `unclassified`, `residential`, `service_other` |
| `km` | including what it absorbed |
| `from_index`, `to_index` | first and last vertex of the route's `geometry` on this stretch; the next stretch starts on this one's last vertex, the first starts at 0 and the last ends on the last vertex |

`roads` is `null` when Valhalla could not read the route (the route itself is
still sent).

**`road_difference`**: what each route drives much more of than the other,
road by road. A road is every stretch, on either route, sharing a ref or a
name; unnamed roads and local streets cannot be matched and are left out.
"Much more" is at least 3 km and a quarter of the road's longer share.
`best_more` / `truck_more` are sorted by the extra km; each entry has
`name` (`null` for a numbered road whose sections carry several names),
`ref`, `best_km` and `truck_km`. `null` unless both routes have `roads`.

```jsonc
{
  "terminal": "Somed", "drop_off": "Qena",
  "truck_route": { "distance_km": 695.4, "duration_min": 505.7,
                   "geometry": "<polyline5>", "departure": "any_hour",
                   "roads": [
                     { "name": "طريق السويس, العين السخنه الجانبي", "ref": "11", "road_class": "primary",
                       "km": 18.2, "from_index": 0, "to_index": 282 },
                     { "name": "طريق العين السخنه", "ref": null, "road_class": "trunk",
                       "km": 36.9, "from_index": 282, "to_index": 882 },
                     // …
                     { "name": "طريق اسيوط, قنا الزراعي", "ref": "25", "road_class": "trunk",
                       "km": 144.9, "from_index": 6142, "to_index": 8030 } ] },
  "best_trip":   { "km": 541.4, "drive_min": 1568.1,   // geofence exit to entry, stops included
                   "day": "2026-11-04", "depart_ts": "2026-11-04T14:32:55Z",
                   "arrive_ts": "2026-11-05T16:41:03Z", "geometry": "<polyline5>",
                   "roads": [
                     { "name": "طريق العين السخنه, الزعفرانه", "ref": "11", "road_class": "trunk",
                       "km": 65.8, "from_index": 0, "to_index": 1127 },
                     // … two more sections of 11 …
                     { "name": "طريق الغردقة, سفاجا", "ref": "11", "road_class": "trunk",
                       "km": 54.2, "from_index": 2684, "to_index": 3221 },
                     { "name": "طريق قنا, سفاجا", "ref": "244", "road_class": "trunk",
                       "km": 168.3, "from_index": 3221, "to_index": 6019 } ] },
  "trips_considered": 583, "set_aside": 199, "min_share": 0.8,
  "road_difference": {
    "best_more":  [ { "name": null, "ref": "11", "best_km": 373.7, "truck_km": 18.2 },
                    { "name": "طريق قنا, سفاجا", "ref": "244", "best_km": 168.3, "truck_km": 0.0 } ],
    "truck_more": [ { "name": "طريق اسيوط, قنا الزراعي", "ref": "25", "best_km": 0.0, "truck_km": 144.9 },
                    { "name": "طريق المنيا, الواسطى", "ref": null, "best_km": 0.0, "truck_km": 127.6 } ]
  }
}
```
