use serde::Serialize;
use anyhow::Result;
use crate::state::AppState;

#[derive(Serialize)]
struct WhatsAppPayload {
    phone: String,
    message: String,
}

pub async fn send_whatsapp_alert(
    state: &AppState,
    codename: &str,
    zone_name: &str,
    entry_local: &str,
    exit_local: &str,
    dwell_secs: i64,
    zone_lat: f64,
    zone_lng: f64,
) -> Result<()> {
    let dwell_mins = dwell_secs / 60;
    let dwell_human = if dwell_mins >= 60 {
        format!("{}h {}m", dwell_mins / 60, dwell_mins % 60)
    } else {
        format!("{}m", dwell_mins)
    };

    let message = format!(
        "🚨 Dead-Zone Violation / مخالفة دخول منطقة محظورة\n\
        Vehicle / المركبة: {}\n\
        Zone / المنطقة: {}\n\
        In / الدخول: {}   Out / الخروج: {}\n\
        Dwell / مدة المكوث: {}\n\
        📍 https://maps.google.com/?q={},{}",
        codename, zone_name, entry_local, exit_local, dwell_human, zone_lat, zone_lng
    );

    let payload = WhatsAppPayload {
        phone: state.cfg.whatsapp_zone_group_id.clone(),
        message,
    };

    let url = format!("{}/send/message", state.cfg.whatsapp_service_url);
    
    let resp = state.http.post(&url)
        .json(&payload)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("WhatsApp API returned {}: {}", status, body));
    }

    Ok(())
}
