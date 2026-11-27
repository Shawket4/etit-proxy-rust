//! Valhalla HTTP client.
//!
//! Three calls, all planned for the configured [`Truck`]:
//!
//! - [`RoutingClient::route`]: the best legal route for a truck leaving at
//!   a given time. Closed roads live in the tiles (`hgv=no`, or
//!   `hgv:conditional` for a timed ban), so the departure time alone decides
//!   whether a timed one may be used: no exclude flag, no night-window logic
//!   here.
//! - [`RoutingClient::match_trace`]: snaps a timestamped GPS trace to the
//!   roads it drove (Valhalla's Meili). It returns the OSM ways driven and
//!   when, which is what lets the audit check them against the bans.
//! - [`RoutingClient::matrix`]: one call for every pairwise distance, for the
//!   best-drop-order check.
//!
//! Valhalla returns precision-6 shapes. Everything leaving this module is
//! precision 5, the format this service has always stored and served.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use arc_swap::ArcSwap;
use chrono::TimeZone;
use serde::Deserialize;
use serde_json::{json, Value};

use super::bans::TimeBans;
use super::Truck;
use crate::trips::geo::decode_polyline6;
use crate::trips::matcher::encode_track;
use crate::zones::geo::haversine_m;

/// When the truck leaves, which decides which time bans apply.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Departure {
    /// Leaving at this instant (epoch seconds).
    At(i64),
    /// Must be legal at any hour, e.g. a stored reference distance. Routed as
    /// leaving inside the ban windows, so every time-banned road is avoided.
    AnyHour,
}

#[derive(Debug, Clone)]
pub struct Route {
    pub distance_km: f64,
    pub duration_secs: i64,
    /// polyline5.
    pub geometry: String,
}

/// One OSM way the matched path drove, with when the truck was on it.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchedEdge {
    pub way_id: i64,
    pub names: Vec<String>,
    pub length_km: f64,
    /// First and last GPS fix matched onto this edge (epoch seconds).
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct MatchedTrace {
    /// Road distance along the matched path, km. Gaps the matcher could not
    /// bridge add their chord, so the total never undercuts the trace.
    pub distance_km: f64,
    /// polyline5 per continuous matched stretch, in order.
    pub geometries: Vec<String>,
    /// Fraction of input fixes matched to a road (0..1).
    pub coverage: f64,
    pub edges: Vec<MatchedEdge>,
}

/// A round (or open) trip with its stops in the best order.
#[derive(Debug, Clone)]
pub struct OptimizedTrip {
    /// Stop indices (0-based, as given) in visiting order.
    pub order: Vec<usize>,
    /// One route per hop: start → first stop … last stop → end.
    pub legs: Vec<Route>,
    pub distance_km: f64,
    pub duration_secs: i64,
    /// The whole trip as one polyline5.
    pub geometry: String,
}

#[derive(Clone)]
pub struct RoutingClient {
    http: reqwest::Client,
    base_url: String,
    truck: Truck,
    tz: chrono_tz::Tz,
    bans: Arc<ArcSwap<TimeBans>>,
    bans_path: Option<PathBuf>,
}

