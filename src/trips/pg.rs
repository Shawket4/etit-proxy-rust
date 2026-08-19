//! Read-only access to the apex Postgres DB (FalconGo's schema).
//!
//! The trip auditor needs: parent trips + their container trips, the car →
//! ETIT-GUID mapping, terminal geofences (with aliases and radius
//! overrides), drop-off pins, and contracted distances from fee_mappings.
//!
//! The canonical-locations tables (`drop_off_points`, `terminal_aliases`,
//! `*.radius_m`) ship with the same release on the FalconGo side; every
//! query that touches them has a legacy fallback so the scanner still works
//! against a pre-migration database.

use std::collections::HashMap;

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use super::{Place, PlannedDelivery, PlannedTrip};

#[derive(Clone)]
pub struct ApexDb {
    pool: PgPool,
}

/// Everything location-shaped the matcher and calibrator need, loaded once
/// per scan.
pub struct LocationCatalog {
    /// Canonical terminal name → Place.
    pub terminals: HashMap<String, Place>,
    /// Alias (or canonical name) → canonical terminal name.
    pub terminal_lookup: HashMap<String, String>,
    /// Drop-off name → Place (only pinned ones).
    pub dropoffs: HashMap<String, Place>,
    /// Drop-off names that exist but have no pin (for unmatched reasons).
    pub unpinned_dropoffs: Vec<String>,
}

impl LocationCatalog {
    pub fn resolve_terminal(&self, raw: &str) -> Option<&Place> {
        let name = self.terminal_lookup.get(raw.trim())?;
        self.terminals.get(name)
    }

    /// All known geofences: used to exempt planned stops from the
    /// unplanned-stop flag.
    pub fn all_places(&self) -> Vec<Place> {
        self.terminals
            .values()
            .chain(self.dropoffs.values())
            .cloned()
            .collect()
    }
}

