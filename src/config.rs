//! Environment-driven configuration. All knobs flow through `Config`; no
//! module reads `std::env` directly.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono_tz::Tz;

#[derive(Debug, Clone)]
pub struct Config {
    // ---- Upstream ETIT ------------------------------------------------------
    /// Base URL for data calls (e.g. https://fms-gps.etit-eg.com)
    pub etit_data_url: String,
    /// Base URL for the login flow (e.g. https://etit-fms.etit-eg.com)
    pub etit_login_url: String,
    pub etit_username: String,
    pub etit_password: String,
    /// Optional overrides for the tenant/staff ids posted to data endpoints.
    /// Needed only if ETIT ever validates them again AND the post-login page
    /// stops embedding them; the server currently ignores the posted values.
    pub etit_sub_id: Option<String>,
    pub etit_staff_id: Option<String>,

    // ---- HTTP server --------------------------------------------------------
    pub bind_addr: String,
    /// Optional shared secret for `/admin/*`. None ⇒ admin routes disabled.
    pub admin_token: Option<String>,
    /// CORS origins; `*` allows any.
    pub cors_origins: Vec<String>,

    // ---- Persistence --------------------------------------------------------
    pub database_url: String,

    // ---- Timezone -----------------------------------------------------------
    pub timezone: Tz,

    // ---- Tiers --------------------------------------------------------------
    pub tier_a_refresh: Duration,
    pub tier_b_poll: Duration,
    pub tier_c_poll: Duration,
    pub tier_c_concurrency: usize,

    // ---- HTTP client tuning ------------------------------------------------
    pub upstream_timeout: Duration,
    pub upstream_retry_max: u32,
    pub upstream_retry_base: Duration,
    /// Optional HTTP/HTTPS proxy for all upstream calls.
    pub upstream_proxy: Option<String>,

    // ---- Logging ------------------------------------------------------------
    pub log_format_json: bool,

    // ---- Zones --------------------------------------------------------------
    pub whatsapp_service_url: String,
    pub whatsapp_zone_group_id: String,
    pub zone_scan_time: String,
    pub zone_lookback_days: i64,
    pub zone_scan_on_startup: bool,
    pub zone_min_dwell_secs: i64,
    pub zone_visit_gap_secs: i64,

    // ---- Trip audit ----------------------------------------------------------
    /// Read-only connection to the apex Postgres DB (trips, fee mappings,
    /// canonical locations). None ⇒ trip audit disabled.
    pub apex_database_url: Option<String>,
    /// OSRM routing endpoint, e.g. http://127.0.0.1:5000
    pub osrm_url: String,
    /// Excludable class for time-gated truck roads. Empty ⇒ never send an
    /// exclude parameter (required while the dataset is still the plain car
    /// profile, which has no such class).
    pub osrm_exclude_class: String,
    pub trip_scan_time: String,
    pub trip_scan_lookback_days: i64,
    pub trip_scan_on_startup: bool,
    /// Fuel-tanker permit window (local time). Departures inside the window
    /// may legally use the time-gated roads; departures outside it are
    /// routed with `osrm_exclude_class` excluded.
    pub tanker_night_start: chrono::NaiveTime,
    pub tanker_night_end: chrono::NaiveTime,
    /// Default geofence radii in meters; per-location overrides come from
    /// the apex DB (terminals.radius_m / drop_off_points.radius_m).
    pub terminal_radius_m: f64,
    pub dropoff_radius_m: f64,
    /// Flag thresholds.
    pub trip_flag_distance_ratio: f64,
    pub trip_flag_duration_ratio: f64,
    /// Interval for the pending-return closer: re-audits only the days
    /// carrying return_pending flags so open trips close within ~an hour of
    /// the truck's actual arrival instead of at the next nightly. 0 = off.
    pub trip_pending_closer_secs: u64,
    pub trip_unplanned_stop_secs: i64,
    pub trip_deviation_m: f64,
    /// Auto-apply a GPS pin suggestion to an UNPINNED place when its cluster
    /// has at least this many stops. Corrections to existing pins are never
    /// auto-applied.
    pub trip_pin_auto_apply_min_stops: i64,
    /// Stop waiting for a trip's terminal return after this long.
    pub trip_return_cap_secs: i64,
    /// Accept a map-matched leg distance when the matcher covered at least
    /// this fraction of the trace points.
    pub trip_match_min_coverage: f64,
    /// Off-optimal fraction of a matched route above which a deviation flag
    /// is raised (paired with the costs-distance condition).
    pub trip_off_route_flag_pct: f64,
    /// Fixed allowed dwell points (garages): (lat, lng, radius_m). A stop
    /// inside one is never flagged, and a dwell above
    /// `trip_allowed_point_dwell_secs` routes the leg's optimal via it.
    pub trip_allowed_points: Vec<(f64, f64, f64)>,
    pub trip_allowed_point_dwell_secs: i64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let _ = dotenvy::dotenv();

