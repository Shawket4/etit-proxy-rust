//! Vehicles persistence. Tier A replaces the whole list (metadata, and the
//! live columns as merged in memory); every Tier B poll then rewrites the
//! live columns — online, speed, status, status_label, last_location_at,
//! lat, lng — so the table is a truthful last-known state, not a day-old
//! snapshot. Reads seed the in-memory cache on cold start.

use anyhow::Result;
use sqlx::SqlitePool;

use crate::domain::time::{utc_now, Timestamp};
use crate::domain::Vehicle;

pub struct VehiclesRepo {
    pool: SqlitePool,
}

impl VehiclesRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Replace the entire snapshot in a single transaction. Faster than per-row
    /// upsert for the typical 50–500 row table.
    pub async fn replace_all(&self, vehicles: &[Vehicle]) -> Result<()> {
        let now = utc_now().to_rfc3339();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM vehicles").execute(&mut *tx).await?;

        for v in vehicles {
            let extra = serde_json::to_string(&v.extra).unwrap_or_else(|_| "{}".into());
            let last_iso = v.last_location_at.map(|t| t.to_rfc3339());

            sqlx::query(
                r#"
                INSERT INTO vehicles (
                    id, codename, plate, speed_limit, online, speed,
                    status, status_label, last_location_at, lat, lng,
                    extra_json, refreshed_at
                ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)
                "#,
            )
            .bind(&v.id)
            .bind(&v.codename)
            .bind(&v.plate)
            .bind(v.speed_limit as i64)
            .bind(if v.online { 1_i64 } else { 0_i64 })
            .bind(v.speed as i64)
            .bind(v.status as i64)
            .bind(&v.status_label)
            .bind(last_iso)
            .bind(v.lat)
            .bind(v.lng)
            .bind(extra)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Rewrite the live columns of every given vehicle (Tier B, each poll).
    /// Metadata columns are left alone; ids not in the table are skipped —
    /// Tier A owns which vehicles exist. One transaction, ~22 rows a minute.
    pub async fn update_live(&self, vehicles: &[Vehicle]) -> Result<u64> {
        let now = utc_now().to_rfc3339();
        let mut tx = self.pool.begin().await?;
        let mut touched = 0;
        for v in vehicles {
            let r = sqlx::query(
                r#"
                UPDATE vehicles
                   SET online = ?, speed = ?, status = ?, status_label = ?,
                       last_location_at = ?, lat = ?, lng = ?, refreshed_at = ?
                 WHERE id = ?
                "#,
            )
            .bind(if v.online { 1_i64 } else { 0_i64 })
            .bind(v.speed as i64)
            .bind(v.status as i64)
            .bind(&v.status_label)
            .bind(v.last_location_at.map(|t| t.to_rfc3339()))
            .bind(v.lat)
            .bind(v.lng)
            .bind(&now)
            .bind(&v.id)
            .execute(&mut *tx)
            .await?;
            touched += r.rows_affected();
        }
        tx.commit().await?;
        Ok(touched)
    }

    pub async fn load_all(&self) -> Result<Vec<Vehicle>> {
        let rows = sqlx::query_as::<_, VehicleRow>(
            r#"
            SELECT id, codename, plate, speed_limit, online, speed,
                   status, status_label, last_location_at, lat, lng, extra_json
              FROM vehicles
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(VehicleRow::into_domain).collect())
    }

    pub async fn count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM vehicles")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }
}

#[derive(sqlx::FromRow)]
struct VehicleRow {
    id: String,
    codename: String,
    plate: String,
    speed_limit: i64,
    online: i64,
    speed: i64,
    status: i64,
    status_label: String,
    last_location_at: Option<String>,
    lat: Option<f64>,
    lng: Option<f64>,
    extra_json: String,
}

impl VehicleRow {
    fn into_domain(self) -> Vehicle {
        let last_location_at: Option<Timestamp> = self
            .last_location_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&chrono::Utc));

        let extra: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&self.extra_json).unwrap_or_default();

        Vehicle {
            id: self.id,
            codename: self.codename,
            plate: self.plate,
            speed_limit: self.speed_limit.max(0) as u32,
            online: self.online != 0,
            speed: self.speed.max(0) as u32,
            status: self.status as i32,
            status_label: self.status_label,
            last_location_at,
            lat: self.lat,
            lng: self.lng,
            extra,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::LiveVehicleStatus;
    use chrono::{DateTime, Utc};
    use sqlx::sqlite::SqlitePoolOptions;

    fn utc(s: &str) -> Timestamp {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    async fn repo() -> VehiclesRepo {
        // One connection: every `:memory:` connection is its own database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        VehiclesRepo::new(pool)
    }

    fn vehicle(id: &str, last: &str) -> Vehicle {
        Vehicle {
            id: id.into(),
            codename: format!("OL/{id}"),
            plate: id.into(),
            speed_limit: 80,
            online: true,
            speed: 0,
            status: 3,
            status_label: "Stopped (Ignition Off)".into(),
            last_location_at: Some(utc(last)),
            lat: None,
            lng: None,
            extra: Default::default(),
        }
    }

    async fn refreshed_at(r: &VehiclesRepo, id: &str) -> String {
        sqlx::query_scalar("SELECT refreshed_at FROM vehicles WHERE id = ?")
            .bind(id)
            .fetch_one(&r.pool)
            .await
            .unwrap()
    }

    /// The 2026-10-25 symptom: Tier B polled every minute but the table kept
    /// Tier A's startup values. Each poll must now land in the live columns.
    #[tokio::test]
    async fn tier_b_poll_updates_the_live_columns() {
        let r = repo().await;
        r.replace_all(&[vehicle("8567", "2026-10-25T06:48:43Z"), vehicle("6834", "2026-10-25T06:50:00Z")])
            .await
            .unwrap();
        let before = refreshed_at(&r, "8567").await;

        let mut v = vehicle("8567", "2026-10-25T06:48:43Z");
        let reading = LiveVehicleStatus {
            id: "8567".into(),
            plate: None,
            speed: 86,
            status: 1,
            status_label: "Moving".into(),
            lat: 26.84,
            lng: 33.92,
            timestamp: Some(utc("2026-10-25T07:18:00Z")),
            event: None,
            sensor_data: None,
        };
        assert!(v.apply_live(&reading, utc("2026-10-25T07:19:00Z")));
        let mut unknown = vehicle("not-in-table", "2026-10-25T07:00:00Z");
        unknown.speed = 1;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert_eq!(r.update_live(&[v, unknown]).await.unwrap(), 1);

        let all = r.load_all().await.unwrap();
        assert_eq!(all.len(), 2, "Tier B never inserts");
        let got = all.iter().find(|v| v.id == "8567").unwrap();
        assert_eq!(got.last_location_at, Some(utc("2026-10-25T07:18:00Z")));
        assert_eq!((got.speed, got.status, got.online), (86, 1, true));
        assert_eq!(got.status_label, "Moving");
        assert_eq!((got.lat, got.lng), (Some(26.84), Some(33.92)));
        assert_eq!((got.plate.as_str(), got.speed_limit), ("8567", 80));
        assert_ne!(refreshed_at(&r, "8567").await, before);

        // The other row is untouched.
        let other = all.iter().find(|v| v.id == "6834").unwrap();
        assert_eq!(other.last_location_at, Some(utc("2026-10-25T06:50:00Z")));
    }
}
