//! SQLite persistence via sqlx.
//!
//! Schema lives in `migrations/`. We run them at startup with
//! `sqlx::migrate!`, so the binary owns its own DB lifecycle — no separate
//! migration step required for ops.

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub mod history;
pub mod trip_audit;
pub mod vehicles;
pub mod zones;

pub use history::HistoryRepo;
pub use trip_audit::TripAuditRepo;
pub use vehicles::VehiclesRepo;
pub use zones::ZonesRepo;

pub async fn open_pool(database_url: &str) -> Result<SqlitePool> {
    // For sqlite:// URLs, ensure the parent directory exists so SQLite
    // doesn't error out trying to create the file.
    if let Some(path) = database_url.strip_prefix("sqlite://") {
        let path = path.split('?').next().unwrap_or(path);
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating db dir {parent:?}"))?;
            }
        }
    }

    let opts = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("parsing DATABASE_URL `{database_url}`"))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(opts)
        .await
        .context("opening SQLite pool")?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("running migrations")?;

    tracing::info!("SQLite pool ready, migrations applied");
    Ok(pool)
}
