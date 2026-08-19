//! Minimal OSRM `/route` client.
//!
//! Day legs are queried with `exclude=<class>` (time-gated truck roads);
//! legs departing inside the tanker permit window are routed on the full
//! network. If the running dataset doesn't know the exclude class (e.g. the
//! plain car profile is still deployed), OSRM answers 400 — we retry once
//! without the exclusion and log, so the auditor degrades gracefully.

use anyhow::{anyhow, Result};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct OsrmRoute {
    pub distance_km: f64,
    pub duration_secs: i64,
    /// polyline5-encoded geometry.
    pub geometry: String,
}

#[derive(Clone)]
pub struct OsrmClient {
    http: reqwest::Client,
    base_url: String,
    exclude_class: String,
}

#[derive(Deserialize)]
struct OsrmResponse {
    code: String,
    #[serde(default)]
    routes: Vec<OsrmResponseRoute>,
}

#[derive(Deserialize)]
struct OsrmResponseRoute {
    distance: f64,
    duration: f64,
    #[serde(default)]
    geometry: String,
}

impl OsrmClient {
    pub fn new(http: reqwest::Client, base_url: String, exclude_class: String) -> Self {
        Self { http, base_url, exclude_class }
    }

    /// Route from → to. `restricted` = departure is outside the permit
    /// window, so the time-gated roads must be excluded.
    pub async fn route(
        &self,
        from: (f64, f64),
        to: (f64, f64),
        restricted: bool,
    ) -> Result<OsrmRoute> {
        let use_exclude = restricted && !self.exclude_class.is_empty();
        match self.route_inner(from, to, use_exclude).await {
            Ok(r) => Ok(r),
            Err(e) if use_exclude => {
                tracing::warn!(error = %e, "OSRM query with exclude failed; retrying without");
                self.route_inner(from, to, false).await
            }
            Err(e) => Err(e),
        }
    }

    async fn route_inner(
        &self,
        (from_lat, from_lng): (f64, f64),
        (to_lat, to_lng): (f64, f64),
        exclude: bool,
    ) -> Result<OsrmRoute> {
        let mut url = format!(
            "{}/route/v1/driving/{:.6},{:.6};{:.6},{:.6}?overview=full&geometries=polyline&alternatives=false&steps=false",
            self.base_url, from_lng, from_lat, to_lng, to_lat
        );
        if exclude {
            url.push_str(&format!("&exclude={}", self.exclude_class));
        }

        let resp = self.http.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("OSRM {status}: {}", body.chars().take(200).collect::<String>()));
        }
        let parsed: OsrmResponse = resp.json().await?;
        if parsed.code != "Ok" {
            return Err(anyhow!("OSRM code {}", parsed.code));
        }
        let route = parsed
            .routes
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("OSRM returned no routes"))?;

        Ok(OsrmRoute {
            distance_km: route.distance / 1000.0,
            duration_secs: route.duration.round() as i64,
            geometry: route.geometry,
        })
    }
}
