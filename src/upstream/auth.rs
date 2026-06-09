//! Login flow.
//!
//! ETIT is two ASP.NET hosts behind one logical app:
//!
//! - `etit-fms.etit-eg.com`  — login, identity, SSOCookie issuance
//! - `fms-gps.etit-eg.com`   — data
//!
//! reqwest's cookie jar is domain-scoped, so cookies set by the login host
//! are never replayed against the data host. We work around this by reading
//! `Set-Cookie` headers manually, capturing `ASP.NET_SessionId` + `SSOCookie`,
//! and synthesizing a single `Cookie:` header that we inject on every data
//! request.
//!
//! The login flow itself is three steps:
//!
//! 1. `GET  /ETITlogin.aspx` to scrape `__VIEWSTATE` / `__VIEWSTATEGENERATOR`.
//! 2. `POST /ETITlogin.aspx` with credentials (ASP.NET partial-postback).
//!    The response body is a ScriptManager pipe-delimited blob containing
//!    a `pageRedirect` directive — *not* a real HTTP redirect.
//! 3. `GET <pageRedirect target>` to provoke `Set-Cookie: SSOCookie=...`.
//!
//! The login is mutually exclusive with itself via a `tokio::Mutex` keyed
//! by an epoch counter, so concurrent expiring requests trigger a single
//! re-login.

use anyhow::{anyhow, Context};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, REFERER, USER_AGENT};

use crate::config::Config;

pub const USER_AGENT_VALUE: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36 etit-proxy/0.2";

// Regex pairs cope with attribute order variation in ASP.NET's emitted HTML.
static VIEWSTATE_RE_FWD: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"id="__VIEWSTATE"[^>]*value="([^"]*)""#).unwrap());
static VIEWSTATE_RE_REV: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"value="([^"]*)"[^>]*id="__VIEWSTATE""#).unwrap());

static GENERATOR_RE_FWD: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"id="__VIEWSTATEGENERATOR"[^>]*value="([^"]*)""#).unwrap());
static GENERATOR_RE_REV: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"value="([^"]*)"[^>]*id="__VIEWSTATEGENERATOR""#).unwrap());

static SUBID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)subid\s*[=:|]\s*([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})")
        .unwrap()
});
static STAFFID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)staffid\s*[=:|]\s*([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})")
        .unwrap()
});
static GUID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap()
});

/// Snapshot of upstream session state. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct AuthState {
    pub sub_id: String,
    pub staff_id: String,
    /// Bumped every successful login. Used to gate concurrent re-logins.
    pub epoch: u64,
    /// Pre-built `Cookie:` header value, ready to inject into data calls.
    pub cookie_header: String,
    /// When this state was established. `None` ⇒ never logged in.
    pub last_login_at: Option<crate::domain::Timestamp>,
}

impl AuthState {
    pub fn is_ready(&self) -> bool {
        !self.cookie_header.is_empty() && !self.sub_id.is_empty()
    }
}