impl RoutingClient {
    pub fn new(
        http: reqwest::Client,
        base_url: String,
        truck: Truck,
        tz: chrono_tz::Tz,
        bans_path: Option<PathBuf>,
    ) -> Self {
        let client = Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            truck,
            tz,
            bans: Arc::new(ArcSwap::from_pointee(TimeBans::default())),
            bans_path,
        };
        client.reload_bans();
        client
    }

    /// Re-reads the ban file (it changes with each tiles deploy). Keeps the
    /// previous set if the file cannot be read. Returns the ways now known.
    pub fn reload_bans(&self) -> usize {
        let Some(path) = &self.bans_path else { return 0 };
        match TimeBans::load(path) {
            Ok(b) => {
                let n = b.way_count();
                self.bans.store(Arc::new(b));
                n
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "time bans not loaded; ban flags off");
                self.bans.load().way_count()
            }
        }
    }

    pub fn bans(&self) -> Arc<TimeBans> {
        self.bans.load_full()
    }

    pub fn truck(&self) -> &Truck {
        &self.truck
    }

    fn truck_costing(&self) -> Value {
        let t = &self.truck;
        json!({ "truck": {
            "height": t.height_m,
            "width": t.width_m,
            "length": t.length_m,
            "weight": t.weight_t,
            "axle_load": t.axle_load_t,
            "axle_count": t.axle_count,
            "hazmat": t.hazmat,
            "top_speed": t.top_speed_kmh,
        }})
    }

    /// Valhalla's `date_time`: depart-at, in the origin's local time.
    fn date_time(&self, dep: Departure) -> Value {
        let epoch = match dep {
            Departure::At(ts) => ts,
            Departure::AnyHour => self.any_hour_epoch(),
        };
        let local = self
            .tz
            .timestamp_opt(epoch, 0)
            .single()
            .map(|d| d.format("%Y-%m-%dT%H:%M").to_string())
            .unwrap_or_default();
        json!({ "type": 1, "value": local })
    }

    /// Today, two hours into the first timed ban's window (09:00 for a 07:00
    /// ban). That leaves the rest of the window for the trip to pass the
    /// banned roads while still inside it. With no timed bans the hour makes
    /// no difference (roads closed at every hour are closed then too) and
    /// 09:00 stands in.
    fn any_hour_epoch(&self) -> i64 {
        let bans = self.bans.load();
        let at = bans
            .reference_time()
            .unwrap_or_else(|| chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        let today = chrono::Utc::now().with_timezone(&self.tz).date_naive();
        self.tz
            .from_local_datetime(&today.and_time(at))
            .earliest()
            .map(|d| d.timestamp())
            .unwrap_or_else(|| chrono::Utc::now().timestamp())
    }

    async fn post(&self, endpoint: &str, body: &Value) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{}/{endpoint}", self.base_url))
            .json(body)
            .send()
            .await
            .with_context(|| format!("Valhalla {endpoint} unreachable"))?;
        let status = resp.status();
        let v: Value = resp.json().await.with_context(|| format!("Valhalla {endpoint}: unreadable answer"))?;
        if !status.is_success() {
            let msg = v.get("error").and_then(Value::as_str).unwrap_or("");
            let code = v.get("error_code").and_then(Value::as_i64).unwrap_or(0);
            return Err(anyhow!("Valhalla {endpoint} {status}: {msg} ({code})"));
        }
        Ok(v)
    }

    /// Best legal route from → to (optionally through `via`) for a truck
    /// leaving at `dep`.
    pub async fn route(
        &self,
        from: (f64, f64),
        to: (f64, f64),
        via: &[(f64, f64)],
        dep: Departure,
    ) -> Result<Route> {
        let mut locations = vec![json!({ "lat": from.0, "lon": from.1, "type": "break" })];
        for (la, ln) in via {
            locations.push(json!({ "lat": la, "lon": ln, "type": "through" }));
        }
        locations.push(json!({ "lat": to.0, "lon": to.1, "type": "break" }));

        let v = self
            .post(
                "route",
                &json!({
                    "locations": locations,
                    "costing": "truck",
                    "costing_options": self.truck_costing(),
                    "date_time": self.date_time(dep),
                    "units": "kilometers",
                    "directions_type": "none",
                }),
            )
            .await?;
        parse_route(&v)
    }

    /// Visit `stops` in the best order (Valhalla's optimized_route, which
    /// minimises truck time), starting at `start` and ending at `end`.
    pub async fn optimize(
        &self,
        start: (f64, f64),
        stops: &[(f64, f64)],
        end: (f64, f64),
        dep: Departure,
    ) -> Result<OptimizedTrip> {
        if stops.is_empty() {
            return Err(anyhow!("optimize needs at least one stop"));
        }
        let locations: Vec<Value> = std::iter::once(start)
            .chain(stops.iter().copied())
            .chain(std::iter::once(end))
            .map(|(la, ln)| json!({ "lat": la, "lon": ln }))
            .collect();
        let v = self
            .post(
                "optimized_route",
                &json!({
                    "locations": locations,
                    "costing": "truck",
                    "costing_options": self.truck_costing(),
                    "date_time": self.date_time(dep),
                    "units": "kilometers",
                    "directions_type": "none",
                }),
            )
            .await?;
        parse_optimized(&v, stops.len())
    }

    /// Pairwise truck distances, km: `m[i][j]` is i → j (directed: one-way
    /// streets and bans make i→j and j→i differ). `None` where unreachable.
    pub async fn matrix(&self, points: &[(f64, f64)], dep: Departure) -> Result<Vec<Vec<Option<f64>>>> {
        let locs: Vec<Value> = points.iter().map(|(la, ln)| json!({ "lat": la, "lon": ln })).collect();
        let v = self
            .post(
                "sources_to_targets",
                &json!({
                    "sources": locs,
                    "targets": locs,
                    "costing": "truck",
                    "costing_options": self.truck_costing(),
                    "date_time": self.date_time(dep),
                    "units": "kilometers",
                    "verbose": true,
                }),
            )
            .await?;
        parse_matrix(&v, points.len())
    }

    /// Snap a timestamped trace ((lat, lng, epoch_secs)) to the roads driven.
    ///
    /// Matching uses car costing with access, restrictions and closures
    /// ignored: the actual is the actual, including a banned road or one the
    /// truck was not allowed on. Those are exactly what the audit wants to see.
    pub async fn match_trace(&self, points: &[(f64, f64, i64)]) -> Result<MatchedTrace> {
        if points.len() < 2 {
            return Err(anyhow!("trace too short to match"));
        }
        let shape: Vec<Value> = points
            .iter()
            .map(|(la, ln, t)| json!({ "lat": la, "lon": ln, "time": t }))
            .collect();
        let v = self
            .post(
                "trace_attributes",
                &json!({
                    "shape": shape,
                    "costing": "auto",
                    "costing_options": { "auto": {
                        "ignore_access": true,
                        "ignore_restrictions": true,
                        "ignore_closures": true,
                    }},
                    "shape_match": "map_snap",
                    "use_timestamps": true,
                    "units": "kilometers",
                    "filters": {
                        "attributes": [
                            "edge.way_id", "edge.names", "edge.length",
                            "matched.type", "matched.edge_index", "matched.point",
                            "shape",
                        ],
                        "action": "include",
                    },
                }),
            )
            .await?;
        parse_trace(&v, points)
    }
}

