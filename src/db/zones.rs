use anyhow::Result;
use chrono::{NaiveDate, Utc};
use sqlx::SqlitePool;

use crate::zones::{DeadZone, PointTS, ScanSummary};
use crate::domain::HistoryPoint;
use crate::domain::time::utc_now;

#[derive(Clone)]
pub struct ZonesRepo {
    pool: SqlitePool,
}

#[derive(sqlx::FromRow)]
pub struct UnalertedVisitRow {
    pub id: i64,
    pub vehicle_id: String,
    pub zone_id: i64,
    pub entry_ts: String,
    pub exit_ts: String,
    pub dwell_secs: i64,
    pub name: String,
    pub lat: f64,
    pub lng: f64,
}

impl ZonesRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    // --- a) Zone CRUD ---

    pub async fn list(&self, active_only: bool) -> Result<Vec<DeadZone>> {
        if active_only {
            Ok(sqlx::query_as::<_, DeadZone>("SELECT id, name, lat, lng, radius_m, active FROM dead_zones WHERE active = 1")
                .fetch_all(&self.pool)
                .await?)
        } else {
            Ok(sqlx::query_as::<_, DeadZone>("SELECT id, name, lat, lng, radius_m, active FROM dead_zones")
                .fetch_all(&self.pool)
                .await?)
        }
    }

    pub async fn get_by_id(&self, id: i64) -> Result<Option<DeadZone>> {
        Ok(sqlx::query_as::<_, DeadZone>("SELECT id, name, lat, lng, radius_m, active FROM dead_zones WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn create(&self, name: &str, lat: f64, lng: f64, radius_m: f64, active: bool) -> Result<i64> {
        let active_int = if active { 1 } else { 0 };
        let res = sqlx::query(
            "INSERT INTO dead_zones (name, lat, lng, radius_m, active) VALUES (?, ?, ?, ?, ?)"
        )
        .bind(name)
        .bind(lat)
        .bind(lng)
        .bind(radius_m)
        .bind(active_int)
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_rowid())
    }

    pub async fn update(&self, id: i64, name: &str, lat: f64, lng: f64, radius_m: f64, active: bool) -> Result<u64> {
        let active_int = if active { 1 } else { 0 };
        let now = utc_now().to_rfc3339();
        let res = sqlx::query(
            "UPDATE dead_zones SET name = ?, lat = ?, lng = ?, radius_m = ?, active = ?, updated_at = ? WHERE id = ?"
        )
        .bind(name)
        .bind(lat)
        .bind(lng)
        .bind(radius_m)
        .bind(active_int)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    pub async fn soft_delete(&self, id: i64) -> Result<u64> {
        let now = utc_now().to_rfc3339();
        let res = sqlx::query(
            "UPDATE dead_zones SET active = 0, updated_at = ? WHERE id = ?"
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    // --- b) Window vehicles + points ---

    pub async fn get_window_vehicles(&self, days: &[NaiveDate]) -> Result<Vec<String>> {
        if days.is_empty() { return Ok(vec![]); }
        let day_strs: Vec<String> = days.iter().map(|d| d.format("%Y-%m-%d").to_string()).collect();
        let placeholders = vec!["?"; day_strs.len()].join(", ");
        let query = format!("SELECT DISTINCT vehicle_id FROM history_days WHERE day_local IN ({})", placeholders);
        
        let mut q = sqlx::query(&query);
        for day in &day_strs {
            q = q.bind(day);
        }
        
        let rows = q.fetch_all(&self.pool).await?;
        let mut vids = Vec::with_capacity(rows.len());
        for row in rows {
            use sqlx::Row;
            let vid: String = row.try_get("vehicle_id")?;
            vids.push(vid);
        }
        Ok(vids)
    }

    pub async fn get_window_points(&self, vehicle_id: &str, days: &[NaiveDate]) -> Result<Vec<PointTS>> {
        if days.is_empty() { return Ok(vec![]); }
        let day_strs: Vec<String> = days.iter().map(|d| d.format("%Y-%m-%d").to_string()).collect();
        let placeholders = vec!["?"; day_strs.len()].join(", ");
        let query = format!("SELECT points_json FROM history_days WHERE vehicle_id = ? AND day_local IN ({})", placeholders);
        
        let mut q = sqlx::query(&query).bind(vehicle_id);
        for day in &day_strs {
            q = q.bind(day);
        }
        
        let rows = q.fetch_all(&self.pool).await?;
        let mut all_points = Vec::new();
        
        for row in rows {
            use sqlx::Row;
            let points_json: String = row.try_get("points_json")?;
            
            let points: Vec<HistoryPoint> = match serde_json::from_str(&points_json) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(vehicle_id, error=%e, "points_json parse failed; skipping day");
                    continue;
                }
            };
            
            let mut dropped = 0;
            for hp in points {
                if let Some(ts) = hp.timestamp {
                    all_points.push(PointTS {
                        ts: ts.timestamp(), // convert from DateTime<Utc> to unix secs
                        lat: hp.lat,
                        lng: hp.lng,
                    });
                } else {
                    dropped += 1;
                }
            }
            if dropped > 0 {
                tracing::debug!(vehicle_id, dropped, "points dropped: no timestamp");
            }
        }
        
        tracing::debug!(vehicle_id, loaded = all_points.len(), "window points loaded");
        
        // Ensure chronological order across stitched day windows
        all_points.sort_by_key(|p| p.ts);
        Ok(all_points)
    }

    // --- c) Upsert a violation visit ---

    pub async fn upsert_visit(&self, vehicle_id: &str, zone_id: i64, entry_ts: i64, exit_ts: i64, points: u32) -> Result<()> {
        use chrono::TimeZone;
        let entry_iso = Utc.timestamp_opt(entry_ts, 0).unwrap().to_rfc3339();
        let exit_iso = Utc.timestamp_opt(exit_ts, 0).unwrap().to_rfc3339();
        let dwell_secs = exit_ts - entry_ts;
        
        sqlx::query(
            r#"
            INSERT INTO zone_visits (vehicle_id, zone_id, entry_ts, exit_ts, dwell_secs, point_count, alerted_at)
            VALUES (?, ?, ?, ?, ?, ?, NULL)
            ON CONFLICT(vehicle_id, zone_id, entry_ts) DO UPDATE SET
                exit_ts = excluded.exit_ts,
                dwell_secs = excluded.dwell_secs,
                point_count = excluded.point_count
            "#
        )
        .bind(vehicle_id)
        .bind(zone_id)
        .bind(entry_iso)
        .bind(exit_iso)
        .bind(dwell_secs)
        .bind(points)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // --- d) Un-alerted query ---

    pub async fn get_unalerted_visits(&self) -> Result<Vec<UnalertedVisitRow>> {
        let rows = sqlx::query_as::<_, UnalertedVisitRow>(
            r#"
            SELECT zv.id, zv.vehicle_id, zv.zone_id, zv.entry_ts, zv.exit_ts, zv.dwell_secs,
                   dz.name, dz.lat, dz.lng
            FROM zone_visits zv
            JOIN dead_zones dz ON dz.id = zv.zone_id
            WHERE zv.alerted_at IS NULL
            "#
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    // --- e) Mark alerted ---

    pub async fn mark_alerted(&self, visit_id: i64) -> Result<()> {
        let now = utc_now().to_rfc3339();
        sqlx::query("UPDATE zone_visits SET alerted_at = ? WHERE id = ?")
            .bind(now)
            .bind(visit_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // --- f) scan_runs ---

    pub async fn insert_scan_run(&self) -> Result<i64> {
        let res = sqlx::query("INSERT INTO zone_scan_runs (status) VALUES ('running')")
            .execute(&self.pool)
            .await?;
        Ok(res.last_insert_rowid())
    }

    pub async fn finalize_scan_run(&self, run_id: i64, summary: &ScanSummary) -> Result<()> {
        let now = utc_now().to_rfc3339();
        sqlx::query(
            r#"
            UPDATE zone_scan_runs 
            SET finished_at = ?, status = ?, days_scanned = ?, vehicles_scanned = ?, violations_found = ?, alerts_sent = ?, error = ?
            WHERE id = ?
            "#
        )
        .bind(now)
        .bind(&summary.status)
        .bind(summary.days_scanned)
        .bind(summary.vehicles_scanned)
        .bind(summary.violations_found)
        .bind(summary.alerts_sent)
        .bind(&summary.error)
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
