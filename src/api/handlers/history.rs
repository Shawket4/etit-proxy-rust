//! `/api/v1/vehicles/:id/history` handler.

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDate;
use serde::Serialize;

use crate::api::dto::HistoryQuery;
use crate::domain::history::{decode_polyline, HistoryLeg, SignificantPin};
use crate::domain::time::Timestamp;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// JSON by default; MessagePack (named maps, the encoding apex-react already
/// speaks for apex-rust) when the caller asks with `format=msgpack`.
fn respond<T: Serialize>(payload: &T, msgpack: bool) -> AppResult<Response> {
    if msgpack {
        let bytes = rmp_serde::to_vec_named(payload)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("msgpack encode: {e}")))?;
        Ok((
            [(axum::http::header::CONTENT_TYPE, "application/msgpack")],
            bytes,
        )
            .into_response())
    } else {
        Ok(axum::Json(payload).into_response())
    }
}

fn parse_iso(s: &str) -> Option<Timestamp> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// Assemble the audit-derived pins AND legs for one vehicle window from a
/// single query over the matched legs.
///
/// Pins: one per named place — terminals, drop-offs, garages, nothing else —
/// with dwell = arrival to the next leg's departure within the same trip;
/// coordinates from the stored leg geometry endpoints so pins land exactly
/// where the drawn track does.
///
/// Legs: time-ranges over the response's own points (no actual geometry —
/// the frontend slices the loaded track, which also self-clips legs that
/// cross the window edge; the boundary flags say when that happened).
async fn build_pins_and_legs(
    s: &AppState,
    vehicle_id: &str,
    from: Timestamp,
    to: Timestamp,
) -> (Vec<SignificantPin>, Vec<HistoryLeg>) {
    let from_iso = from.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let to_iso = to.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let legs = match s.trip_repo.legs_for_vehicle(vehicle_id, &from_iso, &to_iso).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(vehicle_id, error = %e, "audit legs query failed; returning none");
            return (Vec::new(), Vec::new());
        }
    };

    let out_legs: Vec<HistoryLeg> = legs
        .iter()
        .map(|l| {
            let depart = parse_iso(&l.depart_ts);
            let arrive = parse_iso(&l.arrive_ts);
            HistoryLeg {
                parent_trip_id: l.parent_trip_id,
                seq: l.seq,
                leg_type: l.leg_type.clone(),
                from_name: l.from_name.clone(),
                to_name: l.to_name.clone(),
                depart,
                arrive,
                actual_km: l.actual_km,
                actual_secs: l.actual_secs,
                distance_method: l.distance_method.clone(),
                starts_before_window: depart.map_or(false, |d| d < from),
                ends_after_window: arrive.map_or(false, |a| a > to),
            }
        })
        .collect();

    let mut pins: Vec<SignificantPin> = Vec::new();
    let mut i = 0usize;
    while i < legs.len() {
        let leg = &legs[i];
        let geom = decode_polyline(&leg.actual_geometry);

        // The journey's starting place: pinned once, from the first leg of
        // each trip, at the geometry's first point.
        if i == 0 || legs[i - 1].parent_trip_id != leg.parent_trip_id {
            if let Some(&(lat, lng)) = geom.first() {
                pins.push(SignificantPin {
                    name: leg.from_name.clone(),
                    kind: kind_for(&leg.leg_type, true),
                    lat,
                    lng,
                    arrive: None,
                    depart: parse_iso(&leg.depart_ts),
                    dwell_secs: None,
                    parent_trip_id: Some(leg.parent_trip_id),
                });
            }
        }

        // The arrival place; dwell runs until the same trip's next leg
        // departs (None = trip ended here or return still pending).
        if let Some(&(lat, lng)) = geom.last() {
            let next = legs
                .get(i + 1)
                .filter(|n| n.parent_trip_id == leg.parent_trip_id);
            let arrive = parse_iso(&leg.arrive_ts);
            let depart = next.and_then(|n| parse_iso(&n.depart_ts));
            let dwell = match (arrive, depart) {
                (Some(a), Some(d)) => Some((d - a).num_seconds().max(0)),
                _ => None,
            };
            pins.push(SignificantPin {
                name: leg.to_name.clone(),
                kind: kind_for(&leg.leg_type, false),
                lat,
                lng,
                arrive,
                depart,
                dwell_secs: dwell,
                parent_trip_id: Some(leg.parent_trip_id),
            });
        }
        i += 1;
    }

    pins.sort_by_key(|p| p.arrive.or(p.depart));
    (pins, out_legs)
}