        let etit_login_url = trimmed_env("ETIT_LOGIN_URL")?;
        let etit_data_url  = trimmed_env("ETIT_DATA_URL")?;
        let etit_username  = required_env("ETIT_USERNAME")?;
        let etit_password  = required_env("ETIT_PASSWORD")?;
        let etit_sub_id    = optional_env("ETIT_SUB_ID");
        let etit_staff_id  = optional_env("ETIT_STAFF_ID");

        let bind_addr =
            std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

        let admin_token = std::env::var("ADMIN_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let cors_origins = std::env::var("CORS_ALLOWED_ORIGINS")
            .unwrap_or_else(|_| "*".into())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "sqlite://./data/etit.db?mode=rwc".into());

        let tz_name = std::env::var("TIMEZONE").unwrap_or_else(|_| "Africa/Cairo".into());
        let timezone = Tz::from_str(&tz_name)
            .map_err(|_| anyhow!("TIMEZONE `{tz_name}` is not a valid IANA zone"))?;

        let tier_a_refresh   = secs_or("TIER_A_REFRESH_SECS",   24 * 60 * 60)?;
        let tier_b_poll      = secs_or("TIER_B_POLL_SECS",      60)?;
        let tier_c_poll      = secs_or("TIER_C_POLL_SECS",      30 * 60)?;
        let tier_c_concurrency = parse_or("TIER_C_CONCURRENCY",  4_usize)?;

        let upstream_timeout    = secs_or("UPSTREAM_TIMEOUT_SECS", 30)?;
        let upstream_retry_max  = parse_or("UPSTREAM_RETRY_MAX",   3_u32)?;
        let upstream_retry_base = ms_or("UPSTREAM_RETRY_BASE_MS",  300)?;