// ---- parsing ---------------------------------------------------------------
// Kept free of I/O so the shapes Valhalla answers with are unit-tested.

#[derive(Deserialize)]
struct RouteResponse {
    trip: Trip,
}

#[derive(Deserialize)]
struct Trip {
    summary: Summary,
    #[serde(default)]
    legs: Vec<Leg>,
}

#[derive(Deserialize)]
struct Summary {
    length: f64,
    time: f64,
}

#[derive(Deserialize)]
struct Leg {
    #[serde(default)]
    shape: String,
}

fn to_polyline5(shape6: &str) -> String {
    encode_track(&decode_polyline6(shape6))
}

pub(crate) fn parse_route(v: &Value) -> Result<Route> {
    let r: RouteResponse = serde_json::from_value(v.clone()).context("Valhalla route: unexpected answer")?;
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for leg in &r.trip.legs {
        let mut p = decode_polyline6(&leg.shape);
        // Consecutive legs share their joining point.
        if !pts.is_empty() && !p.is_empty() && pts.last() == p.first() {
            p.remove(0);
        }
        pts.extend(p);
    }
    Ok(Route {
        distance_km: r.trip.summary.length,
        duration_secs: r.trip.summary.time.round() as i64,
        geometry: encode_track(&pts),
    })
}

