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
Metadata about a tracked unit.
| Field | Type | Description |
| :--- | :--- | :--- |
| `id` | `string` | Unique identifier (UUID). |
| `codename` | `string` | Raw label (e.g., "OL-1/ABC1234"). |
| `plate` | `string` | Extracted plate number. |
| `speedLimit` | `number` | km/h limit for this vehicle. |
| `online` | `boolean` | True if currently connected to ETIT. |
| `speed` | `number` | Last known speed in km/h. |
| `status` | `number` | Raw status integer. |
| `statusLabel` | `string` | Friendly label (e.g., "Moving"). |
| `lastLocationAt` | `string?` | ISO-8601 timestamp of last fix. |

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



## 5. Trip Audit (`/api/v1/trip-audit/*`)

Populated by the nightly scanner (`TRIP_SCAN_TIME`). All endpoints require the
standard admin JWT like the rest of `/api/v1`.

### 5.1 `GET /trip-audit/matches?from=&to=&status=&company=&flagged=`
One row per audited parent trip. `status` ∈ `matched|partial|unmatched`;
`flagged=true` keeps only trips with at least one flag. Returns
`flag_count`, totals (`actual_km`, `osrm_km`, `expected_km`,
`distance_ratio`, `duration_ratio`), and review state.

### 5.2 `GET /trip-audit/matches/:id`
Match + `legs[]` (per-leg actual vs OSRM, `actual_geometry`/`osrm_geometry`
polyline5, `max_deviation_m`, `night_window`) + `flags[]`
(`excess_distance|excess_duration|unplanned_stop|route_deviation|`
`skipped_delivery|no_return_data|no_terminal_departure`, `details_json`).

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
