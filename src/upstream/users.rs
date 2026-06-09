//! Integration — unblock a user via the legacy `UserList.aspx` admin grid.
//!
//! Unlike the data integrations, this one targets the **login host**
//! (`etit_login_url`, the `etit-fms` ASP.NET app) rather than the GPS data
//! host. `UserList.aspx` is a classic full-postback WebForms page: there is
//! no JSON endpoint. The "Unblock User" control is an `<a>` that calls
//! `__doPostBack('<UniqueID>', '')`, so we reproduce that postback by:
//!
//! 1. `GET /WebPages/Users/UserList.aspx` and scraping *every* hidden input
//!    (`__VIEWSTATE`, `__VIEWSTATEGENERATOR`, `__PREVIOUSPAGE`, the `hdn_*`
//!    auth mirrors, and the Telerik `*_ClientState` fields).
//! 2. Re-posting all of them back, with `__EVENTTARGET` overridden to the
//!    button's `UniqueID` and `__EVENTARGUMENT` left empty.
//!
//! The page carries **no** `__EVENTVALIDATION` field (event validation is
//! disabled), which is what makes a synthesized postback viable at all. The
//! GET must happen under a live session because `__VIEWSTATE` is session-
//! bound; posting a stale viewstate under a fresh session is rejected. We
//! route both calls through [`EtitClient`], so a session bounce on the GET is
//! transparently re-logged and retried before we ever capture viewstate.
//!
//! ## Hardcoded target
//!
//! Per requirements this unblocks the **first data row** of the grid, whose
//! Telerik item index is `ctl04`. On the reference subscriber that row is
//! `magda.adly`. The index is *not* derived from the row contents — if the
//! grid's row order or structure changes upstream, [`UNBLOCK_TARGET`] must be
//! updated. The acting (logged-in) account must also hold Full Control on the
//! `UserList` feature, or the server will silently ignore the event.

use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::header::LOCATION;

use crate::error::AppError;

use super::client::EtitClient;
use super::parsers::truncate_for_log;

/// Users grid page. Lives on the **login** host (`etit_login_url`).
const USER_LIST_PATH: &str = "/WebPages/Users/UserList.aspx";

/// `__EVENTTARGET` for the "Unblock User" button on the **first** grid row
/// (Telerik item `ctl04`). On the reference subscriber this is `magda.adly`.
/// Hardcoded by design; see module docs for the caveat. Note the `$`
/// separators: this is the control's `UniqueID`, not its element `id`.
const UNBLOCK_TARGET: &str =
    "ctl00$ContentPlaceHolder1$grd_UsersList$ctl00$ctl04$btn_UnBlockUser";

/// Human label for the row above, surfaced in responses/logs only.
pub const UNBLOCK_TARGET_LABEL: &str = "magda.adly";

/// Best-effort, structured result of an unblock postback.
///
/// ASP.NET returns `200` with a re-rendered page whether or not the action
/// was authorized, and the grid markup exposes **no per-row blocked flag** —
/// so a clean grid re-render does *not* prove the unblock applied. What we
/// *can* detect is an outright rejection: an error/exception page, an
/// access-denied bounce, or an injected RadWindow/`alert` message. Those
/// signals are surfaced here so the caller can tell "accepted (probably
/// applied)" from "server refused it".
#[derive(Debug, Clone)]
pub struct UnblockOutcome {
    /// HTTP status of the postback response.
    pub http_status: u16,
    /// Body looked like the normal users grid (grid id + a fresh viewstate).
    pub is_users_grid: bool,
    /// Body looked like an ASP.NET error / unhandled-exception page.
    pub error_page: bool,
    /// Access-denied / unauthorized markers were found (body or redirect).
    pub access_denied: bool,
    /// Any extracted alert or error message from the response.
    pub upstream_message: Option<String>,
    /// First slice of the body when it was *not* a clean grid re-render,
    /// for eyeballing what came back instead.
    pub body_excerpt: Option<String>,
}

impl UnblockOutcome {
    /// True when the postback came back as a normal grid re-render with no
    /// rejection signals. Note the caveat in the type docs: this is "the
    /// server accepted the event", not a proof the user is now unblocked.
    pub fn accepted(&self) -> bool {
        self.is_users_grid && !self.error_page && !self.access_denied
    }

    /// One-line interpretation for logs / API `note`.
    pub fn note(&self) -> String {
        if self.access_denied {
            "server refused the action (access-denied / unauthorized markers \
             in response) — the acting account lacks rights on UserList"
                .into()
        } else if self.error_page {
            "upstream returned an error/exception page — postback rejected or \
             faulted (see upstreamMessage / bodyExcerpt)"
                .into()
        } else if self.is_users_grid {
            "users grid re-rendered with no rejection signals; event accepted. \
             NOTE: grid markup has no blocked flag, so this does not by itself \
             confirm the user is now unblocked"
                .into()
        } else {
            "unexpected response (not the users grid, no known error markers); \
             inspect bodyExcerpt"
                .into()
        }
    }
}