pub(crate) fn parse_optimized(v: &Value, n_stops: usize) -> Result<OptimizedTrip> {
    let trip = v.get("trip").ok_or_else(|| anyhow!("Valhalla optimized_route: no trip"))?;
    let order: Vec<usize> = trip
        .get("locations")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Valhalla optimized_route: no locations"))?
        .iter()
        .filter_map(|l| l.get("original_index").and_then(Value::as_u64))
        .map(|i| i as usize)
        // Original indices 1..=n are the stops; 0 and n+1 are start and end.
        .filter(|&i| i >= 1 && i <= n_stops)
        .map(|i| i - 1)
        .collect();
    let mut seen = order.clone();
    seen.sort_unstable();
    seen.dedup();
    if order.len() != n_stops || seen.len() != n_stops {
        return Err(anyhow!("Valhalla optimized_route: {} of {n_stops} stops ordered", seen.len()));
    }
    let mut whole: Vec<(f64, f64)> = Vec::new();
    let legs: Vec<Route> = trip
        .get("legs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|l| {
            let mut pts = decode_polyline6(l.get("shape").and_then(Value::as_str).unwrap_or(""));
            let geometry = encode_track(&pts);
            if !whole.is_empty() && !pts.is_empty() && whole.last() == pts.first() {
                pts.remove(0);
            }
            whole.extend(pts);
            Route {
                distance_km: l.pointer("/summary/length").and_then(Value::as_f64).unwrap_or(0.0),
                duration_secs: l.pointer("/summary/time").and_then(Value::as_f64).unwrap_or(0.0).round() as i64,
                geometry,
            }
        })
        .collect();
    Ok(OptimizedTrip {
        order,
        distance_km: legs.iter().map(|l| l.distance_km).sum(),
        duration_secs: legs.iter().map(|l| l.duration_secs).sum(),
        geometry: encode_track(&whole),
        legs,
    })
}

pub(crate) fn parse_matrix(v: &Value, n: usize) -> Result<Vec<Vec<Option<f64>>>> {
    let rows = v
        .get("sources_to_targets")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Valhalla matrix: no sources_to_targets"))?;
    let mut m = vec![vec![None; n]; n];
    for row in rows {
        for cell in row.as_array().into_iter().flatten() {
            let (Some(i), Some(j)) = (
                cell.get("from_index").and_then(Value::as_u64),
                cell.get("to_index").and_then(Value::as_u64),
            ) else {
                continue;
            };
            if let (Some(slot), Some(d)) = (
                m.get_mut(i as usize).and_then(|r| r.get_mut(j as usize)),
                cell.get("distance").and_then(Value::as_f64),
            ) {
                *slot = Some(d);
            }
        }
    }
    Ok(m)
}

#[derive(Deserialize)]
struct TraceResponse {
    #[serde(default)]
    edges: Vec<TraceEdge>,
    #[serde(default)]
    matched_points: Vec<TracePoint>,
    #[serde(default)]
    shape: String,
}

#[derive(Deserialize)]
struct TraceEdge {
    #[serde(default)]
    way_id: i64,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    length: f64,
}

#[derive(Deserialize)]
struct TracePoint {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    edge_index: Option<i64>,
}

