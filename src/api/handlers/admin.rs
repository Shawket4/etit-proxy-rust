//! `/admin/*` — token-gated operational handlers. The auth check is done
//! by the [`crate::api::middleware::admin_guard`] middleware; handlers here
//! assume the request is authorized.

use axum::extract::State;
use axum::Json;

use crate::api::dto::{InvalidateRequest, InvalidateResponse, ReloginResponse, UnblockResponse};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::upstream::users;

/// `POST /admin/cache/invalidate { "scope": "all" | "vehicles" | "live" | "history" }`
pub async fn invalidate(
    State(s): State<AppState>,
    Json(req): Json<InvalidateRequest>,
) -> AppResult<Json<InvalidateResponse>> {
    match req.scope.as_str() {
        "all" => {
            s.cache.invalidate_all().await;
            // Also wipe persisted history. Vehicles will be re-fetched on
            // the next Tier A tick anyway.
            if let Err(e) = s.history.repo.truncate().await {
                tracing::warn!(error = %e, "history truncate failed");
            }
            Ok(Json(InvalidateResponse {
                scope: "all".into(),
                ok: true,
                message: "all caches and persisted history cleared".into(),
            }))
        }
        "vehicles" => {
            // Force a refresh; the snapshot will be replaced atomically.
            match s.vehicles.refresh_now().await {
                Ok(list) => Ok(Json(InvalidateResponse {
                    scope: "vehicles".into(),
                    ok: true,
                    message: format!("vehicles refreshed; {} vehicles", list.len()),
                })),
                Err(e) => Err(e),
            }
        }
        "live" => {
            match s.live.refresh_now().await {
                Ok(snap) => Ok(Json(InvalidateResponse {
                    scope: "live".into(),
                    ok: true,
                    message: format!("live refreshed; {} vehicles", snap.by_id.len()),
                })),
                Err(e) => Err(e),
            }
        }
        "history" => {
            s.cache.invalidate_all().await;
            if let Err(e) = s.history.repo.truncate().await {
                tracing::warn!(error = %e, "history truncate failed");
            }
            Ok(Json(InvalidateResponse {
                scope: "history".into(),
                ok: true,
                message: "history caches and persisted rows cleared".into(),
            }))
        }
        other => Err(AppError::BadRequest(format!(
            "unknown scope `{other}` (allowed: all, vehicles, live, history)"
        ))),
    }
}

/// `POST /admin/auth/relogin` — force a session re-login.
pub async fn relogin(State(s): State<AppState>) -> AppResult<Json<ReloginResponse>> {
    let epoch = s.client.force_login().await?;
    Ok(Json(ReloginResponse { ok: true, epoch }))
}

/// `POST /admin/users/unblock` — unblock the hardcoded first-row user
/// (`magda.adly`) via the legacy `UserList.aspx` grid postback. Idempotent:
/// unblocking an already-unblocked user is a no-op upstream.
///
/// The response carries best-effort debug detail. `ok: false` with
/// `accessDenied: true` means the acting account lacks rights on UserList.
/// `ok: true` means the server accepted the event with no rejection signals —
/// but the grid markup exposes no blocked flag, so it is not a hard proof the
/// user is now unblocked (see `note`).
pub async fn unblock_user(State(s): State<AppState>) -> AppResult<Json<UnblockResponse>> {
    let outcome = users::unblock_first_row(&s.client).await?;
    Ok(Json(UnblockResponse {
        ok: outcome.accepted(),
        target: users::UNBLOCK_TARGET_LABEL.into(),
        note: outcome.note(),
        http_status: outcome.http_status,
        is_users_grid: outcome.is_users_grid,
        error_page: outcome.error_page,
        access_denied: outcome.access_denied,
        upstream_message: outcome.upstream_message,
        body_excerpt: outcome.body_excerpt,
    }))
}

/// `POST /admin/routing/route`
///
/// The one place any service asks "how far is A from B by road".
///
/// FalconGo previously built its own OSRM URL for fee-mapping enrichment. That
/// call had no `exclude`, so it routed tankers down time-gated roads this
/// service deliberately avoids, and the same pair of points got two different
/// distances depending on which codebase asked. Routing through here means the
/// exclude class, the retry-without-exclude fallback and the base URL all have
/// exactly one definition.
pub async fn route(
    State(s): State<AppState>,
    Json(req): Json<crate::api::dto::RouteRequest>,
) -> AppResult<Json<crate::api::dto::RouteResponse>> {
    let via: Vec<(f64, f64)> = req.via.iter().map(|p| (p[0], p[1])).collect();

    let routed = s
        .osrm
        .route(
            (req.from_lat, req.from_lng),
            (req.to_lat, req.to_lng),
            &via,
            req.restricted,
        )
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(crate::api::dto::RouteResponse {
        distance_km: routed.distance_km,
        duration_min: routed.duration_secs as f64 / 60.0,
        geometry: routed.geometry,
        restricted: req.restricted,
    }))
}