// One `<input ...>` tag, lazily up to the first `>`. ASP.NET self-closes its
// hidden inputs, so a greedy-to-`>` match is safe.
static INPUT_TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<input\b[^>]*>"#).unwrap());
static ATTR_NAME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)\bname\s*=\s*"([^"]*)""#).unwrap());
static ATTR_VALUE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)\bvalue\s*=\s*"([^"]*)""#).unwrap());
static ATTR_TYPE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)\btype\s*=\s*"([^"]*)""#).unwrap());

// ASP.NET unhandled-exception / generic error page markers.
static ERROR_PAGE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(server error in .* application|runtime error|unhandled exception|exception details:|stack trace:)")
        .unwrap()
});
// Access-denied / authorization-failure phrasing (covers ETIT + framework).
static DENIED_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(access is denied|access denied|not authori[sz]ed|unauthori[sz]ed|do(?:es)?n.?t have permission|you do not have permission|insufficient (?:rights|permission)|accessdenied)")
        .unwrap()
});
// An injected client-side message: radalert('..'), radalert(".."), or a
// classic alert('..') emitted into the postback response.
static ALERT_MSG_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?is)(?:radalert|alert)\s*\(\s*['"](.*?)['"]"#).unwrap()
});
// `<title>..</title>`, used to caption an error page when present.
static TITLE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)<title>\s*(.*?)\s*</title>").unwrap());

