//! etit-proxy binary entry point.
//!
//! Cold-start sequence:
//!   1. Initialize tracing (json or pretty per `LOG_FORMAT`).
//!   2. Load config from environment.
//!   3. Open the SQLite pool and run migrations.
//!   4. Build the upstream client.
//!   5. Build the cache, services, broadcaster.
//!   6. Seed the in-memory vehicle cache from SQLite (no upstream call yet).
//!   7. Attempt the first ETIT login. *Failure here is non-fatal* — the
//!      pollers will retry. We must still bind the port so K8s/systemd
//!      readiness probes succeed.
//!   8. Spawn pollers.
//!   9. Build the router with CORS / compression / tracing / request-id.
//!  10. Serve until SIGINT/SIGTERM.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use axum::http::{HeaderName, HeaderValue, Method};
use axum::routing::get;
use tokio::net::TcpListener;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer, ExposeHeaders};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use etit_proxy::api;
use etit_proxy::cache::CacheLayer;
use etit_proxy::config::Config;
use etit_proxy::db::{open_pool, HistoryRepo, TripAuditRepo, VehiclesRepo, ZonesRepo};
use etit_proxy::pollers::Pollers;
use etit_proxy::zones::scan::spawn_zone_scanner;
use etit_proxy::services::broadcaster::Broadcaster;
use etit_proxy::services::history::HistoryService;
use etit_proxy::services::live::LiveService;
use etit_proxy::services::vehicles::VehiclesService;
use etit_proxy::state::AppState;
use etit_proxy::upstream::EtitClient;

/// Default capacity for the live broadcast channel.
const BROADCAST_CAPACITY: usize = 64;