/// Run the three-step login flow, returning a fresh [`AuthState`] (with
/// `epoch = 0`; the caller increments it).
pub async fn login(http: &reqwest::Client, cfg: &Config) -> anyhow::Result<AuthState> {
    let login_url = format!("{}/ETITlogin.aspx", cfg.etit_login_url);

    // ---- Step 1: GET login page ---------------------------------------------
    let get_resp = http
        .get(&login_url)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "text/html,application/xhtml+xml")
        .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .send()
        .await
        .context("GET ETITlogin.aspx")?;

    if !get_resp.status().is_success() {
        return Err(anyhow!("login page returned {}", get_resp.status()));
    }

    let get_headers = get_resp.headers().clone();
    let html = get_resp.text().await.context("read login page body")?;

    let viewstate = scrape(&html, &VIEWSTATE_RE_FWD, &VIEWSTATE_RE_REV)
        .ok_or_else(|| anyhow!("could not scrape __VIEWSTATE"))?;
    let generator = scrape(&html, &GENERATOR_RE_FWD, &GENERATOR_RE_REV)
        .ok_or_else(|| anyhow!("could not scrape __VIEWSTATEGENERATOR"))?;

    // ---- Step 2: POST credentials ------------------------------------------
    let form: Vec<(&str, &str)> = vec![
        ("ScriptManager1",        "UpdatePanel1|lg_AltairLogin$LoginButton"),
        ("__EVENTTARGET",         "lg_AltairLogin$LoginButton"),
        ("__VIEWSTATE",           &viewstate),
        ("__VIEWSTATEGENERATOR",  &generator),
        ("lg_AltairLogin$UserName", &cfg.etit_username),
        ("lg_AltairLogin$Password", &cfg.etit_password),
        ("__ASYNCPOST",           "true"),
        ("RadAJAXControlID",      "RadAjaxManager1"),
    ];

    let post_resp = http
        .post(&login_url)
        .header(REFERER, &login_url)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded; charset=UTF-8")
        .header("X-MicrosoftAjax", "Delta=true")
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&form)
        .send()
        .await
        .context("POST ETITlogin.aspx")?;

    let post_status        = post_resp.status();
    let post_headers       = post_resp.headers().clone();
    let post_body          = post_resp.text().await.unwrap_or_default();

    if !post_status.is_success() && !post_status.is_redirection() {
        return Err(anyhow!(
            "login POST returned {}: {}",
            post_status,
            truncate(&post_body, 400)
        ));
    }

    // ---- Step 3: follow ScriptManager pageRedirect -------------------------
    let redirect_path = extract_page_redirect(&post_body);
    let target = redirect_path
        .as_deref()
        .unwrap_or("WebPages/Transporters/List.aspx")
        .trim_start_matches('/');
    let follow_url = format!("{}/{}", cfg.etit_login_url, target);

    tracing::debug!(url = %follow_url, "following pageRedirect after login POST");

    let redirect_resp = http
        .get(&follow_url)
        .header(REFERER, &login_url)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "text/html,application/xhtml+xml,*/*")
        .send()
        .await
        .context("GET post-login redirect")?;

    let redirect_headers = redirect_resp.headers().clone();
    let sso_cookie = extract_set_cookie(&redirect_headers, "SSOCookie")
        .or_else(|| extract_set_cookie(&post_headers, "SSOCookie"))
        .unwrap_or_default();

    if sso_cookie.is_empty() {
        let seen_cookies: Vec<String> = redirect_resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).to_string())
            .collect();
        return Err(anyhow!(
            "SSOCookie missing. Cookies on redirect GET: {seen_cookies:?}. \
             Post body: {}",
            truncate(&post_body, 200)
        ));
    }

    // Collect all cookies from all steps.
    let mut all_cookies = std::collections::HashMap::new();
    
    // Helper to merge cookies from a HeaderMap.
    let mut merge = |h: &reqwest::header::HeaderMap| {
        for val in h.get_all(reqwest::header::SET_COOKIE).iter() {
            if let Ok(s) = val.to_str() {
                if let Some(pair) = s.split(';').next() {
                    let mut parts = pair.splitn(2, '=');
                    if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                        all_cookies.insert(k.trim().to_string(), v.trim().to_string());
                    }
                }
            }
        }
    };

    merge(&get_headers);
    merge(&post_headers);
    merge(&redirect_headers);

    let cookie_header = all_cookies
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");

    let (sub_id, staff_id) = parse_sso_cookie(&sso_cookie).ok_or_else(|| {
        anyhow!(
            "could not extract subID/staffID from SSOCookie: {}",
            truncate(&sso_cookie, 200)
        )
    })?;

    Ok(AuthState {
        sub_id,
        staff_id,
        epoch: 0,
        cookie_header,
        last_login_at: Some(crate::domain::utc_now()),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn extract_set_cookie(
    headers: &reqwest::header::HeaderMap,
    name: &str,
) -> Option<String> {
    let prefix = format!("{name}=");
    for val in headers.get_all(reqwest::header::SET_COOKIE).iter() {
        let s = val.to_str().unwrap_or("");
        let pair = s.split(';').next().unwrap_or("").trim();
        if let Some(value) = pair.strip_prefix(&prefix) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Pull the path out of an ASP.NET partial-postback `pageRedirect` directive.
/// Format: `1|#||4|38|pageRedirect||%2fWebPages%2fSomePage.aspx|`.
pub(crate) fn extract_page_redirect(body: &str) -> Option<String> {
    let idx = body.find("pageRedirect")?;
    let after = &body[idx..];
    let mut parts = after.splitn(5, '|');
    parts.next()?; // "pageRedirect"
    parts.next()?; // ""
    let encoded = parts.next()?.trim();
    if encoded.is_empty() {
        return None;
    }
    Some(
        percent_encoding::percent_decode_str(encoded)
            .decode_utf8_lossy()
            .into_owned(),
    )
}

pub(crate) fn scrape(html: &str, primary: &Regex, fallback: &Regex) -> Option<String> {
    primary
        .captures(html)
        .or_else(|| fallback.captures(html))
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

pub(crate) fn parse_sso_cookie(raw: &str) -> Option<(String, String)> {
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8_lossy()
        .into_owned();

    // The SSOCookie is a *-delimited string.
    // Based on empirical observation:
    // index 1: subID
    // index 12: staffID
    let parts: Vec<&str> = decoded.split('*').collect();
    if parts.len() > 12 {
        let sub = parts[1].to_string();
        let staff = parts[12].to_string();
        // Basic sanity check: they should look like GUIDs.
        if GUID_RE.is_match(&sub) && GUID_RE.is_match(&staff) {
            return Some((sub, staff));
        }
    }

    // Fallback: search for subid=... or staffid=...
    let sub = SUBID_RE.captures(&decoded).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
    let staff = STAFFID_RE.captures(&decoded).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
    if let (Some(s), Some(t)) = (sub.clone(), staff.clone()) {
        return Some((s, t));
    }

    // Fallback: positional, deduped.
    let mut distinct: Vec<String> = Vec::new();
    for m in GUID_RE.find_iter(&decoded) {
        let g = m.as_str().to_string();
        if !distinct.contains(&g) {
            distinct.push(g);
        }
    }
    match (sub, staff, distinct.as_slice()) {
        (Some(s), None, [_, second, ..])           => Some((s, second.clone())),
        (None,    Some(t), [first, ..])            => Some((first.clone(), t)),
        (None,    None, [first, second, ..])       => Some((first.clone(), second.clone())),
        _                                          => None,
    }
}

pub(crate) fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}…", &s[..n]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_page_redirect_path() {
        let body = "1|#||4|38|pageRedirect||%2fWebPages%2fTransporters%2fList.aspx|";
        assert_eq!(
            extract_page_redirect(body).as_deref(),
            Some("/WebPages/Transporters/List.aspx"),
        );
    }

    #[test]
    fn parses_named_sso_cookie() {
        let raw = "userid=abc&subID=ca53d5de-cf14-ea11-80ee-0025b500010d&\
                   staffID=11111111-2222-3333-4444-555555555555&exp=2026";
        let (s, t) = parse_sso_cookie(raw).unwrap();
        assert_eq!(s, "ca53d5de-cf14-ea11-80ee-0025b500010d");
        assert_eq!(t, "11111111-2222-3333-4444-555555555555");
    }

    #[test]
    fn parses_positional_sso_cookie() {
        let raw = "ca53d5de-cf14-ea11-80ee-0025b500010d|\
                   11111111-2222-3333-4444-555555555555|extra";
        let (s, t) = parse_sso_cookie(raw).unwrap();
        assert_eq!(s, "ca53d5de-cf14-ea11-80ee-0025b500010d");
        assert_eq!(t, "11111111-2222-3333-4444-555555555555");
    }

    #[test]
    fn scrapes_either_attr_order() {
        let a = r#"<input id="__VIEWSTATE" value="ABC" />"#;
        let b = r#"<input value="ABC" id="__VIEWSTATE" />"#;
        assert_eq!(scrape(a, &VIEWSTATE_RE_FWD, &VIEWSTATE_RE_REV).as_deref(), Some("ABC"));
        assert_eq!(scrape(b, &VIEWSTATE_RE_FWD, &VIEWSTATE_RE_REV).as_deref(), Some("ABC"));
    }
}
