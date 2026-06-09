//! History persistence.
//!
//! A row per `(vehicle_id, day_local)` carries:
//!   - `points_json`  — serialized `Vec<HistoryPoint>`
//!   - `geometry`     — precomputed polyline (avoid recompute per read)
//!   - `is_hot`       — 1 if today/yesterday at write time, else 0
//!
//! Reads are cheap enough that we don't bother with a per-day blob format;
//! `points_json` is stored verbatim.

use anyhow::Result;
use chrono::NaiveDate;
use sqlx::SqlitePool;

use crate::domain::history::{encode_polyline, CachedHistory};
use crate::domain::time::utc_now;
use crate::domain::HistoryPoint;

#[derive(Clone)]
pub struct HistoryRepo {
    pool: SqlitePool,
}

impl HistoryRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Look up cached points for a (vehicle, day). Returns `None` if absent.
    pub async fn get(
        &self,
        vehicle_id: &str,
        date: NaiveDate,
    ) -> Result<Option<CachedHistory>> {
        let date_str = date.format("%Y-%m-%d").to_string();
        let row: Option<HistoryRow> = sqlx::query_as::<_, HistoryRow>(
            r#"
            SELECT vehicle_id, day_local, fetched_at, points_json, geometry, stops_json, sensors_json
              FROM history_days
             WHERE vehicle_id = ? AND day_local = ?
            "#,
        )
        .bind(vehicle_id)
        .bind(date_str)
        .fetch_optional(&self.pool)
        .await?;

        let Some(r) = row else { return Ok(None) };

        let points: Vec<HistoryPoint> =
            serde_json::from_str(&r.points_json).unwrap_or_default();
        
        let stops = serde_json::from_str(&r.stops_json).unwrap_or_default();
        let sensors = serde_json::from_str(&r.sensors_json).unwrap_or_default();

        let fetched_at = chrono::DateTime::parse_from_rfc3339(&r.fetched_at)
            .map(|d| d.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| utc_now());

        Ok(Some(CachedHistory {
            points,
            stops,
            sensors,
            geometry: r.geometry,
            fetched_at,
        }))
    }

    /// Upsert a day. `is_hot` is set to `1` for today/yesterday rows so the
    /// poller knows which entries it should rewrite each tick.
    pub async fn upsert(
        &self,
        vehicle_id: &str,
        date: NaiveDate,
        data: &CachedHistory,
        is_hot: bool,
    ) -> Result<()> {
        let date_str   = date.format("%Y-%m-%d").to_string();
        let now        = utc_now().to_rfc3339();
        let geometry   = encode_polyline(&data.points);
        let points_str = serde_json::to_string(&data.points).unwrap_or_else(|_| "[]".into());
        let stops_str  = serde_json::to_string(&data.stops).unwrap_or_else(|_| "[]".into());
        let sensors_str = serde_json::to_string(&data.sensors).unwrap_or_else(|_| "[]".into());

        sqlx::query(
            r#"
            INSERT INTO history_days
                (vehicle_id, day_local, fetched_at, point_count, points_json, geometry, stops_json, sensors_json, is_hot)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(vehicle_id, day_local) DO UPDATE SET
                fetched_at   = excluded.fetched_at,
                point_count  = excluded.point_count,
                points_json  = excluded.points_json,
                geometry     = excluded.geometry,
                stops_json   = excluded.stops_json,
                sensors_json = excluded.sensors_json,
                is_hot       = excluded.is_hot
            "#,
        )
        .bind(vehicle_id)
        .bind(date_str)
        .bind(now)
        .bind(data.points.len() as i64)
        .bind(points_str)
        .bind(geometry)
        .bind(stops_str)
        .bind(sensors_str)
        .bind(if is_hot { 1_i64 } else { 0_i64 })
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Flip any hot rows that are no longer today/yesterday to cold. Run on
    /// each Tier C tick before refreshing.
    pub async fn cool_aged_hot_rows(&self, today: NaiveDate, yesterday: NaiveDate) -> Result<u64> {
        let res = sqlx::query(
            r#"
            UPDATE history_days
               SET is_hot = 0
             WHERE is_hot = 1
               AND day_local NOT IN (?, ?)
            "#,
        )
        .bind(today.format("%Y-%m-%d").to_string())
        .bind(yesterday.format("%Y-%m-%d").to_string())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    pub async fn count(&self) -> Result<i64> {
        let (n,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM history_days").fetch_one(&self.pool).await?;
        Ok(n)
    }

    /// Wipe all history (used by `POST /admin/cache/invalidate { all }`).
    pub async fn truncate(&self) -> Result<u64> {
        let res = sqlx::query("DELETE FROM history_days").execute(&self.pool).await?;
        Ok(res.rows_affected())
    }
}

#[derive(sqlx::FromRow)]
struct HistoryRow {
    #[allow(dead_code)] vehicle_id: String,
    #[allow(dead_code)] day_local:  String,
    fetched_at:   String,
    points_json:  String,
    geometry:     String,
    stops_json:   String,
    sensors_json: String,
}
