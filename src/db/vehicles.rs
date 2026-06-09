//! Vehicles persistence. We snapshot the entire vehicle list every Tier A
//! tick. Reads are used solely to seed the in-memory cache on cold start —
//! the live system always rebuilds from upstream within seconds anyway.

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
                    status, status_label, last_location_at, extra_json, refreshed_at
                ) VALUES (?,?,?,?,?,?,?,?,?,?,?)
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
            .bind(extra)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn load_all(&self) -> Result<Vec<Vehicle>> {
        let rows = sqlx::query_as::<_, VehicleRow>(
            r#"
            SELECT id, codename, plate, speed_limit, online, speed,
                   status, status_label, last_location_at, extra_json
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
            extra,
        }
    }
}