/// Default capacity for the moka history cache.
const HISTORY_CACHE_DEFAULT_CAPACITY: u64 = 4_096;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::from_env().context("loading configuration")?;
    install_tracing(cfg.log_format_json);

    // Sentry, if SENTRY_DSN is set. The guard must outlive the server; with no
    // DSN this is None and the service runs exactly as it did before.
    let _sentry = etit_proxy::observability::init();

    tracing::info!(
        bind      = %cfg.bind_addr,
        data_url  = %cfg.etit_data_url,
        login_url = %cfg.etit_login_url,
        tz        = %cfg.timezone,
        version   = env!("CARGO_PKG_VERSION"),
        "starting etit-proxy",
    );

    // ---- Persistence --------------------------------------------------------
    let pool = open_pool(&cfg.database_url).await?;
    let vehicles_repo = VehiclesRepo::new(pool.clone());
    let zones_repo = ZonesRepo::new(pool.clone());
    let trip_repo = TripAuditRepo::new(pool.clone());

    // ---- Apex Postgres (trip audit) — optional, read-only -------------------
    let apex_db = match &cfg.apex_database_url {
        Some(url) => match etit_proxy::trips::pg::ApexDb::connect(url).await {
            Ok(db) => {
                tracing::info!("apex Postgres connected; trip audit enabled");
                Some(db)
            }
            Err(e) => {
                tracing::error!(error = %e, "apex Postgres connection failed; trip audit disabled");
                None
            }
        },
        None => None,
    };

    // ---- Upstream client ---------------------------------------------------
    let client = EtitClient::new(cfg.clone()).context("building EtitClient")?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("building generic http client")?;

    // ---- In-memory cache ---------------------------------------------------
    let history_capacity = std::env::var("HISTORY_CACHE_CAPACITY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(HISTORY_CACHE_DEFAULT_CAPACITY);
    let cache = CacheLayer::new(cfg.tier_a_refresh, cfg.timezone, history_capacity);

    // ---- Services ----------------------------------------------------------
    let vehicles_svc = Arc::new(VehiclesService {
        client: client.clone(),
        cache:  cache.clone(),
        repo:   VehiclesRepo::new(pool.clone()),
    });

    let live_svc = Arc::new(LiveService {
        client:   client.clone(),
        cache:    cache.clone(),
        vehicles: vehicles_svc.clone(),
    });

    let history_svc = Arc::new(HistoryService::new(
        client.clone(),
        cache.clone(),
        HistoryRepo::new(pool.clone()),
    ));

    let broadcaster = Broadcaster::new(BROADCAST_CAPACITY);

    // ---- Seed in-memory vehicle cache from disk ----------------------------
    match vehicles_repo.load_all().await {
        Ok(vs) if !vs.is_empty() => {
            tracing::info!(count = vs.len(), "seeded vehicle cache from SQLite");
            cache.store_vehicles(vs);
        }
        Ok(_) => {
            tracing::info!("no persisted vehicles; will populate on first Tier A refresh");
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not seed vehicles from disk");
        }
    }

    // ---- Initial login (best-effort) --------------------------------------
    if let Err(e) = client.force_login().await {
        tracing::warn!(error = %e, "initial ETIT login failed; pollers will retry");
    }

    // ---- App state --------------------------------------------------------
    let state = AppState {
        cfg:         cfg.clone(),
        client:      client.clone(),
        http:        http.clone(),
        cache:       cache.clone(),
        vehicles:    vehicles_svc.clone(),
        live:        live_svc.clone(),
        history:     history_svc.clone(),
        zones_repo:  zones_repo.clone(),
        trip_repo:   trip_repo.clone(),
        apex_db:     apex_db.clone(),
        osrm:        etit_proxy::trips::osrm::OsrmClient::new(
            http.clone(),
            cfg.osrm_url.clone(),
            cfg.osrm_exclude_class.clone(),
        ),
        broadcaster: broadcaster.clone(),
        started_at:  Instant::now(),
        pending_rescan: std::sync::Arc::new(parking_lot::Mutex::new((
            std::collections::BTreeSet::new(),
            false,
        ))),
        scan_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
    };

    // ---- Pollers ----------------------------------------------------------
    Arc::new(Pollers {
        cfg:          cfg.clone(),
        vehicles:     vehicles_svc.clone(),
        live:         live_svc.clone(),
        history:      history_svc.clone(),
        history_repo: HistoryRepo::new(pool.clone()),
        broadcaster:  broadcaster.clone(),
    })
    .spawn_all();

    // ---- Zones Scanner ----------------------------------------------------
    spawn_zone_scanner(state.clone());

    // ---- Trip Audit Scanner -------------------------------------------------
    etit_proxy::trips::scan::spawn_trip_scanner(state.clone());
    etit_proxy::trips::scan::spawn_pending_return_closer(state.clone());

    // ---- History late-data healer ------------------------------------------
    etit_proxy::services::history::spawn_history_healer(state.clone());

    // ---- CORS -------------------------------------------------------------
    //
    // `withCredentials: true` on the browser side requires three things from
    // the server — all three must hold simultaneously or the browser blocks:
    //
    //   1. Access-Control-Allow-Origin must be a single explicit origin,
    //      never "*". The browser refuses credentialed requests to a wildcard.
    //
    //   2. Access-Control-Allow-Credentials: true must be present on both
    //      the preflight (OPTIONS) and the actual response.
    //
    //   3. Every header the browser sends in the real request must appear in
    //      Access-Control-Allow-Headers on the preflight — including
    //      Authorization, which is *not* a CORS-safelisted header.
    //
    // CORS_ALLOWED_ORIGIN defaults to the production frontend. Override in
    // .env for local dev, e.g.:
    //   CORS_ALLOWED_ORIGIN=http://localhost:5173
    let allowed_origin = std::env::var("CORS_ALLOWED_ORIGIN")
        .unwrap_or_else(|_| "https://apextransport.ddns.net".to_string());

    let origin_value = HeaderValue::from_str(&allowed_origin)
        .with_context(|| format!("invalid CORS_ALLOWED_ORIGIN: {allowed_origin}"))?;

    tracing::info!(origin = %allowed_origin, "CORS allowed origin");

    let cors = CorsLayer::new()
        // Exact origin — required when allow_credentials is true.
        .allow_origin(AllowOrigin::exact(origin_value))
        // Allow-Credentials: true so the browser forwards cookies / auth
        // headers and exposes the response to JS.
        .allow_credentials(true)
        .allow_methods(AllowMethods::list([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
            Method::OPTIONS,
        ]))
        // List every request header the frontend will send. Authorization is
        // non-safelisted and *must* be here or the preflight is rejected.
        .allow_headers(AllowHeaders::list([
            HeaderName::from_static("authorization"),
            HeaderName::from_static("content-type"),
            HeaderName::from_static("accept"),
            HeaderName::from_static("x-request-id"),
            // The caller's trace id. Neither is CORS-safelisted, so without
            // them the browser strips them at preflight and every request
            // looks like the start of a new trace.
            HeaderName::from_static("sentry-trace"),
            HeaderName::from_static("baggage"),
        ]))
        // Expose response headers that frontend JS needs to read.
        .expose_headers(ExposeHeaders::list([
            HeaderName::from_static("x-request-id"),
        ]))
        // Cache the preflight for 2 hours — reduces OPTIONS round-trips.
        .max_age(std::time::Duration::from_secs(7_200));

    // ---- Server -----------------------------------------------------------
    //
    // Route structure:
    //
    //   /health  — unauthenticated, for systemd / nginx / external monitors.
    //
    //   /*       — everything else is behind `require_admin`. The middleware
    //              validates the JWT (cookie or Bearer header), checks the
    //              user_type == "admin_user" claim, and returns 401/403 before
    //              any handler logic runs.
    //
    // `route_layer` applies the middleware only to the routes it wraps, so
    // /health is genuinely excluded — not just skipped inside the middleware.
    //
    // CORS is the outermost layer so preflight OPTIONS requests are answered
    // before auth runs. Browsers send preflights without credentials, so
    // letting auth see them would cause every preflight to 401.
    // JWT gating now lives inside api::router (v1 routes only) so the
    // token-guarded /admin/* surface and operational endpoints stand alone.
    let app = axum::Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(api::router(state))
        .layer(cors)
        // One hub per request, so a captured error carries only its own
        // request's context and never bleeds scope between concurrent
        // requests. Everything it attaches is scrubbed in `observability`
        // before it leaves the process.
        .layer(sentry_tower::NewSentryLayer::new_from_top())
        // Transactions, so upstream latency and throughput are visible and not
        // only outright failures. NewSentryLayer alone binds a hub and captures
        // nothing; the capture itself lives in AppError::into_response.
        .layer(sentry_tower::SentryHttpLayer::new().enable_transaction());

    let listener = TcpListener::bind(&cfg.bind_addr)
        .await
        .with_context(|| format!("binding {}", cfg.bind_addr))?;

    tracing::info!(addr = %cfg.bind_addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    tracing::info!("shutdown complete");
    Ok(())
}

fn install_tracing(json: bool) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,etit_proxy=debug,tower_http=info"));

    // The Sentry layer turns tracing into diagnostics: sqlx emits a span per
    // query with its elapsed time, so when a 500 fires the event carries the
    // last queries this request ran and how long each took. That answers most
    // of "why was this slow" and "what did it just run" without a profiler.
    //
    // It carries its own filter rather than sharing the console's. The console
    // stays at whatever RUST_LOG says; Sentry needs sqlx at debug to see the
    // queries at all, and putting that on the console would drown it.
    let sentry_layer = sentry_tracing::layer().with_filter(
        EnvFilter::try_from_env("SENTRY_TRACING_FILTER")
            .unwrap_or_else(|_| EnvFilter::new("info,sqlx=debug,etit_proxy=debug")),
    );

    let registry = tracing_subscriber::registry().with(sentry_layer).with(filter);
    if json {
        registry.with(fmt::layer().json().with_target(false)).init();
    } else {
        registry.with(fmt::layer().with_target(false)).init();
    }
}

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c    => tracing::info!("ctrl-c received; shutting down"),
        _ = terminate => tracing::info!("SIGTERM received; shutting down"),
    }

    // Graceful shutdown waits for open connections, and SSE streams never
    // close on their own — without a deadline, `systemctl stop` hangs for
    // TimeoutStopSec and ends in SIGKILL (observed 2026-08-26). Give the
    // server a grace window, then leave on our own terms.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        tracing::warn!("graceful shutdown exceeded 10s (long-lived SSE clients); exiting now");
        std::process::exit(0);
    });
}