pub(crate) fn parse_trace(v: &Value, input: &[(f64, f64, i64)]) -> Result<MatchedTrace> {
    let r: TraceResponse = serde_json::from_value(v.clone()).context("Valhalla trace_attributes: unexpected answer")?;
    if r.edges.is_empty() {
        return Err(anyhow!("Valhalla trace_attributes: nothing matched"));
    }

    // Times per edge, from the fixes matched onto it.
    let mut times: HashMap<usize, (i64, i64)> = HashMap::new();
    let mut matched = 0usize;
    for (i, p) in r.matched_points.iter().enumerate() {
        if p.kind != "matched" && p.kind != "interpolated" {
            continue;
        }
        matched += 1;
        if let (Some(e), Some((_, _, t))) = (p.edge_index, input.get(i)) {
            if e >= 0 {
                let slot = times.entry(e as usize).or_insert((*t, *t));
                slot.0 = slot.0.min(*t);
                slot.1 = slot.1.max(*t);
            }
        }
    }

    let edges: Vec<MatchedEdge> = r
        .edges
        .into_iter()
        .enumerate()
        .map(|(i, e)| MatchedEdge {
            way_id: e.way_id,
            names: e.names,
            length_km: e.length,
            first_ts: times.get(&i).map(|t| t.0),
            last_ts: times.get(&i).map(|t| t.1),
        })
        .collect();

    // The matched path's own geometry is the distance. Where the trace has
    // a GPS gap longer than the matcher's breakage distance, Valhalla joins
    // the two sides with a straight segment in this shape (and no edge), so
    // the gap counts as its chord, never as nothing.
    let path = decode_polyline6(&r.shape);
    let path_m: f64 = path.windows(2).map(|w| haversine_m(w[0].0, w[0].1, w[1].0, w[1].1)).sum();

    Ok(MatchedTrace {
        distance_km: path_m / 1000.0,
        geometries: vec![to_polyline5(&r.shape)],
        coverage: if input.is_empty() { 0.0 } else { matched as f64 / input.len() as f64 },
        edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real Valhalla 3.9.3 answers on the Egypt tiles, trimmed to the fields read.
    const ROUTE: &str = r#"{"trip":{"summary":{"length":4.007,"time":332.219},"legs":[{"shape":"_awix@_hlpz@zJtEyBnGyRbk@oTtv@gH`Ayg@`IeEHke@fG{UdEyw@`P_CFqAU{@q@eF_E{^aWuc@uZeTbl@sH`UvRzBrJv@jMLtb@a@tG|@`C@lDWraAwSlHg@rDVbDfAxBpAlBhBxA~BfAnCz@~FjAxFpBrFxB~QTtKxBr\\j@xGlEhe@|Fbn@`LldAbH|n@fAdIjAtHrAxHdMlw@dOfjAm@rCc@vCYxCO|CEzCB|CbAzt@@pCQnCa@jCu@bCcAxBoAhB}A|AgBjAoBx@uBd@wBRqKD}AE{AOwAa@sAs@gAaA}@sAo@}Aa@gBQkBAmBIgO`A}AlWob@dJeMhDaCpCuArFw@nFk@zFO|EHrCN~C\\~FnApF|BxCnBhAt@pEfDpF`F~LhLzPhOt`BbxAlFpEvDjDfNtLz_@b\\f]xZzXhV~ErEl`@~^iBpDmBpFeBzFq@~BuCbMqBfLwAlL}@tLa@xLExLJzLVvG|@rMzAjMzB`MvCtLrDfLxBxFvBlEP^dD|FpDpF|DdFhEvEfAbAEdBO`B[`Bc@zAi@~AaSj^oVqWcD}DuTb\\mI{JcHmJqF}KaFwJ}BaH}CaNcBqN{AcOfVqA"}]}}"#;

    const TRACE: &str = r#"{"edges":[{"way_id":376242789,"length":0.023,"names":["شارع شجره الدر"]},{"way_id":24987129,"length":0.091,"names":["شارع ابن زنكي"]},{"way_id":25055007,"length":0.062,"names":["شارع 26 يوليو"]},{"way_id":324407510,"length":0.013}],
        "matched_points":[{"lon":31.218319,"lat":30.060575,"type":"matched","edge_index":0},{"lon":31.210959,"lat":30.059198,"type":"matched","edge_index":2},{"lon":31.203,"lat":30.0571,"type":"unmatched"}],
        "shape":"_awix@_hlpz@zJtEyBnGyRbk@oTtv@gH`Ayg@`IkGhBac@bHWD`E|CNrGb@nG`AvMvAzMxB~QTtKwIdRb@pClF}@vAWpKiBdEs@dD`BlC`CnBdDlAzDj@bFf@dEHlA~AbXtEtDzBl@`CJ~BEfa@oG`I|p@b@nD~Era@l[kErBo@lCPlCWlBrO`BjZr^qFJfBfEdh@"}"#;

    #[test]
    fn a_route_reads_its_summary_and_comes_out_as_polyline5() {
        let r = parse_route(&serde_json::from_str(ROUTE).unwrap()).unwrap();
        assert_eq!((r.distance_km, r.duration_secs), (4.007, 332));
        let pts = crate::trips::geo::decode_polyline(&r.geometry);
        assert!(pts.len() > 20);
        // Starts where the request did (Zamalek), read at precision 5.
        assert!((pts[0].0 - 30.0605).abs() < 1e-3 && (pts[0].1 - 31.2185).abs() < 1e-3, "{:?}", pts[0]);
    }

    #[test]
    fn legs_joined_through_a_via_do_not_repeat_the_joint() {
        let leg = |pts: &[(f64, f64)]| {
            // Encode at precision 6 by scaling into the precision-5 encoder.
            let scaled: Vec<(f64, f64)> = pts.iter().map(|(a, b)| (a * 10.0, b * 10.0)).collect();
            encode_track(&scaled)
        };
        let v = json!({"trip":{"summary":{"length":2.0,"time":100.0},"legs":[
            {"shape": leg(&[(30.0, 31.0), (30.01, 31.0)])},
            {"shape": leg(&[(30.01, 31.0), (30.02, 31.0)])}]}});
        let r = parse_route(&v).unwrap();
        assert_eq!(crate::trips::geo::decode_polyline(&r.geometry).len(), 3);
    }

    #[test]
    fn a_matrix_is_directed_and_keeps_unreachable_as_none() {
        let v = json!({"sources_to_targets":[
            [{"from_index":0,"to_index":0,"distance":0.0,"time":0},{"from_index":0,"to_index":1,"distance":29.755,"time":1922}],
            [{"from_index":1,"to_index":0,"distance":31.2,"time":2010},{"from_index":1,"to_index":1,"distance":null,"time":null}]]});
        let m = parse_matrix(&v, 2).unwrap();
        assert_eq!(m[0][1], Some(29.755));
        assert_eq!(m[1][0], Some(31.2));
        assert_eq!(m[1][1], None);
    }

    #[test]
    fn a_trace_reads_ways_times_and_coverage() {
        let input = [(30.0605, 31.2185, 1000), (30.0590, 31.2110, 1060), (30.0571, 31.2030, 1120)];
        let t = parse_trace(&serde_json::from_str(TRACE).unwrap(), &input).unwrap();
        assert!((t.coverage - 2.0 / 3.0).abs() < 1e-9, "the third fix was unmatched");
        assert_eq!(t.edges.len(), 4);
        assert_eq!(t.edges[2].way_id, 25055007);
        assert_eq!(t.edges[2].names, vec!["شارع 26 يوليو".to_string()]);
        assert_eq!((t.edges[0].first_ts, t.edges[2].first_ts), (Some(1000), Some(1060)));
        assert_eq!(t.edges[1].first_ts, None, "no fix landed on it");
        assert!(t.edges[3].names.is_empty(), "unnamed edges are fine");
        assert!(t.distance_km > 1.0 && t.distance_km < 5.0, "{}", t.distance_km);
    }

    #[test]
    fn an_optimized_trip_reports_stops_in_visiting_order() {
        // Real shape: start(0), stops 1..3, end(4); Valhalla visits 3, 1, 2.
        let v = json!({"trip":{
            "locations":[{"original_index":0},{"original_index":3},{"original_index":1},{"original_index":2},{"original_index":4}],
            "legs":[
                {"summary":{"length":68.6,"time":3660.0},"shape":""},
                {"summary":{"length":69.3,"time":3360.0},"shape":""},
                {"summary":{"length":43.1,"time":1920.0},"shape":""},
                {"summary":{"length":46.2,"time":2220.0},"shape":""}]}});
        let t = parse_optimized(&v, 3).unwrap();
        assert_eq!(t.order, vec![2, 0, 1]);
        assert_eq!(t.legs.len(), 4);
        assert!((t.distance_km - 227.2).abs() < 1e-9);
        assert_eq!(t.duration_secs, 11160);

        let mut lost = v.clone();
        lost["trip"]["locations"].as_array_mut().unwrap().remove(2);
        assert!(parse_optimized(&lost, 3).is_err(), "a lost stop is an error, not a shorter trip");
        let mut twice = v.clone();
        twice["trip"]["locations"][2]["original_index"] = json!(3);
        assert!(parse_optimized(&twice, 3).is_err(), "a stop visited twice is an error");
    }

    #[test]
    fn a_trace_with_nothing_matched_is_an_error() {
        let v = json!({"edges": [], "matched_points": [], "shape": ""});
        assert!(parse_trace(&v, &[(30.0, 31.0, 0), (30.1, 31.1, 60)]).is_err());
    }

    /// Against a stand-in Valhalla: what the client sends, and how it reads
    /// an error.
    #[tokio::test]
    async fn requests_carry_the_tanker_and_cairo_local_departure() {
        use axum::routing::post;
        use std::sync::Mutex;

        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let app = axum::Router::new()
            .route("/route", post(move |axum::Json(body): axum::Json<Value>| {
                let s2 = s2.clone();
                async move {
                    s2.lock().unwrap().push(body);
                    axum::Json(serde_json::from_str::<Value>(ROUTE).unwrap())
                }
            }))
            .route("/trace_attributes", post(|| async {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(json!({"error_code": 443, "error": "Exact route match algorithm failed to find path"})),
                )
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = RoutingClient::new(
            reqwest::Client::new(),
            format!("http://{addr}/"),
            Truck::default(),
            chrono_tz::Africa::Cairo,
            None,
        );
        // 11:00 UTC on 28 Nov is 13:00 in Cairo.
        let at = chrono::Utc.with_ymd_and_hms(2026, 11, 28, 11, 0, 0).unwrap().timestamp();
        let r = client.route((30.06, 31.21), (30.05, 31.20), &[(30.055, 31.205)], Departure::At(at)).await.unwrap();
        assert_eq!(r.distance_km, 4.007);

        let body = seen.lock().unwrap()[0].clone();
        assert_eq!(body["costing"], "truck");
        assert_eq!(body["date_time"], json!({"type": 1, "value": "2026-11-28T13:00"}));
        let truck = &body["costing_options"]["truck"];
        assert_eq!((truck["height"].as_f64(), truck["weight"].as_f64(), truck["hazmat"].as_bool()), (Some(4.0), Some(40.0), Some(true)));
        let locs = body["locations"].as_array().unwrap();
        assert_eq!(locs.len(), 3);
        assert_eq!(locs[1]["type"], "through");

        let err = client.match_trace(&[(30.0, 31.0, 0), (30.1, 31.1, 60)]).await.unwrap_err().to_string();
        assert!(err.contains("443") && err.contains("failed to find path"), "{err}");
    }

    #[tokio::test]
    async fn any_hour_departs_inside_the_ban_window() {
        let dir = std::env::temp_dir().join(format!("bans-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("apex-time-bans.json");
        std::fs::write(&path, r#"{"bans":[{"id":"x","label":"x","banned_from":"07:00","banned_to":"20:00","way_ids":[1,2]}]}"#).unwrap();
        let client = RoutingClient::new(reqwest::Client::new(), "http://127.0.0.1:9".into(), Truck::default(), chrono_tz::Africa::Cairo, Some(path));
        assert_eq!(client.bans().way_count(), 2);
        let v = client.date_time(Departure::AnyHour);
        assert!(v["value"].as_str().unwrap().ends_with("T09:00"), "{v}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
