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
