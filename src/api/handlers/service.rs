//! `/service/*` — read-only lookups for other Apex services, gated by
//! [`crate::api::middleware::service_guard`]. Handlers here assume the
//! request is authorized and must never write anything.

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::routing::Departure;
use crate::state::AppState;
use crate::trips::best_trip::{pick, whole_drive, MIN_SHARE};
use crate::trips::geo::decode_polyline;

/// Shapes read per query, and the most pages one request reads (1,000
/// shapes, ~15 MB of polyline at the long end) looking for a whole drive.
const SHAPE_PAGE: usize = 50;
const MAX_SHAPE_PAGES: usize = 20;
use crate::trips::driven::{summarise, DrivenSummary};

#[derive(Deserialize)]
pub struct DrivenKmQuery {
    pub terminal: Option<String>,
    pub drop_off: Option<String>,
}

#[derive(Serialize)]
pub struct DrivenKmResponse {
    pub terminal: String,
    pub drop_off: String,
    #[serde(flatten)]
    pub summary: DrivenSummary,
}

/// `GET /service/trip-audit/driven-km?terminal=&drop_off=`
///
/// How far our trucks actually drove one terminal → drop-off route, from
/// the audit's stored outbound legs (see [`crate::trips::driven`]). The
/// route is physical, so trips of every company count.
pub async fn driven_km(
    State(s): State<AppState>,
    Query(q): Query<DrivenKmQuery>,
) -> AppResult<Json<DrivenKmResponse>> {
    let terminal = required(q.terminal, "terminal")?;
    let drop_off = required(q.drop_off, "drop_off")?;

    let legs = s
        .trip_repo
        .outbound_legs_for_route(&terminal, &drop_off)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(DrivenKmResponse {
        terminal,
        drop_off,
        summary: summarise(&legs),
    }))
}

