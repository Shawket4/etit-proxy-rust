# etit-proxy v2

High-performance Rust middleware that bridges a React/Google Maps frontend
to the legacy ETIT ASP.NET fleet-tracking system.

## What changed in v2

| Area | v1 | v2 |
|------|----|----|
| Persistence | None — caches rebuilt on every restart | SQLite via sqlx — vehicles + history persisted across restarts |
| Hot/cold history | Two `DashMap`s, manual graduation | Single moka cache with capacity-based LRU + `is_hot` flag in DB |
| Vehicle/live snapshots | `RwLock<...>` | `ArcSwap<...>` — lock-free reads, atomic writes |
| Tier C concurrency | Sequential, all vehicles | Bounded fan-out via semaphore (`TIER_C_CONCURRENCY`, default 4) |
| Thundering herd | Possible | `SingleFlight` collapses concurrent identical history requests |
| Live updates | Polling only | SSE stream at `/api/v1/stream/live` |
| Timestamps | Inconsistent (`DD/MM/YYYY hh:mm:ss AM/PM`) | UTC ISO-8601 / RFC3339 everywhere |
| API versioning | None | `/api/v1/*` for stable contract |
| Retries | None | Exponential backoff with `UPSTREAM_RETRY_MAX` |
| Request tracing | None | UUID `X-Request-Id` per request, scoped tracing span |
| Error surfacing | Free-text only | Stable `code` field for frontend switching |
| Polyline encoding | Recomputed every read | Precomputed on store, read straight from DB |
| Admin ops | Not exposed | `POST /admin/cache/invalidate`, `POST /admin/auth/relogin` |
| Live parser | Hard-coded column 9/10/11 | Scans every column past 5, picks event by `\d+#.+` shape |
| Dead code | Several unused vars in services | Removed |

## Architecture

```
                       ┌──────────────┐
                       │   Frontend   │  (React / Google Maps)
                       └─────┬────────┘
                             │ /api/v1/*    /api/v1/stream/live (SSE)
                             ▼
┌────────────────────────────────────────────────────────────┐
│                       etit-proxy                           │
│  ┌────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────┐  │
│  │  api   │→ │ services │→ │  cache   │  │   pollers    │  │
│  │handlers│  │ orchestr.│  │  moka +  │  │  Tier A/B/C  │  │
│  └────────┘  └─────┬────┘  │ ArcSwap  │  └──────┬───────┘  │
│                    │       └────┬─────┘         │          │
│                    ▼            ▼               │          │
│              ┌──────────┐  ┌────────┐           │          │
│              │ upstream │  │   db   │           │          │
│              │ EtitClnt │  │ sqlx   │           │          │
│              └────┬─────┘  └────────┘           │          │
│                   │                             │          │
└───────────────────┼─────────────────────────────┼──────────┘
                    │                             │
                    ▼                             │
              ┌──────────┐                        │
              │   ETIT   │←───────────────────────┘
              │ ASP.NET  │
              └──────────┘
```

Reads flow `handler → service → cache → DB → upstream` with each step
fronting the next. Writes flow back through the same path, populating
both the in-memory cache and SQLite.

## Quickstart

```bash
cp .env.example .env
# fill in ETIT_*, then:
cargo run --release
```

The server creates `./data/etit.db` on first run and is ready as soon as
the port is bound. The first ETIT login is best-effort — pollers will
retry on the configured cadence.

## Endpoints

See [`API.md`](./API.md) for the full reference. The short version:

- `GET  /api/v1/vehicles`               — vehicle metadata (Tier A)
- `GET  /api/v1/vehicles/:id`           — one vehicle
- `GET  /api/v1/vehicles/live`          — current live status for all (Tier B)
- `GET  /api/v1/vehicles/:id/live`      — current live status for one
- `GET  /api/v1/vehicles/:id/history`   — `?date=YYYY-MM-DD` or `?from=...&to=...`
- `GET  /api/v1/stream/live`            — SSE: `snapshot`, `update`, `ping`, `lag`
- `GET  /healthz`                       — health + cache stats
- `GET  /metrics`                       — JSON metrics dump
- `POST /admin/cache/invalidate`        — `{"scope": "all|vehicles|live|history"}`
- `POST /admin/auth/relogin`            — force ETIT re-login

Admin routes require an `X-Admin-Token` header that matches `ADMIN_TOKEN`.
If `ADMIN_TOKEN` is unset, all admin routes are 404'd.

## Configuration

All knobs are env vars; see `.env.example` for the full list and
defaults. The most important:

- `ETIT_DATA_URL` / `ETIT_LOGIN_URL` / `ETIT_USERNAME` / `ETIT_PASSWORD` — required.
- `DATABASE_URL` — defaults to `sqlite://./data/etit.db?mode=rwc`.
- `TIMEZONE` — IANA name; defaults to `Africa/Cairo`. The zone of ETIT's
  wall-clock times (live status, history points, stops, sensors) and of the
  day boundaries.
- `ETIT_LAST_LOCATION_TZ` — zone of the vehicle list's `LastLocationDate`,
  which ETIT sends in UTC; defaults to `UTC`. Only a rollback knob.
- `ETIT_SUB_ID` / `ETIT_STAFF_ID` — optional; posted only to
  `GetTransportersPage`. Unset ⇒ scraped from the post-login page, else zeros.
- `TIER_B_POLL_SECS` — live-status refresh interval (default 60).
- `TIER_C_POLL_SECS` — hot-history refresh interval (default 1800).
- `TIER_C_CONCURRENCY` — concurrent upstream history fetches (default 4).
- `ADMIN_TOKEN` — enables `/admin/*`.

## Performance notes

- `ArcSwap` snapshots are lock-free — handlers never wait on a lock to
  read vehicles or live data.
- The history endpoint single-flights duplicate concurrent requests, so
  a dashboard with N vehicles asking for the same day fires one upstream
  call, not N.
- Polylines are encoded *once* on store, not on every read.
- `Cache-Control: no-store` is sent on every API response to prevent
  intermediate proxies from caching stale snapshots.

## Tests

```bash
cargo test
```

Unit tests cover the live parser column-scan, polyline encoding (against
Google's `(38.5, -120.2) → "_p~iF~ps|U"` reference), the unquoted-JSON
fixer, and the timestamp parser variants.

## License

MIT.
