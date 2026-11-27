//! `/service/*` — read-only lookups for other Apex services, gated by
//! [`crate::api::middleware::service_guard`]. Handlers here assume the
//! request is authorized and must never write anything.

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::trips::driven::{summarise, DrivenSummary};

#[derive(Deserialize)]
pub struct DrivenKmQuery {
    pub terminal: Option<String>,
    pub drop_off: Option<String>,
}

#[derive(Serialize)]
pub struct DrivenKmResponse {
    pub terminal: String,
    pub drop_off: String,
    #[serde(flatten)]
    pub summary: DrivenSummary,
}

/// `GET /service/trip-audit/driven-km?terminal=&drop_off=`
///
/// How far our trucks actually drove one terminal → drop-off route, from
/// the audit's stored outbound legs (see [`crate::trips::driven`]). The
/// route is physical, so trips of every company count.
pub async fn driven_km(
    State(s): State<AppState>,
    Query(q): Query<DrivenKmQuery>,
) -> AppResult<Json<DrivenKmResponse>> {
    let terminal = required(q.terminal, "terminal")?;
    let drop_off = required(q.drop_off, "drop_off")?;

    let legs = s
        .trip_repo
        .outbound_legs_for_route(&terminal, &drop_off)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(DrivenKmResponse {
        terminal,
        drop_off,
        summary: summarise(&legs),
    }))
}

fn required(v: Option<String>, name: &str) -> AppResult<String> {
    match v.map(|s| s.trim().to_string()) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(AppError::BadRequest(format!("{name} is required"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_parameters_are_rejected() {
        assert!(required(None, "terminal").is_err());
        assert!(required(Some("  ".into()), "terminal").is_err());
        assert_eq!(
            required(Some(" Tanta ".into()), "drop_off").unwrap(),
            "Tanta"
        );
    }

    #[test]
    fn response_is_flat_and_keeps_nulls() {
        let body = DrivenKmResponse {
            terminal: "Alexandria".into(),
            drop_off: "Tanta".into(),
            summary: summarise(&[]),
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["terminal"], "Alexandria");
        assert_eq!(v["trips"], 0);
        // Every field present, null where it does not apply.
        for key in ["median_km", "p10_km", "p90_km", "first_day", "latest_day"] {
            assert!(
                v.get(key).is_some_and(|x| x.is_null()),
                "{key} should be present and null"
            );
        }
    }
}