#[derive(Deserialize)]
pub struct RoutesQuery {
    pub terminal: Option<String>,
    pub drop_off: Option<String>,
    /// The two pins, for Valhalla's truck route. Optional: without them the
    /// answer still carries the best trip.
    pub from_lat: Option<f64>,
    pub from_lng: Option<f64>,
    pub to_lat: Option<f64>,
    pub to_lng: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct TruckRouteBody {
    pub distance_km: f64,
    pub duration_min: f64,
    /// polyline5, on the road network.
    pub geometry: String,
    /// Always "any_hour": legal whenever the truck leaves.
    pub departure: &'static str,
}

#[derive(Debug, Serialize)]
pub struct BestTripBody {
    pub km: f64,
    /// Terminal geofence exit to drop-off geofence entry, stops included.
    pub drive_min: f64,
    pub day: String,
    pub depart_ts: String,
    pub arrive_ts: String,
    /// polyline5, map-matched by Valhalla from the truck's GPS.
    pub geometry: String,
}

#[derive(Debug, Serialize)]
pub struct RoutesResponse {
    pub terminal: String,
    pub drop_off: String,
    /// null when no pins were sent or Valhalla could not route.
    pub truck_route: Option<TruckRouteBody>,
    /// null when no road-matched trip clears the floor.
    pub best_trip: Option<BestTripBody>,
    /// Road-matched trips with a shape on this route.
    pub trips_considered: usize,
    /// Readings shorter than the best trip that are not the whole route
    /// (GPS gaps, partial tracks); see best_trip.rs.
    pub set_aside: usize,
    pub min_share: f64,
}

/// `GET /service/trip-audit/routes?terminal=&drop_off=&from_lat=&from_lng=&to_lat=&to_lng=`
///
/// Two drives of one route for the fee lookup's map, both on the road
/// network: Valhalla's truck route between the pins (legal at any hour), and
/// the shortest real drive our trucks made (see [`crate::trips::best_trip`]).
pub async fn routes(
    State(s): State<AppState>,
    Query(q): Query<RoutesQuery>,
) -> AppResult<Json<RoutesResponse>> {
    let terminal = required(q.terminal, "terminal")?;
    let drop_off = required(q.drop_off, "drop_off")?;
    let pins = match (q.from_lat, q.from_lng, q.to_lat, q.to_lng) {
        (Some(a), Some(b), Some(c), Some(d)) => {
            let ok = |lat: f64, lng: f64| lat.abs() <= 90.0 && lng.abs() <= 180.0;
            if !ok(a, b) || !ok(c, d) {
                return Err(AppError::BadRequest("pin coordinates out of range".into()));
            }
            Some(((a, b), (c, d)))
        }
        (None, None, None, None) => None,
        _ => return Err(AppError::BadRequest("send all four pin coordinates or none".into())),
    };

    let truck = match pins {
        None => None,
        Some((from, to)) => match s.routing.route(from, to, &[], Departure::AnyHour).await {
            Ok(r) => Some(TruckRouteBody {
                distance_km: round1(r.distance_km),
                duration_min: round1(r.duration_secs as f64 / 60.0),
                geometry: r.geometry,
                departure: "any_hour",
            }),
            Err(e) => {
                tracing::warn!(error = %e, %terminal, %drop_off, "truck route for the fee lookup failed");
                None
            }
        },
    };

    let candidates = s
        .trip_repo
        .best_trip_candidates(&terminal, &drop_off)
        .await
        .map_err(AppError::Internal)?;
    let choice = pick(&candidates, truck.as_ref().map(|t| t.distance_km));

    // From the shortest up, the first whole drive (see best_trip.rs). On a
    // route through a coverage hole most trips carry a gap: Somed -> Qena's
    // first whole drive was its ~115th shortest reading. Shapes are read a
    // page at a time and the walk stops at the first whole one.
    let mut set_aside = choice.below_floor;
    let mut best = None;
    'pages: for page in choice.ordered.chunks(SHAPE_PAGE).take(MAX_SHAPE_PAGES) {
        let ids: Vec<i64> = page.iter().map(|c| c.id).collect();
        let mut drives = s.trip_repo.leg_drives(&ids).await.map_err(AppError::Internal)?;
        for c in page {
            let Some(d) = drives.remove(&c.id) else { continue };
            if whole_drive(&decode_polyline(&d.actual_geometry), pins).is_err() {
                set_aside += 1;
                continue;
            }
            best = Some(BestTripBody {
                km: round1(d.actual_km),
                drive_min: round1(d.actual_secs as f64 / 60.0),
                day: d.day_local,
                depart_ts: d.depart_ts,
                arrive_ts: d.arrive_ts,
                geometry: d.actual_geometry,
            });
            break 'pages;
        }
    }

    Ok(Json(RoutesResponse {
        terminal,
        drop_off,
        truck_route: truck,
        best_trip: best,
        trips_considered: choice.considered,
        set_aside,
        min_share: MIN_SHARE,
    }))
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn required(v: Option<String>, name: &str) -> AppResult<String> {
    match v.map(|s| s.trim().to_string()) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(AppError::BadRequest(format!("{name} is required"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_parameters_are_rejected() {
        assert!(required(None, "terminal").is_err());
        assert!(required(Some("  ".into()), "terminal").is_err());
        assert_eq!(
            required(Some(" Tanta ".into()), "drop_off").unwrap(),
            "Tanta"
        );
    }

    #[test]
    fn response_is_flat_and_keeps_nulls() {
        let body = DrivenKmResponse {
            terminal: "Alexandria".into(),
            drop_off: "Tanta".into(),
            summary: summarise(&[]),
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["terminal"], "Alexandria");
        assert_eq!(v["trips"], 0);
        // Every field present, null where it does not apply.
        for key in ["median_km", "p10_km", "p90_km", "first_day", "latest_day"] {
            assert!(
                v.get(key).is_some_and(|x| x.is_null()),
                "{key} should be present and null"
            );
        }
    }
}
