//! Auth-aware HTTP client. Hands every data request to the upstream, with
//! transparent re-login on session expiry and exponential backoff on
//! transient failures.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, COOKIE, REFERER};
use reqwest::redirect::Policy;
use reqwest::{Method, Response, StatusCode};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::error::AppError;

use super::auth::{login, AuthState, USER_AGENT_VALUE};

/// The auth-aware client every higher layer uses.
pub struct EtitClient {
    cfg: Config,
    http: reqwest::Client,
    auth: Mutex<AuthState>,
}

impl EtitClient {
    pub fn new(cfg: Config) -> anyhow::Result<Arc<Self>> {
        // We manage cookies ourselves (cross-domain), so disable the jar.
        // We follow redirects manually so we can detect login bounces.
        let mut builder = reqwest::Client::builder()
            .cookie_store(false)
            .redirect(Policy::none())
            .timeout(cfg.upstream_timeout)
            .user_agent(USER_AGENT_VALUE)
            .gzip(true)
            .min_tls_version(reqwest::tls::Version::TLS_1_2)
            .max_tls_version(reqwest::tls::Version::TLS_1_2);

        if let Some(proxy_url) = &cfg.upstream_proxy {
            let proxy = reqwest::Proxy::all(proxy_url)
                .context("parsing UPSTREAM_PROXY")?;
            builder = builder.proxy(proxy);
        }

        let http = builder.build().context("building reqwest client")?;

        Ok(Arc::new(Self {
            cfg,
            http,
            auth: Mutex::new(AuthState::default()),
        }))
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn data_url(&self) -> &str {
        &self.cfg.etit_data_url
    }

    pub async fn auth_snapshot(&self) -> AuthState {
        self.auth.lock().await.clone()
    }

    /// Force a login regardless of current state.
    pub async fn force_login(&self) -> Result<u64, AppError> {
        let mut guard = self.auth.lock().await;
        let next_epoch = guard.epoch.wrapping_add(1);
        let mut new_state = login(&self.http, &self.cfg)
            .await
            .map_err(|e| AppError::Login(e.to_string()))?;
        new_state.epoch = next_epoch;
        *guard = new_state;
        tracing::info!(
            sub_id = %guard.sub_id,
            staff_id = %guard.staff_id,
            epoch = guard.epoch,
            "ETIT login established",
        );
        Ok(guard.epoch)
    }

    /// Ensure we have a session; if our snapshot's epoch matches the current
    /// state's, perform a login. If a concurrent caller already advanced the
    /// epoch, we return without re-logging.
    pub async fn ensure_logged_in(&self, observed_epoch: u64) -> Result<(), AppError> {
        let mut guard = self.auth.lock().await;
        if guard.epoch != observed_epoch {
            return Ok(());
        }
        let next_epoch = guard.epoch.wrapping_add(1);
        let mut new_state = login(&self.http, &self.cfg)
            .await
            .map_err(|e| AppError::Login(e.to_string()))?;
        new_state.epoch = next_epoch;
        *guard = new_state;
        tracing::info!(
            sub_id   = %guard.sub_id,
            staff_id = %guard.staff_id,
            epoch    = guard.epoch,
            "ETIT login refreshed",
        );
        Ok(())
    }

    /// Workhorse for all data calls. Builds the request with the current
    /// cookie header, retries on session expiry, and on transient errors
    /// applies exponential backoff up to `upstream_retry_max` extra attempts.
    ///
    /// The closure receives `(&Client, &cookie_header)` and must produce a
    /// `RequestBuilder`. It will be re-invoked across retries with potentially
    /// different cookies, so it must be fresh-rebuildable.
    pub async fn request_data<F>(&self, build: F) -> Result<Response, AppError>
    where
        F: Fn(&reqwest::Client, &str) -> reqwest::RequestBuilder,
    {
        // Single re-login attempt + N retries on transient errors.
        let mut last_err: Option<AppError> = None;

        for attempt in 0..=self.cfg.upstream_retry_max {
            // Snapshot the auth state for this attempt.
            let (epoch, cookie) = {
                let g = self.auth.lock().await;
                (g.epoch, g.cookie_header.clone())
            };

            match build(&self.http, &cookie).send().await {
                Ok(resp) => {
                    if is_session_redirect(&resp) {
                        drop(resp);
                        self.ensure_logged_in(epoch).await?;

                        let cookie2 = self.auth.lock().await.cookie_header.clone();
                        let resp2 = build(&self.http, &cookie2).send().await?;
                        if is_session_redirect(&resp2) {
                            return Err(AppError::SessionDead);
                        }
                        return Ok(resp2);
                    }
                    return Ok(resp);
                }
                Err(e) if is_transient(&e) && attempt < self.cfg.upstream_retry_max => {
                    let backoff = self.backoff_for(attempt);
                    tracing::warn!(
                        attempt, ?backoff, error = %e,
                        "transient upstream error; retrying",
                    );
                    tokio::time::sleep(backoff).await;
                    last_err = Some(e.into());
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }

        Err(last_err.unwrap_or(AppError::SessionDead))
    }

    fn backoff_for(&self, attempt: u32) -> Duration {
        let base = self.cfg.upstream_retry_base.as_millis() as u64;
        let factor = 1u64 << attempt.min(8); // cap doubling
        Duration::from_millis(base.saturating_mul(factor))
    }

    // ---- header builders ---------------------------------------------------

    fn json_headers(&self, cookie_header: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json; charset=UTF-8"));
        h.insert(ACCEPT, HeaderValue::from_static("application/json, text/javascript, */*; q=0.01"));
        h.insert("X-Requested-With", HeaderValue::from_static("XMLHttpRequest"));
        let referer = format!("{}/WebPages/maps.aspx", self.cfg.etit_data_url);
        if let Ok(v) = HeaderValue::from_str(&referer) { h.insert(REFERER, v); }
        if !cookie_header.is_empty() {
            if let Ok(v) = HeaderValue::from_str(cookie_header) { h.insert(COOKIE, v); }
        }
        h
    }

    fn form_headers(&self, cookie_header: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/x-www-form-urlencoded; charset=UTF-8"));
        h.insert(ACCEPT, HeaderValue::from_static("*/*"));
        h.insert("X-Requested-With", HeaderValue::from_static("XMLHttpRequest"));
        let referer = format!("{}/WebPages/maps.aspx", self.cfg.etit_data_url);
        if let Ok(v) = HeaderValue::from_str(&referer) { h.insert(REFERER, v); }
        if !cookie_header.is_empty() {
            if let Ok(v) = HeaderValue::from_str(cookie_header) { h.insert(COOKIE, v); }
        }
        h
    }

    /// Headers for a GET against the **login** host (`etit_login_url`). Plain
    /// HTML browse — no XHR hint, since `UserList.aspx` is a full-postback
    /// WebForms page, not an ajax endpoint.
    fn login_html_headers(&self, cookie_header: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            ACCEPT,
            HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
        );
        if !cookie_header.is_empty() {
            if let Ok(v) = HeaderValue::from_str(cookie_header) {
                h.insert(COOKIE, v);
            }
        }
        h
    }

    /// Headers for a full-postback form POST against the **login** host.
    /// Mirrors a synchronous browser postback: urlencoded body, HTML accept,
    /// `Referer` pointing back at `UserList.aspx`, and deliberately *no*
    /// `X-Requested-With`/`X-MicrosoftAjax` (those would coerce a partial
    /// postback and a delta response).
    fn login_form_headers(&self, cookie_header: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        h.insert(
            ACCEPT,
            HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
        );
        let referer = format!("{}/WebPages/Users/UserList.aspx", self.cfg.etit_login_url);
        if let Ok(v) = HeaderValue::from_str(&referer) {
            h.insert(REFERER, v);
        }
        if !cookie_header.is_empty() {
            if let Ok(v) = HeaderValue::from_str(cookie_header) {
                h.insert(COOKIE, v);
            }
        }
        h
    }

    // ---- convenience verbs -------------------------------------------------

    pub async fn post_json<B: Serialize + Clone + Send + Sync + 'static>(
        &self,
        path: &str,
        body: B,
    ) -> Result<Response, AppError> {
        let url = format!("{}{path}", self.cfg.etit_data_url);
        self.request_data(move |c, cookie| {
            c.request(Method::POST, &url).headers(self.json_headers(cookie)).json(&body)
        })
        .await
    }

    pub async fn post_form(
        &self,
        path: &str,
        form: Vec<(String, String)>,
    ) -> Result<Response, AppError> {
        let url = format!("{}{path}", self.cfg.etit_data_url);
        self.request_data(move |c, cookie| {
            c.request(Method::POST, &url).headers(self.form_headers(cookie)).form(&form)
        })
        .await
    }

    pub async fn get(&self, path_and_query: &str) -> Result<Response, AppError> {
        let url = format!("{}{path_and_query}", self.cfg.etit_data_url);
        self.request_data(move |c, cookie| {
            c.request(Method::GET, &url).headers(self.json_headers(cookie))
        })
        .await
    }

    /// GET against the **login** host (`etit_login_url`). Used for legacy
    /// WebForms admin pages such as `UserList.aspx`. Routes through
    /// [`Self::request_data`], so a session bounce re-logs and retries.
    pub async fn get_login(&self, path_and_query: &str) -> Result<Response, AppError> {
        let url = format!("{}{path_and_query}", self.cfg.etit_login_url);
        self.request_data(move |c, cookie| {
            c.request(Method::GET, &url).headers(self.login_html_headers(cookie))
        })
        .await
    }

    /// Full-postback form POST against the **login** host. The caller is
    /// responsible for supplying a self-consistent field set (scraped
    /// `__VIEWSTATE` &co. plus the `__EVENTTARGET` to fire).
    pub async fn post_form_login(
        &self,
        path: &str,
        form: Vec<(String, String)>,
    ) -> Result<Response, AppError> {
        let url = format!("{}{path}", self.cfg.etit_login_url);
        self.request_data(move |c, cookie| {
            c.request(Method::POST, &url).headers(self.login_form_headers(cookie)).form(&form)
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

fn is_session_redirect(resp: &Response) -> bool {
    let s = resp.status();
    if s != StatusCode::FOUND
        && s != StatusCode::MOVED_PERMANENTLY
        && s != StatusCode::SEE_OTHER
        && s != StatusCode::TEMPORARY_REDIRECT
    {
        return false;
    }
    match resp.headers().get(reqwest::header::LOCATION) {
        Some(loc) => {
            let s = loc.to_str().unwrap_or("").to_ascii_lowercase();
            s.contains("etitlogin.aspx") || s.contains("login.aspx") || s.contains("/login")
        }
        None => true,
    }
}

fn is_transient(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout() || e.is_request()
}
