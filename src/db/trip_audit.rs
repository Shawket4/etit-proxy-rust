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
                      actual_geometry, osrm_geometry)
                   VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
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

    pub async fn list_matches(
        &self,
        from_day: Option<&str>,
        to_day: Option<&str>,
        status: Option<&str>,
        flagged_only: bool,
        company: Option<&str>,
    ) -> Result<Vec<TripMatchRow>> {
        let mut sql = String::from(
            r#"SELECT m.*, (SELECT COUNT(*) FROM trip_flags f WHERE f.match_id = m.id) AS flag_count
                 FROM trip_matches m WHERE 1=1"#,
        );
        if from_day.is_some() {
            sql.push_str(" AND m.day_local >= ?");
        }
        if to_day.is_some() {
            sql.push_str(" AND m.day_local <= ?");
        }
        if status.is_some() {
            sql.push_str(" AND m.status = ?");
        }
        if company.is_some() {
            sql.push_str(" AND m.company = ?");
        }
        if flagged_only {
            sql.push_str(" AND EXISTS (SELECT 1 FROM trip_flags f WHERE f.match_id = m.id)");
        }
        sql.push_str(" ORDER BY m.day_local DESC, m.id DESC LIMIT 500");

        let mut q = sqlx::query_as::<_, TripMatchRow>(&sql);
        if let Some(v) = from_day {
            q = q.bind(v.to_string());
        }
        if let Some(v) = to_day {
            q = q.bind(v.to_string());
        }
        if let Some(v) = status {
            q = q.bind(v.to_string());
        }
        if let Some(v) = company {
            q = q.bind(v.to_string());
        }
        Ok(q.fetch_all(&self.pool).await?)
    }

    pub async fn get_match(&self, id: i64) -> Result<Option<TripMatchRow>> {
        Ok(sqlx::query_as::<_, TripMatchRow>(
            r#"SELECT m.*, (SELECT COUNT(*) FROM trip_flags f WHERE f.match_id = m.id) AS flag_count
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