/// Unblock the hardcoded first-row user (`magda.adly`). Idempotent upstream:
/// unblocking an already-unblocked user is a no-op. Returns a structured
/// [`UnblockOutcome`] with best-effort rejection detection rather than a bare
/// `()`, so callers can distinguish "accepted" from "server refused".
pub async fn unblock_first_row(client: &EtitClient) -> Result<UnblockOutcome, AppError> {
    // Ensure a live session before we scrape viewstate; a stale viewstate
    // posted under a fresh session would be rejected by ASP.NET.
    if !client.auth_snapshot().await.is_ready() {
        client.force_login().await?;
    }

    // ---- Step 1: GET the grid page, scrape hidden fields -------------------
    let get_resp = client.get_login(USER_LIST_PATH).await?;
    let status = get_resp.status();
    if !status.is_success() {
        let body = get_resp.text().await.unwrap_or_default();
        return Err(AppError::UpstreamStatus {
            status: status.as_u16(),
            body: truncate_for_log(&body, 400),
        });
    }
    let html = get_resp
        .text()
        .await
        .map_err(|e| AppError::Parse(format!("UserList GET read: {e}")))?;

    let mut form = scrape_hidden_inputs(&html);
    if !form.iter().any(|(k, _)| k == "__VIEWSTATE") {
        return Err(AppError::Parse(
            "UserList page had no __VIEWSTATE; not the expected grid \
             (session bounce or upstream markup change)"
                .into(),
        ));
    }

    // ---- Step 2: drive the postback ----------------------------------------
    set_field(&mut form, "__EVENTTARGET", UNBLOCK_TARGET);
    set_field(&mut form, "__EVENTARGUMENT", "");

    let post_resp = client.post_form_login(USER_LIST_PATH, form).await?;
    let http_status = post_resp.status().as_u16();

    // A redirect to the *login* page would already have been turned into a
    // re-login + retry by the client. Any redirect that survives to here is a
    // genuine app redirect — capture its target as a denial signal.
    let redirect_location = post_resp
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let body = post_resp.text().await.unwrap_or_default();
    let outcome = analyze_response(http_status, redirect_location.as_deref(), &body);

    // Log at a level that matches severity, so a refused unblock is visible in
    // ops logs without scraping the API response.
    if outcome.accepted() {
        tracing::info!(
            target = UNBLOCK_TARGET,
            label = UNBLOCK_TARGET_LABEL,
            status = outcome.http_status,
            "unblock postback accepted (block-state not confirmable from markup)"
        );
    } else {
        tracing::warn!(
            target = UNBLOCK_TARGET,
            label = UNBLOCK_TARGET_LABEL,
            status = outcome.http_status,
            access_denied = outcome.access_denied,
            error_page = outcome.error_page,
            is_users_grid = outcome.is_users_grid,
            message = outcome.upstream_message.as_deref().unwrap_or(""),
            "unblock postback NOT accepted — see message/excerpt"
        );
    }

    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Classify a postback response into an [`UnblockOutcome`]. Pure function of
/// (status, redirect target, body) so it's unit-testable without a network.
fn analyze_response(status: u16, redirect_location: Option<&str>, body: &str) -> UnblockOutcome {
    let is_users_grid = body.contains("grd_UsersList") && body.contains("__VIEWSTATE");
    let error_page = ERROR_PAGE_RE.is_match(body);

    // Denial can show up in the body, or in a redirect to an access-denied /
    // login page (the latter only survives here if it wasn't the SSO login
    // bounce the client already handles).
    let denied_in_body = DENIED_RE.is_match(body);
    let denied_in_redirect = redirect_location
        .map(|loc| DENIED_RE.is_match(loc) || loc.to_ascii_lowercase().contains("login"))
        .unwrap_or(false);
    let access_denied = denied_in_body || denied_in_redirect;

    // Prefer the most specific message we can find: an injected alert, then a
    // denial phrase in context, then an error-page title, then the redirect.
    let upstream_message = extract_alert(body)
        .or_else(|| {
            DENIED_RE.find(body).map(|m| {
                // Grab a little surrounding context for the matched phrase.
                let start = m.start().saturating_sub(40);
                let end = (m.end() + 40).min(body.len());
                collapse_ws(&body[start..end])
            })
        })
        .or_else(|| {
            if error_page {
                TITLE_RE
                    .captures(body)
                    .and_then(|c| c.get(1))
                    .map(|m| collapse_ws(m.as_str()))
            } else {
                None
            }
        })
        .or_else(|| redirect_location.map(|l| format!("redirected to {l}")));

    // Only attach an excerpt when the response wasn't a clean grid render —
    // otherwise it'd just be a megabyte of viewstate.
    let body_excerpt = if is_users_grid && !error_page && !access_denied {
        None
    } else if !body.is_empty() {
        Some(truncate_for_log(&collapse_ws(body), 600))
    } else {
        redirect_location.map(|l| format!("(empty body; Location: {l})"))
    };

    UnblockOutcome {
        http_status: status,
        is_users_grid,
        error_page,
        access_denied,
        upstream_message,
        body_excerpt,
    }
}

/// Extract a client-side alert message (`radalert('…')` / `alert("…")`) from a
/// postback response, skipping the empty `{1}` placeholders that appear in the
/// page's *static* RadWindow templates.
fn extract_alert(body: &str) -> Option<String> {
    ALERT_MSG_RE
        .captures_iter(body)
        .map(|c| c[1].to_string())
        .map(|m| collapse_ws(&m))
        .find(|m| !m.is_empty() && m != "{1}" && m != "{0}")
}

/// Collapse runs of whitespace to single spaces and trim. Keeps log/JSON
/// messages from carrying raw markup newlines.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pull every hidden `<input>`'s `(name, value)` out of an ASP.NET page.
/// Keeps inputs typed `hidden`, plus the WebForms/Telerik control fields
/// whose names start with `__` or `ctl00` (some omit the `type` attribute).
/// Clickable controls (`submit`/`button`/`image`/`reset`) are dropped: a
/// genuine `__doPostBack` request body omits every button except the one
/// activated, and we drive the event via `__EVENTTARGET` instead — replaying
/// an unclicked submit risks firing its server-side handler too. Inputs
/// without a `value` attribute (e.g. empty Telerik `*_ClientState`) yield an
/// empty string.
fn scrape_hidden_inputs(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for tag in INPUT_TAG_RE.find_iter(html) {
        let tag = tag.as_str();

        let name = match ATTR_NAME_RE.captures(tag) {
            Some(c) => c[1].to_string(),
            None => continue,
        };

        let typ = ATTR_TYPE_RE
            .captures(tag)
            .map(|c| c[1].to_ascii_lowercase())
            .unwrap_or_default();

        if matches!(typ.as_str(), "submit" | "button" | "image" | "reset") {
            continue;
        }

        let is_hidden = typ == "hidden";
        if !is_hidden && !name.starts_with("__") && !name.starts_with("ctl00") {
            continue;
        }

        let value = ATTR_VALUE_RE
            .captures(tag)
            .map(|c| decode_entities(&c[1]))
            .unwrap_or_default();

        out.push((name, value));
    }
    out
}

/// Set (or append) a form field, replacing any existing occurrence.
fn set_field(form: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some(slot) = form.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value.to_string();
    } else {
        form.push((key.to_string(), value.to_string()));
    }
}