impl ApexDb {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(20))
            .connect(url)
            .await
            .context("connecting to apex Postgres")?;
        Ok(Self { pool })
    }

    /// Load terminals (+aliases, +radius overrides) and drop-off pins.
    pub async fn load_locations(
        &self,
        default_terminal_radius: f64,
        default_dropoff_radius: f64,
    ) -> Result<LocationCatalog> {
        // Terminals — radius_m may not exist pre-migration.
        let term_rows: Vec<(String, Option<f64>, Option<f64>, Option<f64>)> =
            match sqlx::query_as(
                r#"SELECT name, CAST(latitude AS float8), CAST(longitude AS float8),
                          CAST(radius_m AS float8)
                     FROM terminals WHERE deleted_at IS NULL"#,
            )
            .fetch_all(&self.pool)
            .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(error = %e, "terminals.radius_m not available; using legacy query");
                    let legacy: Vec<(String, Option<f64>, Option<f64>)> = sqlx::query_as(
                        r#"SELECT name, CAST(latitude AS float8), CAST(longitude AS float8)
                             FROM terminals WHERE deleted_at IS NULL"#,
                    )
                    .fetch_all(&self.pool)
                    .await
                    .context("loading terminals")?;
                    legacy.into_iter().map(|(n, la, ln)| (n, la, ln, None)).collect()
                }
            };

        let mut terminals = HashMap::new();
        let mut terminal_lookup = HashMap::new();
        for (name, lat, lng, radius) in term_rows {
            let name = name.trim().to_string();
            if let (Some(lat), Some(lng)) = (lat, lng) {
                if lat != 0.0 || lng != 0.0 {
                    terminal_lookup.insert(name.clone(), name.clone());
                    terminals.insert(
                        name.clone(),
                        Place {
                            name: name.clone(),
                            lat,
                            lng,
                            radius_m: radius.unwrap_or(default_terminal_radius),
                        },
                    );
                }
            }
        }

        // Aliases — table may not exist pre-migration.
        let alias_rows: Vec<(String, String)> = sqlx::query_as(
            r#"SELECT a.alias, t.name
                 FROM terminal_aliases a
                 JOIN terminals t ON t.id = a.terminal_id
                WHERE a.deleted_at IS NULL AND t.deleted_at IS NULL"#,
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "terminal_aliases not available; continuing without");
            Vec::new()
        });
        for (alias, name) in alias_rows {
            terminal_lookup.insert(alias.trim().to_string(), name.trim().to_string());
        }

        // Drop-off pins — prefer canonical drop_off_points, fall back to the
        // legacy per-mapping coordinates.
        let mut dropoffs = HashMap::new();
        let mut unpinned = Vec::new();
        let dop_rows: Result<Vec<(String, Option<f64>, Option<f64>, Option<f64>)>, sqlx::Error> =
            sqlx::query_as(
                r#"SELECT name, CAST(latitude AS float8), CAST(longitude AS float8),
                          CAST(radius_m AS float8)
                     FROM drop_off_points WHERE deleted_at IS NULL"#,
            )
            .fetch_all(&self.pool)
            .await;

        match dop_rows {
            Ok(rows) => {
                for (name, lat, lng, radius) in rows {
                    let name = name.trim().to_string();
                    match (lat, lng) {
                        (Some(lat), Some(lng)) if lat != 0.0 || lng != 0.0 => {
                            dropoffs.insert(
                                name.clone(),
                                Place {
                                    name,
                                    lat,
                                    lng,
                                    radius_m: radius.unwrap_or(default_dropoff_radius),
                                },
                            );
                        }
                        _ => unpinned.push(name),
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "drop_off_points not available; falling back to fee_mappings coords");
                let rows: Vec<(String, Option<f64>, Option<f64>)> = sqlx::query_as(
                    r#"SELECT DISTINCT ON (drop_off_point)
                              drop_off_point,
                              CAST(NULLIF(latitude, 0)  AS float8),
                              CAST(NULLIF(longitude, 0) AS float8)
                         FROM fee_mappings
                        WHERE deleted_at IS NULL AND drop_off_point <> ''
                        ORDER BY drop_off_point, (NULLIF(latitude, 0) IS NULL), updated_at DESC"#,
                )
                .fetch_all(&self.pool)
                .await
                .context("loading fee_mappings coords")?;
                for (name, lat, lng) in rows {
                    let name = name.trim().to_string();
                    match (lat, lng) {
                        (Some(lat), Some(lng)) => {
                            dropoffs.insert(
                                name.clone(),
                                Place { name, lat, lng, radius_m: default_dropoff_radius },
                            );
                        }
                        _ => unpinned.push(name),
                    }
                }
            }
        }

        Ok(LocationCatalog {
            terminals,
            terminal_lookup,
            dropoffs,
            unpinned_dropoffs: unpinned,
        })
    }

    /// For unpinned drop-offs: which (vehicle GUID, local day) served each
    /// name since `since_day`. Used by calibration to gather candidate stops.
    pub async fn load_dropoff_service_days(
        &self,
        since_day: &str,
        names: &[String],
    ) -> Result<Vec<(String, String, String)>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_as(
            r#"SELECT DISTINCT t.drop_off_point, pt.date, c.etit_car_id
                 FROM trips t
                 JOIN parent_trips pt ON pt.id = t.parent_trip_id
                 LEFT JOIN cars c ON c.id = pt.car_id
                WHERE t.deleted_at IS NULL
                  AND pt.deleted_at IS NULL
                  AND pt.date >= $1
                  AND t.drop_off_point = ANY($2)
                  AND c.etit_car_id IS NOT NULL AND c.etit_car_id <> ''"#,
        )
        .bind(since_day)
        .bind(names)
        .fetch_all(&self.pool)
        .await
        .context("loading drop-off service days")?)
    }

    /// Load all parent trips for the given local dates (YYYY-MM-DD), with
    /// their container trips, car GUIDs, resolved geofences, and contracted
    /// distances.
    pub async fn load_planned_trips(
        &self,
        dates: &[String],
        catalog: &LocationCatalog,
    ) -> Result<Vec<PlannedTrip>> {
        let parents: Vec<(i64, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> =
            sqlx::query_as(
                r#"SELECT CAST(pt.id AS int8), pt.date, pt.company, pt.terminal,
                          pt.car_no_plate, pt.driver_name, c.etit_car_id
                     FROM parent_trips pt
                     LEFT JOIN cars c ON c.id = pt.car_id
                    WHERE pt.deleted_at IS NULL AND pt.date = ANY($1)
                    ORDER BY pt.date, pt.id"#,
            )
            .bind(dates)
            .fetch_all(&self.pool)
            .await
            .context("loading parent_trips")?;

        if parents.is_empty() {
            return Ok(Vec::new());
        }

        let parent_ids: Vec<i64> = parents.iter().map(|p| p.0).collect();
        let children: Vec<(i64, i64, Option<String>, Option<f64>)> = sqlx::query_as(
            r#"SELECT CAST(t.parent_trip_id AS int8), CAST(t.id AS int8),
                      t.drop_off_point, CAST(f.distance AS float8)
                 FROM trips t
                 LEFT JOIN fee_mappings f
                        ON f.deleted_at IS NULL
                       AND f.company = t.company
                       AND f.terminal = t.terminal
                       AND f.drop_off_point = t.drop_off_point
                WHERE t.deleted_at IS NULL AND t.parent_trip_id = ANY($1)
                ORDER BY t.parent_trip_id, t.id"#,
        )
        .bind(&parent_ids)
        .fetch_all(&self.pool)
        .await
        .context("loading container trips")?;

        let mut by_parent: HashMap<i64, Vec<PlannedDelivery>> = HashMap::new();
        for (parent_id, trip_id, drop_off, expected_km) in children {
            let name = drop_off.unwrap_or_default().trim().to_string();
            let deliveries = by_parent.entry(parent_id).or_default();
            // Multi-compartment loads produce several container trips to the
            // same drop-off — physically one stop, so audit it as one
            // delivery.
            if deliveries.iter().any(|d| d.drop_off_point == name) {
                continue;
            }
            deliveries.push(PlannedDelivery {
                trip_id,
                place: catalog.dropoffs.get(&name).cloned(),
                drop_off_point: name,
                expected_km,
            });
        }

        Ok(parents
            .into_iter()
            .map(|(id, date, company, terminal, plate, driver, guid)| {
                let terminal_name = terminal.unwrap_or_default().trim().to_string();
                PlannedTrip {
                    parent_trip_id: id,
                    day_local: date,
                    company: company.unwrap_or_default(),
                    terminal: catalog.resolve_terminal(&terminal_name).cloned(),
                    terminal_name,
                    car_no_plate: plate.unwrap_or_default(),
                    driver_name: driver.unwrap_or_default(),
                    etit_car_id: guid.unwrap_or_default().trim().to_string(),
                    deliveries: by_parent.remove(&id).unwrap_or_default(),
                }
            })
            .collect())
    }
}
