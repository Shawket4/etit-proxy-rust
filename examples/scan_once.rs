//! One-shot trip-audit scan runner for integration testing.
//!
//! Builds the same state as main.rs (minus the HTTP server and pollers),
//! runs a single trip-audit scan for the given dates, prints the summary,
//! and exits. Reads the normal environment/.env configuration.
//!
//! Usage: cargo run --example scan_once -- 2026-08-18 [more dates...]

use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use chrono::NaiveDate;

use etit_proxy::cache::CacheLayer;
use etit_proxy::config::Config;
use etit_proxy::db::{open_pool, HistoryRepo, TripAuditRepo, VehiclesRepo, ZonesRepo};
use etit_proxy::services::broadcaster::Broadcaster;
use etit_proxy::services::history::HistoryService;
use etit_proxy::services::live::LiveService;
use etit_proxy::services::vehicles::VehiclesService;
use etit_proxy::state::AppState;
use etit_proxy::upstream::EtitClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info,etit_proxy=debug").init();

    let dates: Vec<NaiveDate> = std::env::args()
        .skip(1)
        .map(|a| NaiveDate::parse_from_str(&a, "%Y-%m-%d").context("date must be YYYY-MM-DD"))
        .collect::<Result<_, _>>()?;

    let cfg = Config::from_env()?;
    let pool = open_pool(&cfg.database_url).await?;

    let client = EtitClient::new(cfg.clone())?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let cache = CacheLayer::new(cfg.tier_a_refresh, cfg.timezone, 4096);

    let vehicles_svc = Arc::new(VehiclesService {
        client: client.clone(),
        cache: cache.clone(),
        repo: VehiclesRepo::new(pool.clone()),
    });
    let live_svc = Arc::new(LiveService {
        client: client.clone(),
        cache: cache.clone(),
        vehicles: vehicles_svc.clone(),
    });
    let history_svc = Arc::new(HistoryService::new(
        client.clone(),
        cache.clone(),
        HistoryRepo::new(pool.clone()),
    ));

    let apex_db = match &cfg.apex_database_url {
        Some(url) => Some(etit_proxy::trips::pg::ApexDb::connect(url).await?),
        None => anyhow::bail!("APEX_DATABASE_URL must be set for scan_once"),
    };

    // Login is best-effort: cached history days don't need upstream.
    if let Err(e) = client.force_login().await {
        tracing::warn!(error = %e, "ETIT login failed; will rely on cached history only");
    }

    let state = AppState {
        cfg: cfg.clone(),
        client,
        http: http.clone(),
        cache,
        vehicles: vehicles_svc,
        live: live_svc,
        history: history_svc,
        zones_repo: ZonesRepo::new(pool.clone()),
        trip_repo: TripAuditRepo::new(pool.clone()),
        apex_db,
        osrm: etit_proxy::trips::osrm::OsrmClient::new(
            http,
            cfg.osrm_url.clone(),
            cfg.osrm_exclude_class.clone(),
        ),
        broadcaster: Broadcaster::new(8),
        started_at: Instant::now(),
    };

    let summary =
        etit_proxy::trips::scan::run_scan(state, if dates.is_empty() { None } else { Some(dates) })
            .await;

    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