/// Minimal HTML-entity decode for scraped attribute values. ASP.NET HTML-
/// encodes `&` (→ `&amp;`) inside hidden-field values; `reqwest`'s form
/// encoder re-encodes on the way out, so we must hand it the *decoded* value
/// to avoid double-encoding.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrapes_hidden_inputs_varied_attr_order_and_missing_value() {
        let html = r#"
            <input type="hidden" name="__VIEWSTATE" id="__VIEWSTATE" value="/wEPaA&amp;b" />
            <input type="hidden" name="__VIEWSTATEGENERATOR" value="382089F7" />
            <input name="ctl00_grd_ClientState" id="x" type="hidden" />
            <input type="text" name="visible_text" value="ignore me" />
            <input type="submit" name="ctl00$Button1" value="Search" />
        "#;
        let fields = scrape_hidden_inputs(html);

        // __VIEWSTATE decoded, generator captured, empty clientstate kept.
        assert_eq!(
            fields.iter().find(|(k, _)| k == "__VIEWSTATE").map(|(_, v)| v.as_str()),
            Some("/wEPaA&b"),
        );
        assert!(fields.iter().any(|(k, v)| k == "__VIEWSTATEGENERATOR" && v == "382089F7"));
        assert!(fields.iter().any(|(k, v)| k == "ctl00_grd_ClientState" && v.is_empty()));
        // A plain visible text input with no __/ctl00 name is dropped...
        assert!(!fields.iter().any(|(k, _)| k == "visible_text"));
        // ...and so is the unclicked Search submit button (would otherwise
        // fire its server-side Click handler).
        assert!(!fields.iter().any(|(k, _)| k == "ctl00$Button1"));
    }

    #[test]
    fn set_field_replaces_then_appends() {
        let mut form = vec![("__EVENTTARGET".into(), "".into())];
        set_field(&mut form, "__EVENTTARGET", UNBLOCK_TARGET);
        set_field(&mut form, "__EVENTARGUMENT", "");
        assert_eq!(form.iter().filter(|(k, _)| k == "__EVENTTARGET").count(), 1);
        assert_eq!(form[0].1, UNBLOCK_TARGET);
        assert!(form.iter().any(|(k, v)| k == "__EVENTARGUMENT" && v.is_empty()));
    }

    #[test]
    fn clean_grid_render_is_accepted() {
        let body = r#"<html><input type="hidden" name="__VIEWSTATE" value="x"/>
            <div id="ctl00_ContentPlaceHolder1_grd_UsersList">...rows...</div></html>"#;
        let o = analyze_response(200, None, body);
        assert!(o.is_users_grid);
        assert!(!o.error_page && !o.access_denied);
        assert!(o.accepted());
        assert!(o.body_excerpt.is_none()); // no megabyte of viewstate echoed back
    }

    #[test]
    fn access_denied_in_body_is_flagged() {
        let body = "<html><body>Access is denied. You do not have permission to \
                    perform this action.</body></html>";
        let o = analyze_response(200, None, body);
        assert!(o.access_denied);
        assert!(!o.accepted());
        assert!(o.upstream_message.is_some());
        assert!(o.body_excerpt.is_some());
    }

    #[test]
    fn denied_redirect_is_flagged() {
        let o = analyze_response(302, Some("/WebPages/AccessDenied.aspx"), "");
        assert!(o.access_denied);
        assert!(!o.accepted());
        assert_eq!(o.http_status, 302);
        assert!(o.upstream_message.is_some());
    }

    #[test]
    fn error_page_uses_title_as_message() {
        let body = "<html><head><title>Runtime Error</title></head><body>\
                    Server Error in '/' Application. Exception details: ...</body></html>";
        let o = analyze_response(500, None, body);
        assert!(o.error_page);
        assert!(!o.accepted());
        assert_eq!(o.upstream_message.as_deref(), Some("Runtime Error"));
    }

    #[test]
    fn radalert_message_extracted() {
        let body = r#"<html><input type="hidden" name="__VIEWSTATE" value="x"/>
            <div id="grd_UsersList"></div>
            <script>radalert('You are not authorized to unblock users.', 330, 180);</script>
            </html>"#;
        let o = analyze_response(200, None, body);
        // The injected alert is surfaced; the denial phrasing also trips the flag.
        assert_eq!(
            o.upstream_message.as_deref(),
            Some("You are not authorized to unblock users.")
        );
        assert!(o.access_denied);
        assert!(!o.accepted());
    }

    #[test]
    fn static_alert_placeholder_is_ignored() {
        // The page's static template contains radalert-less {1} placeholders;
        // ensure a bare alert('{1}') style placeholder isn't surfaced.
        let body = r#"<div id="grd_UsersList"></div>
            <input type="hidden" name="__VIEWSTATE" value="x"/>
            <script>function f(){ alert('{1}'); }</script>"#;
        let o = analyze_response(200, None, body);
        assert!(o.accepted());
        assert!(o.upstream_message.is_none());
    }
}
