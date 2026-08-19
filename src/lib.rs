//! etit-proxy — middleware bridging React/Google Maps to legacy ETIT
//! ASP.NET tracking.
//!
//! Module layout (top-down):
//!
//! - [`config`]    — env-driven configuration
//! - [`error`]     — uniform error type and HTTP mapping
//! - [`domain`]    — pure data types the rest of the system passes around
//! - [`upstream`]  — auth, scraping, and parsing for the legacy ETIT system
//! - [`cache`]     — moka-backed in-memory tiers + single-flight
//! - [`db`]        — sqlx persistence (vehicles + history)
//! - [`services`]  — orchestration: cache → DB → upstream lookups
//! - [`pollers`]   — background workers that warm the caches
//! - [`api`]       — axum router, handlers, middleware, SSE
//! - [`state`]     — shared `AppState` glued together at startup

pub mod api;
pub mod cache;
pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod pollers;
pub mod services;
pub mod state;
pub mod upstream;
pub mod auth;
pub mod zones;
pub mod trips;
