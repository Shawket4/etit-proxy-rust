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

/// Result of map-matching a GPS trace onto the road network.
#[derive(Debug, Clone)]
pub struct OsrmMatch {
    /// Road distance along the matched path, km. Where the trace fragments
    /// into several sub-matchings (GPS gaps), the chord distance between the
    /// fragments' boundary points is added so the total stays comparable to
    /// a chord-sum over the same trace.
    pub distance_km: f64,
    /// polyline5 geometry per sub-matching, in order.
    pub geometries: Vec<String>,
    /// Fraction of input points the matcher accepted (0..1). Outliers and
    /// phantom fixes come back as null tracepoints and lower this.
    pub coverage: f64,
}

#[derive(Deserialize)]
struct MatchResponse {
    code: String,
    #[serde(default)]
    matchings: Vec<Matching>,
    #[serde(default)]
    tracepoints: Vec<Option<Tracepoint>>,
}

#[derive(Deserialize)]
struct Matching {
    distance: f64,
    #[serde(default)]
    geometry: String,
}

#[derive(Deserialize)]
struct Tracepoint {
    #[serde(default)]
    matchings_index: i64,
    #[serde(default)]
    location: Vec<f64>,
}

impl OsrmClient {
    pub fn new(http: reqwest::Client, base_url: String, exclude_class: String) -> Self {
        Self { http, base_url, exclude_class }
    }

    /// Map-match a timestamped GPS trace ((lat, lng, epoch_secs)) onto the
    /// road network. No exclude is applied: the actual is the actual, even
    /// on roads the truck wasn't allowed to use.
    pub async fn match_trace(&self, points: &[(f64, f64, i64)]) -> Result<OsrmMatch> {
        if points.len() < 2 {
            return Err(anyhow!("trace too short to match"));
        }
        let coords = points
            .iter()
            .map(|(la, ln, _)| format!("{ln:.6},{la:.6}"))
            .collect::<Vec<_>>()
            .join(";");
        let timestamps = points
            .iter()
            .map(|(_, _, t)| t.to_string())
            .collect::<Vec<_>>()
            .join(";");
        let radiuses = vec!["40"; points.len()].join(";");
        let url = format!(
            "{}/match/v1/driving/{coords}?overview=full&geometries=polyline&steps=false&gaps=split&timestamps={timestamps}&radiuses={radiuses}",
            self.base_url
        );

        let resp = self.http.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("OSRM match {status}: {}", body.chars().take(200).collect::<String>()));
        }
        let parsed: MatchResponse = resp.json().await?;
        if parsed.code != "Ok" || parsed.matchings.is_empty() {
            return Err(anyhow!("OSRM match code {}", parsed.code));
        }

        let matched_points = parsed.tracepoints.iter().flatten().count();
        let coverage = matched_points as f64 / points.len() as f64;

        let mut distance_m: f64 = parsed.matchings.iter().map(|m| m.distance).sum();

        // Bridge gaps between consecutive sub-matchings with the chord
        // between their boundary tracepoints, so fragmentation never makes
        // the matched total shorter than reality.
        let mut last_of_matching: Vec<Option<(f64, f64)>> = vec![None; parsed.matchings.len()];
        let mut first_of_matching: Vec<Option<(f64, f64)>> = vec![None; parsed.matchings.len()];
        for tp in parsed.tracepoints.iter().flatten() {
            if tp.location.len() == 2 {
                let idx = tp.matchings_index.max(0) as usize;
                if idx < last_of_matching.len() {
                    let p = (tp.location[1], tp.location[0]);
                    if first_of_matching[idx].is_none() {
                        first_of_matching[idx] = Some(p);
                    }
                    last_of_matching[idx] = Some(p);
                }
            }
        }
        for i in 1..parsed.matchings.len() {
            if let (Some(a), Some(b)) = (last_of_matching[i - 1], first_of_matching[i]) {
                distance_m += crate::zones::geo::haversine_m(a.0, a.1, b.0, b.1);
            }
        }

        Ok(OsrmMatch {
            distance_km: distance_m / 1000.0,
            geometries: parsed.matchings.into_iter().map(|m| m.geometry).collect(),
            coverage,
        })
    }

    /// Route from → to. `restricted` = departure is outside the permit
    /// window, so the time-gated roads must be excluded. `via` inserts
    /// intermediate waypoints (e.g. an allowed garage the truck dwelled at)
    /// so the optimal is measured over the route the trip legitimately took.
    pub async fn route(
        &self,
        from: (f64, f64),
        to: (f64, f64),
        via: &[(f64, f64)],
        restricted: bool,
    ) -> Result<OsrmRoute> {
        let use_exclude = restricted && !self.exclude_class.is_empty();
        match self.route_inner(from, to, via, use_exclude).await {
            Ok(r) => Ok(r),
            Err(e) if use_exclude => {
                tracing::warn!(error = %e, "OSRM query with exclude failed; retrying without");
                self.route_inner(from, to, via, false).await
            }
            Err(e) => Err(e),
        }
    }

    async fn route_inner(
        &self,
        (from_lat, from_lng): (f64, f64),
        (to_lat, to_lng): (f64, f64),
        via: &[(f64, f64)],
        exclude: bool,
    ) -> Result<OsrmRoute> {
        let mut coords = format!("{from_lng:.6},{from_lat:.6}");
        for (la, ln) in via {
            coords.push_str(&format!(";{ln:.6},{la:.6}"));
        }
        coords.push_str(&format!(";{to_lng:.6},{to_lat:.6}"));
        let mut url = format!(
            "{}/route/v1/driving/{coords}?overview=full&geometries=polyline&alternatives=false&steps=false",
            self.base_url
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