        let upstream_proxy = std::env::var("UPSTREAM_PROXY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let log_format_json = std::env::var("LOG_FORMAT")
            .map(|s| s.eq_ignore_ascii_case("json"))
            .unwrap_or(false);

        let whatsapp_service_url = std::env::var("WHATSAPP_SERVICE_URL").unwrap_or_default();
        let whatsapp_zone_group_id = std::env::var("WHATSAPP_ZONE_GROUP_ID").unwrap_or_default();
        let zone_scan_time = std::env::var("ZONE_SCAN_TIME").unwrap_or_else(|_| "03:30".into());
        // Validate HH:MM format
        if chrono::NaiveTime::parse_from_str(&zone_scan_time, "%H:%M").is_err() {
            return Err(anyhow!("ZONE_SCAN_TIME must be HH:MM"));
        }

        let zone_lookback_days = parse_or("ZONE_LOOKBACK_DAYS", 2)?;
        let zone_scan_on_startup = parse_or("ZONE_SCAN_ON_STARTUP", false)?;
        let zone_min_dwell_secs = parse_or("ZONE_MIN_DWELL_SECS", 120)?;
        let zone_visit_gap_secs = parse_or("ZONE_VISIT_GAP_SECS", 180)?;

        let apex_database_url = std::env::var("APEX_DATABASE_URL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let osrm_url = std::env::var("OSRM_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:5000".into())
            .trim_end_matches('/')
            .to_string();
        let osrm_exclude_class =
            std::env::var("OSRM_EXCLUDE_CLASS").unwrap_or_default().trim().to_string();

        let trip_scan_time = std::env::var("TRIP_SCAN_TIME").unwrap_or_else(|_| "04:45".into());
        if chrono::NaiveTime::parse_from_str(&trip_scan_time, "%H:%M").is_err() {
            return Err(anyhow!("TRIP_SCAN_TIME must be HH:MM"));
        }
        let trip_scan_lookback_days = parse_or("TRIP_SCAN_LOOKBACK_DAYS", 1)?;
        let trip_scan_on_startup = parse_or("TRIP_SCAN_ON_STARTUP", false)?;

        let tanker_night_start = hhmm_or("TANKER_NIGHT_START", "20:00")?;
        let tanker_night_end = hhmm_or("TANKER_NIGHT_END", "07:00")?;

        let terminal_radius_m = parse_or("TERMINAL_RADIUS_M", 500.0_f64)?;
        let dropoff_radius_m = parse_or("DROPOFF_RADIUS_M", 300.0_f64)?;

        let trip_flag_distance_ratio = parse_or("TRIP_FLAG_DISTANCE_RATIO", 1.2_f64)?;
        // Trucks run well below OSRM's modeled speeds; 2.5× filters to real
        // outliers instead of flagging every trip.
        let trip_flag_duration_ratio = parse_or("TRIP_FLAG_DURATION_RATIO", 2.5_f64)?;
        let trip_pending_closer_secs = parse_or("TRIP_PENDING_CLOSER_SECS", 3600_u64)?;
        let trip_unplanned_stop_secs = parse_or("TRIP_UNPLANNED_STOP_SECS", 900)?;
        let trip_deviation_m = parse_or("TRIP_DEVIATION_M", 2000.0_f64)?;
        let trip_pin_auto_apply_min_stops = parse_or("TRIP_PIN_AUTO_APPLY_MIN_STOPS", 5_i64)?;
        let trip_return_cap_secs = parse_or("TRIP_RETURN_CAP_HOURS", 72_i64)?.max(1) * 3600;
        let trip_match_min_coverage = parse_or("TRIP_MATCH_MIN_COVERAGE", 0.85_f64)?;
        let trip_off_route_flag_pct = parse_or("TRIP_OFF_ROUTE_FLAG_PCT", 0.25_f64)?;
        // "lat,lng[,radius_m];lat,lng[,radius_m]" — radius defaults to 500 m.
        let trip_allowed_points = std::env::var("TRIP_ALLOWED_POINTS")
            .unwrap_or_default()
            .split(';')
            .filter_map(|entry| {
                let parts: Vec<&str> = entry.split(',').map(str::trim).collect();
                if parts.len() < 2 {
                    return None;
                }
                let lat = parts[0].parse::<f64>().ok()?;
                let lng = parts[1].parse::<f64>().ok()?;
                let radius = parts.get(2).and_then(|r| r.parse::<f64>().ok()).unwrap_or(500.0);
                Some((lat, lng, radius))
            })
            .collect::<Vec<_>>();
        let trip_allowed_point_dwell_secs =
            parse_or("TRIP_ALLOWED_POINT_DWELL_SECS", 20 * 60_i64)?;

        Ok(Self {
            etit_data_url,
            etit_login_url,
            etit_username,
            etit_password,
            etit_sub_id,
            etit_staff_id,
            bind_addr,
            admin_token,
            cors_origins,
            database_url,
            timezone,
            tier_a_refresh,
            tier_b_poll,
            tier_c_poll,
            tier_c_concurrency,
            upstream_timeout,
            upstream_retry_max,
            upstream_retry_base,
            upstream_proxy,
            log_format_json,
            whatsapp_service_url,
            whatsapp_zone_group_id,
            zone_scan_time,
            zone_lookback_days,
            zone_scan_on_startup,
            zone_min_dwell_secs,
            zone_visit_gap_secs,
            apex_database_url,
            osrm_url,
            osrm_exclude_class,
            trip_scan_time,
            trip_scan_lookback_days,
            trip_scan_on_startup,
            tanker_night_start,
            tanker_night_end,
            terminal_radius_m,
            dropoff_radius_m,
            trip_flag_distance_ratio,
            trip_flag_duration_ratio,
            trip_pending_closer_secs,
            trip_unplanned_stop_secs,
            trip_deviation_m,
            trip_pin_auto_apply_min_stops,
            trip_return_cap_secs,
            trip_match_min_coverage,
            trip_off_route_flag_pct,
            trip_allowed_points,
            trip_allowed_point_dwell_secs,
        })
    }

    pub fn admin_enabled(&self) -> bool {
        self.admin_token.is_some()
    }
}

// ---- helpers ---------------------------------------------------------------

fn required_env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{key} must be set"))
}

fn trimmed_env(key: &str) -> Result<String> {
    Ok(required_env(key)?.trim_end_matches('/').to_string())
}

fn optional_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn secs_or(key: &str, default: u64) -> Result<Duration> {
    parse_or::<u64>(key, default).map(Duration::from_secs)
}

fn ms_or(key: &str, default: u64) -> Result<Duration> {
    parse_or::<u64>(key, default).map(Duration::from_millis)
}

fn hhmm_or(key: &str, default: &str) -> Result<chrono::NaiveTime> {
    let raw = std::env::var(key).unwrap_or_else(|_| default.to_string());
    chrono::NaiveTime::parse_from_str(&raw, "%H:%M")
        .map_err(|_| anyhow!("{key} must be HH:MM, got `{raw}`"))
}

fn parse_or<T: FromStr>(key: &str, default: T) -> Result<T>
where
    <T as FromStr>::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(v) => v
            .parse::<T>()
            .map_err(|e| anyhow!("{key} invalid: {e}")),
        Err(_) => Ok(default),
    }
}
