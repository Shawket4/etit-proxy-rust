//! Road routing for the whole service: Valhalla, planned for our tankers.
//!
//! Everything that asks "how far is it by road" goes through here: the trip
//! audit, `/admin/routing/*` (FalconGo, the advisor), and `/service/*` (the
//! fee lookup). One engine, one truck, one definition of a legal route.
//!
//! - [`valhalla`] — the HTTP client: truck routes at a departure time, map
//!   matching that returns the OSM ways driven, and distance matrices.
//! - [`bans`]     — the roads closed to the fleet, built into the tiles, by
//!   way id, so a matched trace can be checked against them.
//! - [`roads`]    — the major roads a route takes, in order, for the fee
//!   lookup's list (read from Valhalla's edges by the client).
//!
//! Closed roads are data, not request flags: `valhalla/build.sh` writes them
//! into the map (`hgv=no` when closed at every hour, `hgv:conditional` when
//! closed at some hours), and every route here carries a departure time, so
//! Valhalla applies them itself.

pub mod bans;
pub mod roads;
pub mod valhalla;

pub use bans::{Ban, TimeBans};
pub use roads::{RoadDifference, RoadStretch};
pub use valhalla::{Departure, MatchedTrace, OptimizedTrip, Route, RoutingClient};

/// The vehicle every route is planned for. Defaults are Apex's 40 t fuel
/// tanker; each is overridable from the environment (`TRUCK_*`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Truck {
    pub height_m: f64,
    pub width_m: f64,
    pub length_m: f64,
    pub weight_t: f64,
    pub axle_load_t: f64,
    pub axle_count: u32,
    /// Fuel is hazardous: avoid roads and tunnels tagged `hazmat=no`.
    pub hazmat: bool,
    pub top_speed_kmh: u32,
}

impl Default for Truck {
    fn default() -> Self {
        Self {
            height_m: 4.0,
            width_m: 2.6,
            length_m: 16.5,
            weight_t: 40.0,
            axle_load_t: 10.0,
            axle_count: 5,
            hazmat: true,
            top_speed_kmh: 90,
        }
    }
}
