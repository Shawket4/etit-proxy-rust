//! Tiny request/response DTOs that don't fit naturally on the domain types.
//!
//! Everything here is API-shape only. Domain types live in `crate::domain`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    /// `YYYY-MM-DD` in the configured timezone. Mutually exclusive with
    /// `from`/`to`.
    pub date: Option<String>,
    /// `YYYY-MM-DDTHH:MM:SS` (configured-tz local) or full RFC3339.
    pub from: Option<String>,
    pub to: Option<String>,
    /// If true, bypass cache and force a fresh fetch from upstream.
    pub refresh: Option<bool>,
    /// "msgpack" for a MessagePack body; anything else (or absent) is JSON.
    pub format: Option<String>,
    /// "optimal" attaches each leg's OSRM-optimal polyline.
    pub include: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct InvalidateRequest {
    /// One of: "all", "vehicles", "live", "history".
    pub scope: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvalidateResponse {
    pub scope: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReloginResponse {
    pub ok: bool,
    pub epoch: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnblockResponse {
    /// The server accepted the event with no rejection signals. NOTE: the
    /// grid markup has no blocked flag, so this is "accepted", not a proof the
    /// user is now unblocked. `false` means the server refused it (see
    /// `accessDenied` / `upstreamMessage`).
    pub ok: bool,
    /// The user whose row was targeted (the hardcoded first row).
    pub target: String,
    /// Upstream HTTP status of the postback.
    pub http_status: u16,
    /// Response looked like the normal users grid.
    pub is_users_grid: bool,
    /// Response looked like an ASP.NET error / exception page.
    pub error_page: bool,
    /// Server denied the action (access-denied / unauthorized markers).
    pub access_denied: bool,
    /// Best-effort extracted alert/error message from upstream, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_message: Option<String>,
    /// One-line interpretation of the outcome.
    pub note: String,
    /// First slice of the response body when it wasn't a clean grid render.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_excerpt: Option<String>,
}

/// `POST /admin/routing/route` — one routed leg.
///
/// Exists so other services stop talking to OSRM directly. FalconGo used to
/// build its own `/route/v1/driving` URL for fee-mapping enrichment, which
/// meant it never applied the time-gated exclude this service applies, and the
/// two produced different distances for the same pair of points. One caller,
/// one rule.
#[derive(Debug, Deserialize)]
pub struct RouteRequest {
    pub from_lat: f64,
    pub from_lng: f64,
    pub to_lat: f64,
    pub to_lng: f64,
    /// Intermediate waypoints, in order.
    #[serde(default)]
    pub via: Vec<[f64; 2]>,
    /// Whether the time-gated truck roads must be excluded — i.e. whether the
    /// vehicle is moving outside the tanker permit window. Defaults to true,
    /// which is the conservative reading: a route that avoids restricted roads
    /// is legal at any hour, whereas one that uses them is not.
    #[serde(default = "default_restricted")]
    pub restricted: bool,
}

fn default_restricted() -> bool {
    true
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteResponse {
    pub distance_km: f64,
    pub duration_min: f64,
    /// polyline5-encoded geometry, for callers that draw the route.
    pub geometry: String,
    /// Echoed so a caller can record which rule produced the number.
    pub restricted: bool,
}
