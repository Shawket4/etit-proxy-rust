use std::time::Duration;
use chrono::{NaiveDate, Utc, TimeZone};
use crate::state::AppState;
use crate::zones::{ScanSummary, ScanViolation, visits};

/// How far back a manual scan may reach in one call. The window is read a
/// vehicle-day at a time out of `history_days`, so an unbounded range is a
/// request that holds the pool for minutes; operators wanting a year take it a
/// quarter at a time.
pub const MAX_MANUAL_RANGE_DAYS: i64 = 92;

/// Which days a round covers, and — the part that matters — whether it is
/// allowed to talk to WhatsApp.
///
/// `Scheduled` is the nightly round: it walks the configured lookback, records
/// each visit, and alerts on the ones not alerted on yet. `Manual` is an
/// operator asking a question about a past range: it reports what it found and
/// touches neither `zone_visits` nor the WhatsApp group, so re-running it over
/// the same weeks is free of consequence and never re-notifies anyone.
#[derive(Debug, Clone)]
pub enum ScanWindow {
    Scheduled,
    Manual { from: NaiveDate, to: NaiveDate },
}

impl ScanWindow {
    fn mode(&self) -> &'static str {
        match self {
            ScanWindow::Scheduled => "scheduled",
            ScanWindow::Manual { .. } => "manual",
        }
    }
}

pub async fn run_scan(state: AppState) -> ScanSummary {
    run_scan_window(state, ScanWindow::Scheduled).await
}

pub async fn run_scan_window(state: AppState, window: ScanWindow) -> ScanSummary {
    let mut summary = ScanSummary {
        run_id: 0,
        status: "running".into(),
        mode: window.mode().into(),
        from: String::new(),
        to: String::new(),
        days_scanned: 0,
        vehicles_scanned: 0,
        violations_found: 0,
        alerts_sent: 0,
        violations: Vec::new(),
        error: None,
    };

    // 1. Insert scan_runs 'running'
    let run_id = match state.zones_repo.insert_scan_run().await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "Failed to insert scan_run");
            summary.status = "error".into();
            summary.error = Some(e.to_string());
            return summary;
        }
    };
    summary.run_id = run_id;

    if let Err(e) = do_run_scan(&state, &window, &mut summary).await {
        tracing::error!(error = %e, "Scan run failed");
        summary.status = "error".into();
        summary.error = Some(e.to_string());
    } else {
        summary.status = "success".into();
    }

    if let Err(e) = state.zones_repo.finalize_scan_run(run_id, &summary).await {
        tracing::error!(error = %e, "Failed to finalize scan_run");
    }

    summary
}

