//! Trip-audit persistence (SQLite).
//!
//! Written by the nightly scanner, read by `/api/v1/trip-audit/*`. Re-scans
//! of the same parent trip replace the old match row and everything hanging
//! off it (legs/flags cascade).

use anyhow::Result;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::trips::calibrate::Suggestion;
use crate::trips::{TripScanSummary};

#[derive(Clone)]
pub struct TripAuditRepo {
    pool: SqlitePool,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct MatchSummaryRow {
    pub total: i64,
    pub matched: i64,
    pub partial: i64,
    pub unmatched: i64,
    pub flagged: i64,
    pub critical: i64,
    pub flagged_unreviewed: i64,
    pub actual_km: f64,
    /// Actual km restricted to trips that have an OSRM comparison — the
    /// honest denominator-mate for `osrm_km` when computing efficiency.
    pub actual_km_compared: f64,
    pub osrm_km: f64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct WorstRouteRow {
    pub terminal_name: String,
    pub destinations: Option<String>,
    pub trips: i64,
    pub excess_km: f64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TripMatchRow {
    pub id: i64,
    pub scan_run_id: i64,
    pub parent_trip_id: i64,
    pub day_local: String,
    pub company: String,
    pub terminal_name: String,
    pub car_no_plate: String,
    pub vehicle_id: Option<String>,
    pub driver_name: Option<String>,
    pub status: String,
    pub unmatched_reason: Option<String>,
    pub start_ts: Option<String>,
    pub end_ts: Option<String>,
    pub returned_to_terminal: i64,
    pub deliveries_expected: i64,
    pub deliveries_visited: i64,
    pub actual_km: Option<f64>,
    pub osrm_km: Option<f64>,
    pub expected_km: Option<f64>,
    pub actual_secs: Option<i64>,
    pub osrm_secs: Option<i64>,
    pub distance_ratio: Option<f64>,
    pub duration_ratio: Option<f64>,
    pub reviewed_at: Option<String>,
    pub review_note: Option<String>,
    pub created_at: String,
    /// Filled by list queries: number of flags on this match.
    #[sqlx(default)]
    pub flag_count: i64,
    /// Count of critical-severity flags — drives the queue's severity chip.
    pub critical_count: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TripLegRow {
    pub id: i64,
    pub match_id: i64,
    pub seq: i64,
    pub leg_type: String,
    pub from_name: String,
    pub to_name: String,
    pub depart_ts: String,
    pub arrive_ts: String,
    pub night_window: i64,
    pub actual_km: f64,
    pub osrm_km: Option<f64>,
    pub actual_secs: i64,
    pub osrm_secs: Option<i64>,
    pub distance_ratio: Option<f64>,
    pub max_deviation_m: Option<f64>,
    pub actual_geometry: String,
    pub osrm_geometry: String,
    #[sqlx(default)]
    pub distance_method: String,
    #[sqlx(default)]
    pub off_route_pct: Option<f64>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TripFlagRow {
    pub id: i64,
    pub match_id: i64,
    pub leg_id: Option<i64>,
    pub flag_type: String,
    pub severity: String,
    pub details_json: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SuggestionRow {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub suggested_lat: f64,
    pub suggested_lng: f64,
    pub current_lat: Option<f64>,
    pub current_lng: Option<f64>,
    pub offset_m: Option<f64>,
    pub stop_count: i64,
    pub scan_run_id: i64,
    pub status: String,
    pub updated_at: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct PinLegRow {
    pub parent_trip_id: i64,
    pub seq: i64,
    pub leg_type: String,
    pub from_name: String,
    pub to_name: String,
    pub depart_ts: String,
    pub arrive_ts: String,
    pub actual_geometry: String,
    pub actual_km: f64,
    pub osrm_km: Option<f64>,
    pub actual_secs: i64,
    pub osrm_secs: Option<i64>,
    pub distance_ratio: Option<f64>,
    #[sqlx(default)]
    pub off_route_pct: Option<f64>,
    #[sqlx(default)]
    pub distance_method: String,
    pub osrm_geometry: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TripScanRunRow {
    pub id: i64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub days_scanned: i64,
    pub trips_scanned: i64,
    pub trips_matched: i64,
    pub trips_unmatched: i64,
    pub flags_raised: i64,
    pub suggestions_made: i64,
    pub error: Option<String>,
}

/// Everything needed to persist one scanned trip in a single call.
pub struct NewMatch<'a> {
    pub scan_run_id: i64,
    pub parent_trip_id: i64,
    pub day_local: &'a str,
    pub company: &'a str,
    pub terminal_name: &'a str,
    pub car_no_plate: &'a str,
    pub vehicle_id: Option<&'a str>,
    pub driver_name: Option<&'a str>,
    pub status: &'a str,
    pub unmatched_reason: Option<&'a str>,
    pub start_ts: Option<String>,
    pub end_ts: Option<String>,
    pub returned_to_terminal: bool,
    pub deliveries_expected: i64,
    pub deliveries_visited: i64,
    pub actual_km: Option<f64>,
    pub osrm_km: Option<f64>,
    pub expected_km: Option<f64>,
    pub actual_secs: Option<i64>,
    pub osrm_secs: Option<i64>,
    pub distance_ratio: Option<f64>,
    pub duration_ratio: Option<f64>,
}

pub struct NewLeg<'a> {
    pub seq: i64,
    pub leg_type: &'a str,
    pub from_name: &'a str,
    pub to_name: &'a str,
    pub depart_ts: String,
    pub arrive_ts: String,
    pub night_window: bool,
    pub actual_km: f64,
    pub osrm_km: Option<f64>,
    pub actual_secs: i64,
    pub osrm_secs: Option<i64>,
    pub distance_ratio: Option<f64>,
    pub max_deviation_m: Option<f64>,
    pub actual_geometry: &'a str,
    pub osrm_geometry: &'a str,
    pub distance_method: &'a str,
    pub off_route_pct: Option<f64>,
}

pub struct NewFlag<'a> {
    pub leg_id: Option<i64>,
    pub flag_type: &'a str,
    pub severity: &'a str,
    pub details_json: String,
}

impl TripAuditRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    // ---- scan runs -----------------------------------------------------------

    pub async fn insert_scan_run(&self) -> Result<i64> {
        let res = sqlx::query("INSERT INTO trip_scan_runs DEFAULT VALUES")
            .execute(&self.pool)
            .await?;
        Ok(res.last_insert_rowid())
    }

    pub async fn finalize_scan_run(&self, run_id: i64, s: &TripScanSummary) -> Result<()> {
        sqlx::query(
            r#"UPDATE trip_scan_runs
                  SET finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                      status = ?, days_scanned = ?, trips_scanned = ?,
                      trips_matched = ?, trips_unmatched = ?, flags_raised = ?,
                      suggestions_made = ?, error = ?
                WHERE id = ?"#,
        )
        .bind(&s.status)
        .bind(s.days_scanned)
        .bind(s.trips_scanned)
        .bind(s.trips_matched)
        .bind(s.trips_unmatched)
        .bind(s.flags_raised)
        .bind(s.suggestions_made)
        .bind(s.error.as_deref())
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Matched legs for one vehicle overlapping a UTC window — the raw
    /// material for the history endpoint's significant pins. ISO-8601 UTC
    /// strings compare lexicographically, so BETWEEN-style bounds work
    /// directly on the stored text timestamps.
    pub async fn legs_for_vehicle(
        &self,
        vehicle_id: &str,
        from_iso: &str,
        to_iso: &str,
    ) -> Result<Vec<PinLegRow>> {
        Ok(sqlx::query_as::<_, PinLegRow>(
            "SELECT m.parent_trip_id, l.seq, l.leg_type, l.from_name, l.to_name,                     l.depart_ts, l.arrive_ts, l.actual_geometry, l.actual_km, l.osrm_km, l.actual_secs, l.osrm_secs, l.distance_ratio, l.off_route_pct, l.distance_method, l.osrm_geometry FROM trip_match_legs l              JOIN trip_matches m ON m.id = l.match_id              WHERE m.vehicle_id = ? AND l.arrive_ts >= ? AND l.depart_ts <= ?              ORDER BY m.parent_trip_id, l.seq",
        )
        .bind(vehicle_id)
        .bind(from_iso)
        .bind(to_iso)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn list_runs(&self, limit: i64) -> Result<Vec<TripScanRunRow>> {
        Ok(sqlx::query_as::<_, TripScanRunRow>(
            "SELECT * FROM trip_scan_runs ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    // ---- matches ---------------------------------------------------------------

    /// Replace any previous match for this parent trip and insert the new
    /// one with its legs and flags, atomically.
    pub async fn store_match(
        &self,
        m: &NewMatch<'_>,
        legs: &[NewLeg<'_>],
        flags: &[NewFlag<'_>],
        // flag index → leg position (into `legs`); None = trip-level flag.
        flag_leg_pos: &[Option<usize>],
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM trip_matches WHERE parent_trip_id = ?")
            .bind(m.parent_trip_id)
            .execute(&mut *tx)
            .await?;

        let match_id = sqlx::query(
            r#"INSERT INTO trip_matches
                 (scan_run_id, parent_trip_id, day_local, company, terminal_name,
                  car_no_plate, vehicle_id, driver_name, status, unmatched_reason,
                  start_ts, end_ts, returned_to_terminal, deliveries_expected,
                  deliveries_visited, actual_km, osrm_km, expected_km, actual_secs,
                  osrm_secs, distance_ratio, duration_ratio)
               VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(m.scan_run_id)
        .bind(m.parent_trip_id)
        .bind(m.day_local)
        .bind(m.company)
        .bind(m.terminal_name)
        .bind(m.car_no_plate)
        .bind(m.vehicle_id)
        .bind(m.driver_name)
        .bind(m.status)
        .bind(m.unmatched_reason)
        .bind(&m.start_ts)
        .bind(&m.end_ts)
        .bind(m.returned_to_terminal as i64)
        .bind(m.deliveries_expected)
        .bind(m.deliveries_visited)
        .bind(m.actual_km)
        .bind(m.osrm_km)
        .bind(m.expected_km)
        .bind(m.actual_secs)
        .bind(m.osrm_secs)
        .bind(m.distance_ratio)
        .bind(m.duration_ratio)
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();

        let mut leg_ids = Vec::with_capacity(legs.len());
        for leg in legs {
            let id = sqlx::query(
                r#"INSERT INTO trip_match_legs
                     (match_id, seq, leg_type, from_name, to_name, depart_ts,
                      arrive_ts, night_window, actual_km, osrm_km, actual_secs,
                      osrm_secs, distance_ratio, max_deviation_m,
                      actual_geometry, osrm_geometry, distance_method, off_route_pct)
                   VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
            )
            .bind(match_id)
            .bind(leg.seq)
            .bind(leg.leg_type)
            .bind(leg.from_name)
            .bind(leg.to_name)
            .bind(&leg.depart_ts)
            .bind(&leg.arrive_ts)
            .bind(leg.night_window as i64)
            .bind(leg.actual_km)
            .bind(leg.osrm_km)
            .bind(leg.actual_secs)
            .bind(leg.osrm_secs)
            .bind(leg.distance_ratio)
            .bind(leg.max_deviation_m)
            .bind(leg.actual_geometry)
            .bind(leg.osrm_geometry)
            .bind(leg.distance_method)
            .bind(leg.off_route_pct)
            .execute(&mut *tx)
            .await?
            .last_insert_rowid();
            leg_ids.push(id);
        }

        for (i, flag) in flags.iter().enumerate() {
            let leg_id = flag_leg_pos
                .get(i)
                .copied()
                .flatten()
                .and_then(|pos| leg_ids.get(pos).copied());
            sqlx::query(
                r#"INSERT INTO trip_flags (match_id, leg_id, flag_type, severity, details_json)
                   VALUES (?,?,?,?,?)"#,
            )
            .bind(match_id)
            .bind(leg_id.or(flag.leg_id))
            .bind(flag.flag_type)
            .bind(flag.severity)
            .bind(&flag.details_json)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(match_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn list_matches(
        &self,
        from_day: Option<&str>,
        to_day: Option<&str>,
        status: Option<&str>,
        flagged_only: bool,
        unreviewed_only: bool,
        company: Option<&str>,
        q_search: Option<&str>,
        sort: &str,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<TripMatchRow>, i64)> {
        let mut filter = String::from(" FROM trip_matches m WHERE 1=1");
        if from_day.is_some() {
            filter.push_str(" AND m.day_local >= ?");
        }
        if to_day.is_some() {
            filter.push_str(" AND m.day_local <= ?");
        }
        if status.is_some() {
            filter.push_str(" AND m.status = ?");
        }
        if company.is_some() {
            filter.push_str(" AND m.company = ?");
        }
        if flagged_only {
            filter.push_str(" AND EXISTS (SELECT 1 FROM trip_flags f WHERE f.match_id = m.id)");
        }
        if unreviewed_only {
            filter.push_str(" AND m.reviewed_at IS NULL");
        }
        // Free-text search over the fields a dispatcher actually types:
        // plate, driver, terminal, and drop-off destinations (leg to_name —
        // a Qena→Aswan trip must be findable by "Aswan", not just "Qena").
        let q_like = q_search
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("%{}%", s.trim()));
        if q_like.is_some() {
            filter.push_str(
                " AND (m.car_no_plate LIKE ? OR m.driver_name LIKE ? OR m.terminal_name LIKE ? \
                   OR EXISTS (SELECT 1 FROM trip_match_legs l \
                              WHERE l.match_id = m.id AND l.to_name LIKE ?))",
            );
        }

        let count_sql = format!("SELECT COUNT(*){filter}");
        let mut cq = sqlx::query_as::<_, (i64,)>(&count_sql);
        for v in [from_day, to_day, status, company].into_iter().flatten() {
            cq = cq.bind(v.to_string());
        }
        if let Some(ref like) = q_like {
            for _ in 0..4 {
                cq = cq.bind(like.clone());
            }
        }
        let (total,) = cq.fetch_one(&self.pool).await?;

        // "severity" ranks what a reviewer should look at first: critical
        // flags, then how many km over optimal the trip drove.
        let order = match sort {
            "severity" => {
                " ORDER BY (SELECT COUNT(*) FROM trip_flags f
                             WHERE f.match_id = m.id AND f.severity = 'critical') DESC,
                          COALESCE(m.actual_km - m.osrm_km, 0) DESC,
                          m.day_local DESC, m.id DESC"
            }
            _ => " ORDER BY m.day_local DESC, m.id DESC",
        };
        let page = page.max(1);
        let per_page = per_page.clamp(1, 200);
        let sql = format!(
            "SELECT m.*, (SELECT COUNT(*) FROM trip_flags f WHERE f.match_id = m.id) AS flag_count, (SELECT COUNT(*) FROM trip_flags f WHERE f.match_id = m.id AND f.severity = 'critical') AS critical_count{filter}{order} LIMIT ? OFFSET ?"
        );
        let mut q = sqlx::query_as::<_, TripMatchRow>(&sql);
        for v in [from_day, to_day, status, company].into_iter().flatten() {
            q = q.bind(v.to_string());
        }
        if let Some(ref like) = q_like {
            for _ in 0..4 {
                q = q.bind(like.clone());
            }
        }
        let rows = q
            .bind(per_page)
            .bind((page - 1) * per_page)
            .fetch_all(&self.pool)
            .await?;
        Ok((rows, total))
    }

    pub async fn get_match(&self, id: i64) -> Result<Option<TripMatchRow>> {
        Ok(sqlx::query_as::<_, TripMatchRow>(
            r#"SELECT m.*, (SELECT COUNT(*) FROM trip_flags f WHERE f.match_id = m.id) AS flag_count,
                      (SELECT COUNT(*) FROM trip_flags f WHERE f.match_id = m.id AND f.severity = 'critical') AS critical_count
                 FROM trip_matches m WHERE m.id = ?"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn get_legs(&self, match_id: i64) -> Result<Vec<TripLegRow>> {
        Ok(sqlx::query_as::<_, TripLegRow>(
            "SELECT * FROM trip_match_legs WHERE match_id = ? ORDER BY seq",
        )
        .bind(match_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_flags(&self, match_id: i64) -> Result<Vec<TripFlagRow>> {
        Ok(sqlx::query_as::<_, TripFlagRow>(
            "SELECT * FROM trip_flags WHERE match_id = ? ORDER BY id",
        )
        .bind(match_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn set_reviewed(&self, id: i64, note: Option<&str>) -> Result<bool> {
        let res = sqlx::query(
            r#"UPDATE trip_matches
                  SET reviewed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'), review_note = ?
                WHERE id = ?"#,
        )
        .bind(note)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    // ---- calibration suggestions ----------------------------------------------

    /// Upsert a suggestion. A previous dismissal is preserved unless the
    /// suggested point moved > 100 m (then it's worth another look).
    pub async fn upsert_suggestion(&self, s: &Suggestion, scan_run_id: i64) -> Result<()> {
        let existing: Option<(f64, f64, String)> = sqlx::query_as(
            "SELECT suggested_lat, suggested_lng, status FROM location_suggestions WHERE kind = ? AND name = ?",
        )
        .bind(s.kind)
        .bind(&s.name)
        .fetch_optional(&self.pool)
        .await?;

        let status = match &existing {
            Some((lat, lng, status)) if status != "pending" => {
                let moved = crate::zones::geo::haversine_m(*lat, *lng, s.suggested_lat, s.suggested_lng);
                if moved > 100.0 { "pending" } else { status.as_str() }
            }
            _ => "pending",
        }
        .to_string();

        sqlx::query(
            r#"INSERT INTO location_suggestions
                 (kind, name, suggested_lat, suggested_lng, current_lat, current_lng,
                  offset_m, stop_count, scan_run_id, status, updated_at)
               VALUES (?,?,?,?,?,?,?,?,?,?, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
               ON CONFLICT(kind, name) DO UPDATE SET
                  suggested_lat = excluded.suggested_lat,
                  suggested_lng = excluded.suggested_lng,
                  current_lat   = excluded.current_lat,
                  current_lng   = excluded.current_lng,
                  offset_m      = excluded.offset_m,
                  stop_count    = excluded.stop_count,
                  scan_run_id   = excluded.scan_run_id,
                  status        = excluded.status,
                  updated_at    = excluded.updated_at"#,
        )
        .bind(s.kind)
        .bind(&s.name)
        .bind(s.suggested_lat)
        .bind(s.suggested_lng)
        .bind(s.current_lat)
        .bind(s.current_lng)
        .bind(s.offset_m)
        .bind(s.stop_count)
        .bind(scan_run_id)
        .bind(status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Whole-window aggregates for the KPI strip — computed in SQL so they
    /// are exact regardless of pagination.
    pub async fn match_summary(
        &self,
        from_day: Option<&str>,
        to_day: Option<&str>,
        company: Option<&str>,
    ) -> Result<MatchSummaryRow> {
        let mut filter = String::from(" FROM trip_matches m WHERE 1=1");
        if from_day.is_some() {
            filter.push_str(" AND m.day_local >= ?");
        }
        if to_day.is_some() {
            filter.push_str(" AND m.day_local <= ?");
        }
        if company.is_some() {
            filter.push_str(" AND m.company = ?");
        }
        let sql = format!(
            r#"SELECT
                 COUNT(*)                                             AS total,
                 SUM(m.status = 'matched')                            AS matched,
                 SUM(m.status = 'partial')                            AS partial,
                 SUM(m.status = 'unmatched')                          AS unmatched,
                 SUM(EXISTS (SELECT 1 FROM trip_flags f WHERE f.match_id = m.id))          AS flagged,
                 SUM(EXISTS (SELECT 1 FROM trip_flags f WHERE f.match_id = m.id
                              AND f.severity = 'critical'))           AS critical,
                 SUM(m.reviewed_at IS NULL AND EXISTS (
                       SELECT 1 FROM trip_flags f WHERE f.match_id = m.id))                AS flagged_unreviewed,
                 COALESCE(SUM(m.actual_km), 0.0)                      AS actual_km,
                 COALESCE(SUM(CASE WHEN m.osrm_km IS NOT NULL THEN m.actual_km END), 0.0) AS actual_km_compared,
                 COALESCE(SUM(m.osrm_km), 0.0)                        AS osrm_km
               {filter}"#
        );
        let mut q = sqlx::query_as::<_, MatchSummaryRow>(&sql);
        for v in [from_day, to_day, company].into_iter().flatten() {
            q = q.bind(v.to_string());
        }
        Ok(q.fetch_one(&self.pool).await?)
    }

    /// Top excess-km routes in the window (for the "worst route" KPI).
    pub async fn worst_routes(
        &self,
        from_day: Option<&str>,
        to_day: Option<&str>,
        company: Option<&str>,
        limit: i64,
    ) -> Result<Vec<WorstRouteRow>> {
        let mut filter = String::from(
            " FROM trip_matches m WHERE m.osrm_km IS NOT NULL AND m.actual_km IS NOT NULL",
        );
        if from_day.is_some() {
            filter.push_str(" AND m.day_local >= ?");
        }
        if to_day.is_some() {
            filter.push_str(" AND m.day_local <= ?");
        }
        if company.is_some() {
            filter.push_str(" AND m.company = ?");
        }
        let sql = format!(
            r#"SELECT m.terminal_name,
                      (SELECT GROUP_CONCAT(DISTINCT l.to_name)
                         FROM trip_match_legs l
                        WHERE l.match_id = m.id AND l.leg_type != 'return') AS destinations,
                      COUNT(*)                                     AS trips,
                      COALESCE(SUM(m.actual_km - m.osrm_km), 0.0)  AS excess_km
                 {filter}
                GROUP BY m.terminal_name, destinations
               HAVING excess_km > 0
                ORDER BY excess_km DESC
                LIMIT ?"#
        );
        let mut q = sqlx::query_as::<_, WorstRouteRow>(&sql);
        for v in [from_day, to_day, company].into_iter().flatten() {
            q = q.bind(v.to_string());
        }
        Ok(q.bind(limit.clamp(1, 20)).fetch_all(&self.pool).await?)
    }

    /// Local days within the last `days_back` days that still have a trip
    /// with an open return (return_pending flag) — re-audited nightly.
    pub async fn days_with_pending_returns(&self, days_back: i64) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"SELECT DISTINCT m.day_local
                 FROM trip_matches m
                 JOIN trip_flags f ON f.match_id = m.id AND f.flag_type = 'return_pending'
                WHERE m.day_local >= date('now', ?)"#,
        )
        .bind(format!("-{} days", days_back.max(1)))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(d,)| d).collect())
    }

    pub async fn upsert_mileage_check(
        &self,
        vehicle_id: &str,
        day_local: &str,
        etit_km: Option<f64>,
        computed_km: f64,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO mileage_checks (vehicle_id, day_local, etit_km, computed_km)
               VALUES (?,?,?,?)
               ON CONFLICT(vehicle_id, day_local) DO UPDATE SET
                  etit_km = excluded.etit_km,
                  computed_km = excluded.computed_km"#,
        )
        .bind(vehicle_id)
        .bind(day_local)
        .bind(etit_km)
        .bind(computed_km)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_suggestion_auto_applied(&self, kind: &str, name: &str) -> Result<()> {
        sqlx::query(
            r#"UPDATE location_suggestions
                  SET status = 'auto_applied',
                      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                WHERE kind = ? AND name = ? AND status = 'pending'"#,
        )
        .bind(kind)
        .bind(name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Per-day newest audit write epoch, for the nightly activity sweep.
    pub async fn audit_write_epoch_by_day(
        &self,
        since_day: &str,
    ) -> Result<Vec<(String, i64)>> {
        Ok(sqlx::query_as(
            r#"SELECT day_local, CAST(strftime('%s', MAX(created_at)) AS INTEGER)
                 FROM trip_matches
                WHERE day_local >= ?
                GROUP BY day_local"#,
        )
        .bind(since_day)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Remove audit rows for parent trips that no longer exist (deleted or
    /// re-dated) on a day being re-scanned. Legs/flags cascade.
    pub async fn delete_stale_for_day(&self, day: &str, keep_ids: &[i64]) -> Result<u64> {
        let placeholders = std::iter::repeat("?")
            .take(keep_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = if keep_ids.is_empty() {
            "DELETE FROM trip_matches WHERE day_local = ?".to_string()
        } else {
            format!("DELETE FROM trip_matches WHERE day_local = ? AND parent_trip_id NOT IN ({placeholders})")
        };
        let mut q = sqlx::query(&sql).bind(day);
        for id in keep_ids {
            q = q.bind(id);
        }
        Ok(q.execute(&self.pool).await?.rows_affected())
    }

    /// Mark live suggestions superseded when a human has since re-pinned the
    /// place outside the suggestion flow (pin_source no longer gps-derived,
    /// or the pin moved away from the suggested point). Runs each scan so
    /// the Locations queue never shows ghosts.
    pub async fn supersede_suggestion(&self, kind: &str, name: &str) -> Result<bool> {
        let res = sqlx::query(
            r#"UPDATE location_suggestions
                  SET status = 'accepted',
                      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                WHERE kind = ? AND name = ? AND status IN ('pending', 'auto_applied')"#,
        )
        .bind(kind)
        .bind(name)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// All suggestions still shown in the queue, for reconciliation.
    /// The last element is the suggestion's updated_at as a unix epoch.
    pub async fn live_suggestions(&self) -> Result<Vec<(String, String, f64, f64, i64)>> {
        Ok(sqlx::query_as(
            r#"SELECT kind, name, suggested_lat, suggested_lng,
                      CAST(strftime('%s', updated_at) AS INTEGER)
                 FROM location_suggestions
                WHERE status IN ('pending', 'auto_applied')"#,
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn list_suggestions(&self, status: Option<&str>) -> Result<Vec<SuggestionRow>> {
        let rows = match status {
            Some(s) => {
                sqlx::query_as::<_, SuggestionRow>(
                    "SELECT * FROM location_suggestions WHERE status = ? ORDER BY stop_count DESC",
                )
                .bind(s.to_string())
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, SuggestionRow>(
                    "SELECT * FROM location_suggestions ORDER BY stop_count DESC",
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows)
    }

    pub async fn ack_suggestion(&self, id: i64, status: &str) -> Result<bool> {
        let res = sqlx::query(
            r#"UPDATE location_suggestions
                  SET status = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                WHERE id = ?"#,
        )
        .bind(status)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    // ---- calibration input ------------------------------------------------------

    /// All stored (vehicle_id, day_local, stops_json) rows newer than the
    /// given local day. Only reads what's already in SQLite — calibration
    /// never triggers upstream fetches.
    pub async fn load_recent_stop_rows(
        &self,
        since_day: &str,
    ) -> Result<Vec<(String, String, String)>> {
        Ok(sqlx::query_as(
            r#"SELECT vehicle_id, day_local, stops_json
                 FROM history_days
                WHERE day_local >= ? AND stops_json IS NOT NULL AND stops_json != '[]'"#,
        )
        .bind(since_day)
        .fetch_all(&self.pool)
        .await?)
    }
}