fn kind_for(leg_type: &str, is_origin: bool) -> String {
    match (leg_type, is_origin) {
        // A trip's first departure point is the loading terminal.
        (_, true) => "terminal".into(),
        ("return", false) => "terminal".into(),
        ("garage", false) => "garage".into(),
        _ => "dropoff".into(),
    }
}

/// `GET /api/v1/vehicles/:id/history?date=YYYY-MM-DD`  
/// `GET /api/v1/vehicles/:id/history?from=...&to=...`
///
/// Without any query parameters, defaults to **today** in the configured
/// timezone. `date` and `from`/`to` are mutually exclusive: providing both
/// is a `BAD_REQUEST`.
pub async fn for_vehicle(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> AppResult<Response> {
    if id.is_empty() {
        return Err(AppError::BadRequest("vehicle id required".into()));
    }

    let force = q.refresh.unwrap_or(false);
    let msgpack = q.format.as_deref() == Some("msgpack");

    let mut resp = match (q.date.as_deref(), q.from.as_deref(), q.to.as_deref()) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            return Err(AppError::BadRequest(
                "use either `date` OR `from`+`to`, not both".into(),
            ))
        }
        (_, Some(f), Some(t)) => s.history.for_range(&id, f, t, force).await?,
        (_, Some(_), None) | (_, None, Some(_)) => {
            return Err(AppError::BadRequest(
                "`from` and `to` must be provided together".into(),
            ))
        }
        (Some(d), _, _) => {
            let date = NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").map_err(|e| {
                AppError::BadRequest(format!("invalid `date` (YYYY-MM-DD): {e}"))
            })?;
            s.history.for_day(&id, date, force).await?
        }
        (None, None, None) => {
            let today = s.cache.today_local();
            s.history.for_day(&id, today, force).await?
        }
    };

    let (pins, legs) = build_pins_and_legs(&s, &id, resp.from, resp.to).await;
    resp.pins = pins;
    resp.legs = legs;
    respond(&resp, msgpack)
}

/// `GET /api/v1/vehicles/:id/history/summary?from=...&to=...`
///
/// Computed locally from the cached GPS points/stops/ignition events inside
/// the exact window — never from ETIT's summary endpoint, whose numbers keep
/// shifting for the same query while their ingest settles. Fully dynamic:
/// nothing is stored, any window shape works.
pub async fn summary(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> AppResult<Response> {
    let (from, to) = match (q.date.as_deref(), q.from.as_deref(), q.to.as_deref()) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            return Err(AppError::BadRequest(
                "use either `date` OR `from`+`to`, not both".into(),
            ))
        }
        (_, Some(f), Some(t)) => (f.to_string(), t.to_string()),
        (_, Some(_), None) | (_, None, Some(_)) => {
            return Err(AppError::BadRequest(
                "`from` and `to` must be provided together".into(),
            ))
        }
        (Some(d), _, _) => {
            // date shortcut -> full day range
            let from = format!("{d} 00:00:00");
            let to = format!("{d} 23:59:59");
            (from, to)
        }
        (None, None, None) => {
            let today = s.cache.today_local();
            let from = today.format("%Y-%m-%d 00:00:00").to_string();
            let to = today.format("%Y-%m-%d 23:59:59").to_string();
            (from, to)
        }
    };

    let force = q.refresh.unwrap_or(false);
    let msgpack = q.format.as_deref() == Some("msgpack");
    let summary = s.history.computed_summary(&id, &from, &to, force).await?;
    respond(&summary, msgpack)
}