async fn do_run_scan(
    state: &AppState,
    window: &ScanWindow,
    summary: &mut ScanSummary,
) -> anyhow::Result<()> {
    // Failed alerts are warned on individually, and a warn is a breadcrumb, so
    // on its own it raises nothing. Counted here so the scan can say once that
    // it delivered none of them, which is the alerting path being down.
    let mut alerts_failed: u32 = 0;
    // 2. Load active zones
    let zones = state.zones_repo.list(true).await?;

    // 3. Compute window day_local list
    let days = match window {
        ScanWindow::Scheduled => {
            let today = crate::domain::time::today_local(state.cfg.timezone);
            let mut days = Vec::new();
            for i in 0..state.cfg.zone_lookback_days {
                if let Some(d) = today.checked_sub_days(chrono::Days::new(i as u64)) {
                    days.push(d);
                }
            }
            days
        }
        ScanWindow::Manual { from, to } => {
            let mut days = Vec::new();
            let mut d = *from;
            while d <= *to {
                days.push(d);
                match d.succ_opt() {
                    Some(next) => d = next,
                    None => break,
                }
            }
            days
        }
    };

    // Echoed back even when there is nothing to scan, so a caller can see the
    // range its request actually resolved to. `days` runs newest-first for the
    // scheduled round and oldest-first for a manual range, so take the extremes
    // by date rather than by position.
    if let (Some(lo), Some(hi)) = (days.iter().min(), days.iter().max()) {
        summary.from = lo.format("%Y-%m-%d").to_string();
        summary.to = hi.format("%Y-%m-%d").to_string();
    }

    if zones.is_empty() {
        return Ok(());
    }
    summary.days_scanned = days.len() as i64;

    let vids = state.zones_repo.get_window_vehicles(&days).await?;
    summary.vehicles_scanned = vids.len() as i64;

    let report_only = matches!(window, ScanWindow::Manual { .. });

    // 4. Detect visits
    for vid in &vids {
        let points = state.zones_repo.get_window_points(vid, &days).await?;
        for zone in &zones {
            let visits = visits::detect_visits(&points, zone, state.cfg.zone_visit_gap_secs);
            for v in visits {
                if (v.exit - v.entry) >= state.cfg.zone_min_dwell_secs {
                    summary.violations_found += 1;
                    if report_only {
                        summary.violations.push(ScanViolation {
                            vehicle_id: vid.clone(),
                            codename: codename_for(state, vid).await,
                            zone_id: zone.id,
                            zone_name: zone.name.clone(),
                            zone_lat: zone.lat,
                            zone_lng: zone.lng,
                            entry_local: format_local(state, v.entry),
                            exit_local: format_local(state, v.exit),
                            dwell_secs: v.exit - v.entry,
                            point_count: v.points,
                        });
                    } else {
                        state.zones_repo.upsert_visit(vid, zone.id, v.entry, v.exit, v.points).await?;
                    }
                }
            }
        }
    }

    if report_only {
        // Newest first: an operator reading a range report wants the most
        // recent violation at the top.
        summary.violations.sort_by(|a, b| b.entry_local.cmp(&a.entry_local));
        return Ok(());
    }

    // 5. Alert
    let unalerted = state.zones_repo.get_unalerted_visits().await?;
    for u in unalerted {
        let codename = codename_for(state, &u.vehicle_id).await;

        let entry_utc = chrono::DateTime::parse_from_rfc3339(&u.entry_ts)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| crate::domain::time::utc_now());
        let exit_utc = chrono::DateTime::parse_from_rfc3339(&u.exit_ts)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| crate::domain::time::utc_now());

        let entry_local = entry_utc.with_timezone(&state.cfg.timezone).format("%Y-%m-%d %H:%M:%S").to_string();
        let exit_local = exit_utc.with_timezone(&state.cfg.timezone).format("%Y-%m-%d %H:%M:%S").to_string();

        let res = crate::zones::notify::send_whatsapp_alert(
            state, &codename, &u.name, &entry_local, &exit_local,
            u.dwell_secs, u.lat, u.lng
        ).await;

        match res {
            Ok(_) => {
                state.zones_repo.mark_alerted(u.id).await?;
                summary.alerts_sent += 1;
            }
            Err(e) => {
                // Counted, not reported one by one: if WhatsApp is down every
                // alert in the scan fails, and N events would say the same
                // thing N times. The round reports itself below.
                alerts_failed += 1;
                tracing::warn!(error = %e, visit_id = u.id, "Failed to send whatsapp alert");
            }
        }
    }

    if alerts_failed > 0 && summary.alerts_sent == 0 {
        crate::observability::capture_batch_failure(
            "zones.whatsapp_alert", alerts_failed as usize, alerts_failed as usize,
        );
    }

    Ok(())
}

/// The vehicle's codename, falling back to its raw id when the vehicle is
/// unknown — a report row is still useful with an id in it.
async fn codename_for(state: &AppState, vehicle_id: &str) -> String {
    match state.vehicles.get_by_id(vehicle_id).await {
        Ok(Some(v)) => v.codename,
        _ => vehicle_id.to_string(),
    }
}

fn format_local(state: &AppState, unix_secs: i64) -> String {
    Utc.timestamp_opt(unix_secs, 0)
        .single()
        .map(|t| {
            t.with_timezone(&state.cfg.timezone)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

pub fn spawn_zone_scanner(state: AppState) {
    crate::observability::spawn_isolated(async move {
        if state.cfg.zone_scan_on_startup {
            let _ = run_scan(state.clone()).await;
        }
        loop {
            let sleep = duration_until_next_local(&state.cfg.zone_scan_time, state.cfg.timezone);
            tokio::time::sleep(sleep).await;
            let _ = run_scan(state.clone()).await;
        }
    });
}

fn duration_until_next_local(time_str: &str, tz: chrono_tz::Tz) -> Duration {
    let now = Utc::now().with_timezone(&tz);
    let target_time = chrono::NaiveTime::parse_from_str(time_str, "%H:%M").unwrap();
    
    // chrono-tz doesn't provide a direct way to construct DateTime from NaiveDate and NaiveTime via `and_time` easily without `TimeZone` trait,
    // let's construct it manually.
    let mut target_dt = tz.from_local_datetime(&now.date_naive().and_time(target_time)).unwrap();
    
    if target_dt <= now {
        target_dt = target_dt + chrono::Days::new(1);
    }
    
    let diff = target_dt.signed_duration_since(now);
    Duration::from_secs(diff.num_seconds().max(0) as u64)
}
